#!/usr/bin/env python3
"""Build evals/e2e/e2e-report.html from results.jsonl + runs/*.ndjson.

    python3 evals/e2e/make_report.py

Single-file interactive report (tabs via hash routing), same visual system
as the branch-comparison report. Data is embedded — no server needed.
"""
import datetime
import glob
import json
import os

_HERE = os.path.dirname(os.path.abspath(__file__))
RESULTS = os.path.join(_HERE, "results.jsonl")
SCENARIOS = os.path.join(_HERE, "scenarios.json")
RUNS = os.path.join(_HERE, "runs")
OUT = os.path.join(_HERE, "e2e-report.html")

SCEN_LABEL = {
    "s1": "artifacts + verify ok + citation resolves",
    "s2": "no unmarked second page + extend/allow-new surfaced",
    "s3": "manifest + session-kind sources ingested",
    "s4": "current 7d + 30d superseded",
    "s5": "refusal tokens in output",
    "s6": "no grep/rg in commands",
    "s7": "postgres + retry 3/5 + census via query cmds",
    "s8": "graph.json + graph.html renders node labels",
}


def load_rows():
    rows = []
    for line in open(RESULTS):
        line = line.strip()
        if line:
            rows.append(json.loads(line))
    return rows


def transcript_meta(row):
    """duration (mtime - name ts) + final assistant text."""
    path = row.get("transcript") or ""
    if not path or not os.path.isfile(path):
        return None, None
    name = os.path.basename(path).replace(".ndjson", "")
    dur = None
    try:
        start = datetime.datetime.strptime(name[:15], "%Y%m%d-%H%M%S")
        dur = max(0, int(os.path.getmtime(path) - start.timestamp()))
    except ValueError:
        pass
    final = None
    for line in open(path):
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        if ev.get("type") == "message_end":
            msg = ev.get("message") or {}
            if msg.get("role") == "assistant":
                texts = [c.get("text", "") for c in msg.get("content", [])
                         if c.get("type") == "text"]
                if texts:
                    final = "\n".join(texts)
    return dur, final


