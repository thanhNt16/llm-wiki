"""Loopback visualizer server with live reload (SP2b #5).

Serves the prebuilt wiki-visualize UI on 127.0.0.1 only, regenerates
graph-data.json when the store changes, and pushes `event: reload` to
connected browsers over SSE — mirroring openwiki's server.ts debounce
pattern. Read-only against the wiki: regeneration uses graphdata, never a
Transaction.
"""
import json
import os
import threading
import time
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from . import graphdata, vizexport
from .transaction import TxnError

_POLL_S = 0.5
_QUIET_S = 0.3  # debounce: push after the store stops changing


def _store_signature(wiki) -> tuple:
    """Cheap change signal: (path, mtime_ns, size) for every canonical file."""
    sig = []
    for sub in ("claims", "decisions", "sources"):
        d = wiki.p(sub)
        if not os.path.isdir(d):
            continue
        for r, _, fs in os.walk(d):
            for f in sorted(fs):
                p = os.path.join(r, f)
                try:
                    st = os.stat(p)
                    sig.append((p, st.st_mtime_ns, st.st_size))
                except OSError:
                    pass
    for f in (wiki.p("wiki.json"),):
        if os.path.isfile(f):
            st = os.stat(f)
            sig.append((f, st.st_mtime_ns, st.st_size))
    return tuple(sig)


class _State:
    def __init__(self, wiki, ui_dir):
        self.wiki = wiki
        self.ui_dir = ui_dir
        self.lock = threading.Lock()
        self.payload_json = None
        self.clients = []  # writable response streams

    def regenerate(self):
        payload = graphdata.graph_payload(self.wiki, full=True)
        data = json.dumps(payload, ensure_ascii=False)
        with self.lock:
            self.payload_json = data
        return data

    def broadcast_reload(self, data):
        msg = ("event: reload\ndata: %s\n\n" % data).encode()
        with self.lock:
            dead = []
            for w in self.clients:
                try:
                    w.write(msg)
                    w.flush()
                except Exception:
                    dead.append(w)
            for w in dead:
                self.clients.remove(w)


def _watch_loop(state):
    last = _store_signature(state.wiki)
    while True:
        time.sleep(_POLL_S)
        try:
            cur = _store_signature(state.wiki)
        except Exception:
            continue
        if cur == last:
            continue
        # debounce: wait for the writer to go quiet, then re-check
        time.sleep(_QUIET_S)
        if _store_signature(state.wiki) != cur:
            continue
        last = cur
        try:
            data = state.regenerate()
        except Exception:
            continue  # transient mid-write state; next poll retries
        state.broadcast_reload(data)


def _make_handler(state):
    ui = state.ui_dir

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *a):
            pass

        def _send(self, code, ctype, body, head_only=False):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            if not head_only:
                self.wfile.write(body)

        def do_HEAD(self):
            self.do_GET(head_only=True)

        def do_GET(self, head_only=False):
            path = self.path.split("?", 1)[0]
            if path == "/events":
                self._events()
                return
            if path in ("/", "/index.html"):
                fp = os.path.join(ui, "index.html")
            elif path == "/graph-data.json":
                # always live: the template's shipped copy is stale
                if state.payload_json is None:
                    state.regenerate()
                self._send(200, "application/json",
                           state.payload_json.encode(), head_only)
                return
            else:
                fp = os.path.normpath(os.path.join(ui, path.lstrip("/")))
                if not fp.startswith(os.path.abspath(ui) + os.sep):
                    self._send(403, "text/plain", b"forbidden", head_only)
                    return
            if not os.path.isfile(fp):
                self._send(404, "text/plain", b"not found", head_only)
                return
            ext = os.path.splitext(fp)[1]
            ctype = {".html": "text/html; charset=utf-8",
                     ".js": "text/javascript",
                     ".css": "text/css", ".json": "application/json",
                     ".svg": "image/svg+xml", ".png": "image/png",
                     ".ico": "image/x-icon"}.get(ext, "application/octet-stream")
            with open(fp, "rb") as f:
                self._send(200, ctype, f.read(), head_only)

        def _events(self):
            if state.payload_json is None:
                state.regenerate()
            # hello stays small: node count only, never the whole payload
            nodes = len(json.loads(state.payload_json).get("nodes", []))
            hello = ("retry: 1000\n\n"
                     "event: hello\ndata: {\"nodes\": %d}\n\n" % nodes).encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                self.wfile.write(hello)
                self.wfile.flush()
            except Exception:
                return
            with state.lock:
                state.clients.append(self.wfile)
            try:
                while True:
                    # SSE comments keep half-dead connections detectable
                    self.wfile.write(b": ping\n\n")
                    self.wfile.flush()
                    time.sleep(5)
            except Exception:
                pass
            finally:
                with state.lock:
                    if self.wfile in state.clients:
                        state.clients.remove(self.wfile)

    return Handler


def serve(wiki, port=4333, ui_dist_dir=None, no_open=False,
          open_browser=None, run_forever=True):
    """Start the loopback server. Returns (server, url)."""
    ui_dir = vizexport.ui_dist(ui_dist_dir)
    state = _State(wiki, ui_dir)
    state.regenerate()
    handler = _make_handler(state)
    last_err = None
    for cand in [port + i for i in range(21)]:
        try:
            srv = ThreadingHTTPServer(("127.0.0.1", cand), handler)
            break
        except OSError as e:
            last_err = e
    else:
        raise TxnError("no free loopback port near %d: %s" % (port, last_err))
    url = "http://127.0.0.1:%d/" % srv.server_address[1]
    threading.Thread(target=_watch_loop, args=(state,), daemon=True).start()
    if run_forever:
        print(json.dumps({"serving": url, "ui": ui_dir}), flush=True)
        if not no_open:
            (open_browser or webbrowser.open)(url)
        try:
            srv.serve_forever()
        except KeyboardInterrupt:
            pass
        finally:
            srv.server_close()
        return 0
    return srv, url