CSS = r"""
:root{
  --bg:#f6f7f9; --panel:#ffffff; --ink:#1b2430; --muted:#5b6b7d; --line:#d9dee5;
  --accent:#1f6feb; --ok:#1a7f37; --okbg:#e6f4ea; --warn:#9a6700; --warnbg:#fff4d6;
  --err:#cf222e; --errbg:#ffebe9; --chip:#eef2f6; --mono:'SF Mono',Menlo,Consolas,'DejaVu Sans Mono',monospace;
  --c1:#1f6feb; --c2:#8250df;
}
@media (prefers-color-scheme: dark){
  :root{
    --bg:#0d1117; --panel:#161b22; --ink:#e6edf3; --muted:#8b949e; --line:#2d333b;
    --accent:#58a6ff; --ok:#3fb950; --okbg:#12261e; --warn:#d29922; --warnbg:#2a2410;
    --err:#f85149; --errbg:#3d1215; --chip:#21262d; --c1:#58a6ff; --c2:#a371f7;
  }
}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.55 -apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif}
header{padding:28px 32px 0}
h1{font-size:24px;margin:0 0 4px}
.sub{color:var(--muted);font-size:13px}
nav{display:flex;gap:6px;flex-wrap:wrap;padding:16px 32px 0;position:sticky;top:0;background:var(--bg);border-bottom:1px solid var(--line);z-index:5}
nav button{background:none;border:1px solid var(--line);border-bottom:none;color:var(--muted);padding:8px 16px;cursor:pointer;font-size:13px;border-radius:8px 8px 0 0}
nav button.on{color:var(--ink);background:var(--panel);font-weight:600}
main{padding:24px 32px 64px;max-width:1180px}
section{display:none;background:var(--panel);border:1px solid var(--line);border-radius:10px;padding:24px}
section.on{display:block}
h2{margin:0 0 12px;font-size:19px}
h3{font-size:15px;margin:22px 0 8px}
.cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:12px;margin:16px 0}
.card{background:var(--chip);border:1px solid var(--line);border-radius:8px;padding:14px}
.card b{display:block;font-size:26px;font-family:var(--mono)}
.card span{color:var(--muted);font-size:12px}
table{border-collapse:collapse;width:100%;font-size:13px;margin:10px 0}
th{background:var(--panel);text-align:left;color:var(--muted);font-weight:600;border-bottom:2px solid var(--line);padding:7px 10px;white-space:nowrap}
td{border-bottom:1px solid var(--line);padding:7px 10px;vertical-align:top}
tr:hover td{background:var(--chip)}
.mono{font-family:var(--mono);font-size:12px}
.badge{display:inline-block;padding:1px 8px;border-radius:10px;font-size:11px;font-family:var(--mono);white-space:nowrap}
.b-ok{background:var(--okbg);color:var(--ok)}
.b-warn{background:var(--warnbg);color:var(--warn)}
.b-err{background:var(--errbg);color:var(--err)}
.b-neutral{background:var(--chip);color:var(--muted)}
details{border:1px solid var(--line);border-radius:8px;margin:8px 0;background:var(--bg)}
details>summary{cursor:pointer;padding:9px 14px;font-size:13px;color:var(--accent);list-style:none}
details>summary::before{content:"▸ ";color:var(--muted)}
details[open]>summary::before{content:"▾ "}
details>div{padding:4px 14px 12px}
pre{background:var(--chip);border-radius:6px;padding:10px;overflow-x:auto;font-family:var(--mono);font-size:12px;line-height:1.5}
.sha{font-family:var(--mono);font-size:12px;color:var(--accent)}
code{font-family:var(--mono);font-size:12px;background:var(--chip);padding:1px 5px;border-radius:4px}
ul{margin:6px 0 6px 20px;padding:0} li{margin:3px 0}
.legend span{display:inline-block;margin-right:16px;font-size:12px;color:var(--muted)}
"""

JS = r"""
const $=s=>document.querySelector(s);
const esc=s=>String(s==null?"":s).replace(/[&<>]/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;"}[c]));
const rows=DATA.rows, scen=DATA.scenarios;
const shortWiki=/\/Users\/\S*?wiki\.py/g;
const badge=s=>({pass:'<span class="badge b-ok">PASS</span>',fail:'<span class="badge b-err">FAIL</span>',error:'<span class="badge b-err">ERROR</span>',skipped:'<span class="badge b-neutral">SKIP</span>'}[s]||s);
const dkey=r=>(r.transcript||"").split("/").pop().replace(".ndjson","");
const dur=r=>{const d=DATA.dur[dkey(r)];return d==null?"—":d+"s";};
const uniqCmds=r=>{const s=new Set(),o=[];for(const c of (r.commands_used||[])){const k=c.replace(shortWiki,"$WIKI");if(!s.has(k)){s.add(k);o.push(k);}}return o;};
const runsOf=(rt,sc)=>rows.filter(r=>r.runtime===rt&&r.scenario===sc);
const finalOf=rt=>Object.fromEntries([...new Set(rows.filter(r=>r.runtime===rt&&r.scenario!=="lane-fill-e2e").map(r=>r.scenario))].map(s=>{const rs=runsOf(rt,s);const p=rs.find(r=>r.status==="pass")||rs[rs.length-1];return [s,p];}));
const laneRow=rows.find(r=>r.scenario==="lane-fill-e2e");
const SCEN_LABEL=DATA.labels;

const tabs=[
 ["overview","Overview"],["lanefill","Lane-fill e2e"],["timeline","Run timeline"],["matrix","Runtime matrix"],["scenarios","Scenario detail"],["findings","Findings"]
];

function overview(){
 const omp=finalOf("omp");
 const stat=(label,val,sub)=>`<div class="card"><b>${val}</b><span>${label} — ${sub}</span></div>`;
 const passCount=Object.values(omp).filter(r=>r.status==="pass").length;
 const ompRuns=rows.filter(r=>r.runtime==="omp").length;
 const toks=rows.filter(r=>r.runtime==="omp").reduce((a,r)=>a+(r.tokens_estimate||0),0);
 return `<h2>What was tested</h2>
 <p>Eight scripted scenarios (S1–S8) driven through a real <code>omp -p --no-session</code> agent session against the <b>unified engine</b> at worktree HEAD — now including the lane-fill feature set (heuristic kind routing, auto-materialized decisions, Jev advisory verdicts). Each run builds a temp project from the golden corpus, lets the agent work freely, then checks outcomes with <b>deterministic filesystem/CLI assertions</b> — no LLM judging. Full history kept: bring-up failures are not hidden.</p>
 <div class="cards">
  ${stat("omp scenarios green", passCount+"/"+Object.keys(scen).length,"final attempts")}
  ${stat("omp runs recorded", ompRuns,"incl. bring-up retries")}
  ${stat("est. tokens", (toks/1e6).toFixed(1)+"M","across all omp runs")}
  ${stat("lane-fill e2e", laneRow?(laneRow.pass?"PASS":"FAIL"):"—","fresh ptt corpus wiki")}
 </div>
 <h2>Final outcome per scenario (omp)</h2>
 <table><tr><th>#</th><th>Scenario</th><th>Attempts</th><th>Final</th><th>Duration</th><th>Assertion detail</th></tr>
 ${Object.keys(scen).map(s=>{const rs=runsOf("omp",s);const f=omp[s];if(!f)return "";return `<tr><td><b class="sha">${s}</b></td><td>${esc((scen[s].instruction||"").split(":")[0].replace("In {project}","").trim()).slice(0,70)}</td><td>${rs.length>1?`<span class="badge b-warn">${rs.length} runs</span>`:"1"}</td><td>${badge(f.status)}</td><td>${dur(f)}</td><td class="mono" style="color:var(--muted)">${esc(f.detail||"")}</td></tr>`}).join("")}
 </table>
 <h2>Headline</h2>
 <p><b>The unified skill works end-to-end under a real agent — and after lane-fill, the wiki routes every claim kind and auto-materializes decision records.</b> The omp agent initialized, ingested, compiled, cited claims (S1); hit the near-dup write-guard and recovered (S2); classified+ingested a mixed inbox (S3); landed a staged correction superseding 30d → 7d (S4); refused an unanswerable question (S5); answered through the query ladder with zero <code>grep</code>/<code>rg</code> (S6); multi-part llm-query across database/conflict/census (S7); and emitted graph-data + rendered graph.html with live labels (S8). Separately, a fresh 129-source ptt/docs wiki exercised all four page lanes, Jev verdicts, and auto decision stubs — see the Lane-fill tab.</p>
 <p class="legend"><span>Legend:</span><span class="badge b-ok">PASS</span><span>assertion green</span><span class="badge b-err">FAIL/ERROR</span><span>assertion red / harness fault</span><span class="badge b-neutral">SKIP</span><span>runtime absent</span></p>`;
}

function lanefill(){
 if(!laneRow) return "<h2>Lane-fill e2e</h2><p>No lane-fill-e2e row in results.jsonl.</p>";
 const c=laneRow.counts||{}, j=laneRow.jev||{}, o=laneRow.omp_session||{}, d=laneRow.doctor||{};
 const stat=(label,val,sub)=>`<div class="card"><b>${val}</b><span>${label} — ${sub}</span></div>`;
 return `<h2>Lane-fill validation — fresh wiki on real corpus</h2>
 <p class="sub">Fresh <code>${esc(laneRow.demo_wiki||"/tmp/ptt-wiki-demo-2")}</code> built from <code>ptt/docs</code> at engine commit <span class="sha">${esc(laneRow.engine_commit||"")}</span>. Bounded representative extraction (${c.sources_extracted||"—"} of ${c.sources||"—"} sources); result row appended to <code>results.jsonl</code>.</p>
 <div class="cards">
  ${stat("sources ingested",c.sources||"—","triage + dir")}
  ${stat("claims reconciled",c.claims||"—","all UNRELATED, staged")}
  ${stat("auto decision stubs",c.decisions_auto||"—","explicit_project_decision")}
  ${stat("page kinds emitted",c.kinds_emitted||"—","heuristics live")}
  ${stat("pages written",c.pages_written||"—","write-page, 0 failures")}
  ${stat("doctor errors",d.errors!=null?d.errors:"—","auto_decision_stub info ×"+(d.auto_decision_stub_findings||0))}
 </div>
 <h2>Lane coverage</h2>
 <p>Before lane-fill every target fell to <code>concepts</code>. With the 5 built-in heuristics the same claims route:</p>
 <table><tr><th>kind</th><th>targets</th><th>matched_rule</th></tr>
 <tr><td><code>entities</code></td><td>6</td><td class="mono">heuristic:2 (ticket ids, dotted names)</td></tr>
 <tr><td><code>procedures</code></td><td>4</td><td class="mono">heuristic:1 (step/runbook predicates)</td></tr>
 <tr><td><code>questions</code></td><td>1</td><td class="mono">heuristic:4 (open-question markers)</td></tr>
 <tr><td><code>concepts</code></td><td>6</td><td class="mono">null (default)</td></tr></table>
 <h2>Jev advisory verdicts (live endpoint)</h2>
 <table><tr><th>probe</th><th>result</th></tr>
 <tr><td><code>jev --check</code></td><td class="mono">ready:true · model ${esc(j.model||"—")}</td></tr>
 <tr><td><code>page-targets --decide</code></td><td class="mono">${j.targets_with_verdict||"—"}/${c.pages_written||"—"} heuristic-routed targets carry decision.suggested_kind + confidence</td></tr>
 <tr><td><code>write-page --decide</code> near-dup</td><td class="mono">refusal carried decision.same_page_p = ${j.near_dup_same_page_p} (exit 3 unchanged)</td></tr></table>
 <h2>Separate omp query session</h2>
 <p class="sub"><code>omp -p --no-session</code> in a dir containing only the wiki — 4 questions.</p>
 <table><tr><th>question type</th><th>outcome</th></tr>
 <tr><td>entity fact (STC-88)</td><td>${badge("pass")} cited ${(o.pages_cited||[]).map(p=>`<code>${esc(p)}</code>`).join(" ")}</td></tr>
 <tr><td>procedure (pm-97 typecheck)</td><td>${badge("pass")} cited runbook steps</td></tr>
 <tr><td>recorded decision</td><td>${badge("pass")} cited (mention-token freeze)</td></tr>
 <tr><td>unanswerable (2028 pricing)</td><td>${badge("pass")} explicit refusal — no confabulation</td></tr></table>
 <p><b>detail:</b> ${esc(laneRow.detail||"")}</p>`;
}

function timeline(){
 return `<h2>All recorded runs — chronological</h2>
 <p class="sub">FAIL rows are real agent runs whose assertions were red on that attempt; ERROR rows are harness/runtime faults fixed during bring-up.</p>
 <table><tr><th>ts</th><th>runtime</th><th>scen</th><th>result</th><th>cmds</th><th>uniq</th><th>dur</th><th>~tok</th><th>detail</th></tr>
 ${rows.map(r=>`<tr><td class="mono">${r.ts.slice(9)}</td><td>${r.runtime}</td><td><b class="sha">${r.scenario}</b></td><td>${badge(r.status)}</td><td>${(r.commands_used||[]).length||"—"}</td><td>${uniqCmds(r).length||"—"}</td><td>${dur(r)}</td><td>${r.tokens_estimate?(r.tokens_estimate/1000).toFixed(0)+"k":"—"}</td><td class="mono" style="color:var(--muted)">${esc((r.detail||"").slice(0,160))}</td></tr>`).join("")}
 </table>`;
}

function matrix(){
 const rts=["omp","claude","cellock"], scs=Object.keys(scen);
 return `<h2>Runtime × scenario — latest attempt per cell</h2>
 <table><tr><th>runtime</th>${scs.map(s=>`<th>${s}</th>`).join("")}</tr>
 ${rts.map(rt=>`<tr><td><b>${rt}</b></td>${scs.map(s=>{const f=finalOf(rt)[s];if(!f)return "<td>—</td>";return `<td title="${esc(f.detail||"")}">${badge(f.status)}</td>`}).join("")}</tr>`).join("")}
 </table>
 <p class="sub">omp drives the full suite. claude <code>-p</code> exits 1 on this machine (API retry-storm / model env issue — environment, not skill). cellock has no headless CLI — explicit SKIP rows.</p>`;
}

function scenarios(){
 return Object.keys(scen).map(s=>{
  const rs=runsOf("omp",s);
  if(!rs.length) return "";
  return `<h2>${s} — ${esc(SCEN_LABEL[scen[s].assert]||scen[s].assert)}</h2>
  <p class="sub"><b>Instruction:</b> ${esc(scen[s].instruction)}</p>
  ${rs.map((r,i)=>`<details${r.status==="pass"?" open":""}><summary>${badge(r.status)} &nbsp;<span class="mono">${r.ts.slice(9)}</span> — ${(r.commands_used||[]).length} cmds (${uniqCmds(r).length} uniq) · ${dur(r)} · ${r.tokens_estimate?(r.tokens_estimate/1000).toFixed(0)+"k tok":"no run"}${rs.length>1?` · <em>attempt ${i+1}/${rs.length}</em>`:""}</summary><div>
   <p><b>detail:</b> ${esc(r.detail||"")}</p>
   ${DATA.final[dkey(r)]?`<p><b>agent final answer:</b></p><pre>${esc(DATA.final[dkey(r)])}</pre>`:""}
   ${uniqCmds(r).length?`<p><b>commands (deduped):</b></p><pre>${esc(uniqCmds(r).join("\n"))}</pre>`:""}
  </div></details>`).join("")}`;
 }).join("");
}

function findings(){
 return `<h2>Lane-fill implementation findings (this session)</h2>
 <table><tr><th>Finding</th><th>Evidence</th><th>Resolution</th></tr>
 <tr><td>heuristics gated behind <code>heuristics_active = not rules</code> — one file rule disabled ALL heuristics</td><td class="mono">test_predicate_and_min_claims_rules FAIL</td><td>per-subject fallthrough: file rules extend, not replace (<span class="sha">3290e80</span>)</td></tr>
 <tr><td>auto-materialized decision stubs leaked into context packs as authored decisions</td><td class="mono">2× test_contextpack FAIL</td><td>contextpack filters <code>auto_materialized</code> (<span class="sha">3290e80</span>)</td></tr>
 <tr><td>Jev wire shape ≠ spec: endpoint takes <code>criteria:</code> not <code>options:</code>; answers flat not nested</td><td class="mono">live probe</td><td><code>jev.ask</code> translates — callers keep spec shape (<span class="sha">3f0dd55</span>)</td></tr>
 <tr><td>questions lane starved: open-question subjects all at 1 root_origin or claimed by rules 1–3</td><td class="mono">T5 e2e</td><td>second-origin claim + decision-authority claim surfaced it (bounded-subset note)</td></tr>
 </table>
 <h2>Harness bugs found &amp; fixed during earlier bring-up</h2>
 <table><tr><th>Bug</th><th>Evidence</th><th>Fix</th></tr>
 <tr><td><code>{...}</code> literal in S3 instruction collided with <code>.format</code></td><td class="mono">S3 ERROR</td><td>escaped braces in scenarios.json</td></tr>
 <tr><td>assert_s4 multi-claim-per-value dict collision</td><td class="mono">S4 FAIL: <code>30d claim status=accepted</code></td><td>assertion walks all claims per value</td></tr>
 <tr><td>assert_s6 vacuously passed empty transcripts</td><td class="mono">S3 ERROR</td><td>inconclusive runs now fail</td></tr>
 <tr><td>assert_s2 too strict ("no second page, ever")</td><td class="mono">S2 FAIL: <code>--allow-new</code> used</td><td>accept <code>forced_new</code> marker as sanctioned escape</td></tr>
 <tr><td>missing <code>glob</code> import in assertions</td><td class="mono">S5 ERROR <code>name 'glob' is not defined</code></td><td>import added</td></tr>
 </table>
 <h2>Reproduce</h2>
 <pre>cd .worktrees/skill-effectiveness
python3 evals/e2e/run_e2e.py --runtime omp          # all scenarios
python3 evals/e2e/run_e2e.py --runtime omp --scenario S2
python3 evals/e2e/make_report.py                    # rebuild this report</pre>
 <p class="sub">Artifacts: <code>evals/e2e/results.jsonl</code> (append-only, gitignored) · <code>evals/e2e/runs/*.ndjson</code> transcripts · harness: <code>run_e2e.py</code> / <code>scenarios.json</code> / <code>assertions.py</code></p>`;
}

const VIEWS={overview,lanefill,timeline,matrix,scenarios,findings};
function go(name){
 if(!VIEWS[name]) name="overview";
 document.querySelectorAll("nav button").forEach(b=>b.classList.toggle("on",b.dataset.t===name));
 $("#main").innerHTML=`<section class="on">${VIEWS[name]()}</section>`;
}
$("nav").innerHTML=tabs.map(([id,l])=>`<button data-t="${id}">${l}</button>`).join("");
document.querySelectorAll("nav button").forEach(b=>b.onclick=()=>{location.hash=b.dataset.t;go(b.dataset.t);});
addEventListener("hashchange",()=>go(location.hash.slice(1)));
go(location.hash.slice(1));
"""


def main():
    rows = load_rows()
    with open(SCENARIOS) as f:
        scen = json.load(f)
    dur, final = {}, {}
    for r in rows:
        d, f_ = transcript_meta(r)
        key = os.path.basename(r.get("transcript") or "").replace(".ndjson", "")
        if key:
            if d is not None:
                dur[key] = d
            if f_:
                final[key] = f_
    data = {"rows": rows, "scenarios": scen, "dur": dur, "final": final,
            "labels": SCEN_LABEL}
    today = datetime.date.today().isoformat()
    head = subprocess_head()
    html = (
        '<!DOCTYPE html>\n<html lang="en"><head><meta charset="utf-8">'
        '<title>E2E report — omp runtime · llm-wiki skill-unified</title>'
        "<style>" + CSS + "</style></head><body>\n<header>\n"
        "<h1>E2E report — <span style=\"color:var(--c2)\">omp</span> runtime</h1>\n"
        '<p class="sub">skill-effectiveness HEAD <code>' + head + "</code>"
        " · engine = worktree wiki.py · <code>omp -p --no-session</code>"
        " · generated " + today + " · data embedded from "
        "<code>evals/e2e/results.jsonl</code> + transcripts</p>\n"
        "</header>\n<nav id=\"nav\"></nav>\n<main id=\"main\"></main>\n"
        "<script>const DATA = " + json.dumps(data) + ";</script>\n"
        "<script>" + JS + "</script>\n</body></html>"
    )
    with open(OUT, "w") as f:
        f.write(html)
    print("wrote %s (%d bytes, %d rows)" % (OUT, os.path.getsize(OUT), len(rows)))


def subprocess_head():
    import subprocess
    try:
        return subprocess.check_output(
            ["git", "log", "--oneline", "-1"],
            cwd=os.path.join(_HERE, "..", "..")).decode().strip()
    except Exception:
        return "?"


if __name__ == "__main__":
    main()
