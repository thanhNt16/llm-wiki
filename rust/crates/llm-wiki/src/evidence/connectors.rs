//! Built-in ingestion connectors — port of wikicore/connectors.py.
//!
//! Lanes: `ingest_git` (git history via `git` subprocess),
//! `ingest_feed` (RSS 2.0/RDF/Atom via quick-xml), and `ingest_preset`
//! (declarative JSON-API presets via ureq). Every item funnels through
//! `sources::ingest` so dedup, secrets warn-lane, injection scanning and
//! root_origin bookkeeping are identical to single-source ingest.
//!
//! Preset token rule (connectors.py:172-178): env-var bearer tokens exist
//! only in the headers handed to `fetch` — never in the result dict,
//! stored content, or raised messages.

use std::io::Read;
use std::path::Path;
use std::process::Command;

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::reader::NsReader;
use serde_json::{json, Value};

use crate::err::EngineError;
use crate::evidence::sources::{self, IngestOpts};
use crate::store::Wiki;

const PARSER_NAME: &str = "wikicore-connector";
const PARSER_VERSION: &str = "1.0";

// ---------------------------------------------------------------- git ----

/// `_git` (connectors.py:30-35): `git -C <repo> <args>`; non-zero exit is a
/// Txn with the subcommand name + stderr[:200].
fn git(repo: &Path, args: &[&str]) -> Result<String, EngineError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| EngineError::Txn(format!("git connector: {}: {e}", args[0])))?;
    if !out.status.success() {
        let stderr: String = String::from_utf8_lossy(&out.stderr)
            .trim()
            .chars()
            .take(200)
            .collect();
        return Err(EngineError::Txn(format!(
            "git connector: {}: {stderr}",
            args[0]
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `git_commits` (connectors.py:38-61): one record per commit —
/// sha/author/date/subject/body + changed files, oldest-first so a bounded
/// ingest keeps the start of history.
fn git_commits(repo: &Path, max_commits: usize) -> Result<Vec<GitCommit>, EngineError> {
    git(repo, &["rev-parse", "--git-dir"])?; // fail fast: not a repo
    let max = max_commits.to_string();
    let log = git(
        repo,
        &[
            "log",
            "--reverse",
            &format!("--max-count={max}"),
            "--date=iso-strict",
            "--pretty=format:%H%x1f%an%x1f%ae%x1f%ad%x1f%s%x1f%b%x1e",
        ],
    )?;
    let files = git(
        repo,
        &[
            "log",
            "--reverse",
            &format!("--max-count={max}"),
            "--name-only",
            "--pretty=format:%x1e%H",
        ],
    )?;
    let mut by_sha: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for chunk in files.split('\u{1e}') {
        // python splitlines keeps original lines, filtered on stripped content
        let lines: Vec<&str> = chunk
            .trim_matches('\n')
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        if let Some((sha, rest)) = lines.split_first() {
            by_sha
                .entry(sha.trim().to_string())
                .or_default()
                .extend(rest.iter().map(|s| s.to_string()));
        }
    }
    let mut commits = Vec::new();
    for rec in log.split('\u{1e}') {
        let f: Vec<String> = rec
            .trim_matches('\n')
            .split('\u{1f}')
            .map(|s| s.trim().to_string())
            .collect();
        if f.len() < 6 {
            continue;
        }
        let files = by_sha.get(&f[0]).cloned().unwrap_or_default();
        commits.push(GitCommit {
            sha: f[0].clone(),
            name: f[1].clone(),
            email: f[2].clone(),
            date: f[3].clone(),
            subject: f[4].clone(),
            body: f[5].clone(),
            files,
        });
    }
    Ok(commits)
}

struct GitCommit {
    sha: String,
    name: String,
    email: String,
    date: String,
    subject: String,
    body: String,
    files: Vec<String>,
}

/// `ingest_git` (connectors.py:64-85): each commit becomes a `git_commit`
/// source keyed `git:<repo>@<sha>` under root_origin `git:<repo>`.
pub fn ingest_git(
    wiki: &Wiki,
    repo: &str,
    max_commits: usize,
    force: bool,
) -> Result<Value, EngineError> {
    let abs = std::path::absolute(Path::new(repo))
        .map_err(|e| EngineError::Txn(format!("git connector: {repo}: {e}")))?;
    let repo = abs.to_string_lossy().into_owned();
    let mut items = 0_i64;
    let mut ingested = 0_i64;
    let mut deduplicated = 0_i64;
    let mut sources_out: Vec<Value> = Vec::new();
    for c in git_commits(Path::new(&repo), max_commits)? {
        let mut doc = format!(
            "# git commit {}\n\n- author: {} <{}>\n- date: {}\n\n## subject\n\n{}\n",
            c.sha, c.name, c.email, c.date, c.subject
        );
        if !c.body.is_empty() {
            doc += &format!("\n## message\n\n{}\n", c.body);
        }
        if !c.files.is_empty() {
            let list = c
                .files
                .iter()
                .map(|f| format!("- {f}"))
                .collect::<Vec<_>>()
                .join("\n");
            doc += &format!("\n## changed files\n\n{list}\n");
        }
        let opts = IngestOpts {
            normalized_content: Some(doc.clone()),
            parser_name: Some(PARSER_NAME.into()),
            root_origin: Some(format!("git:{repo}")),
            force,
            ..Default::default()
        };
        let r = sources::ingest(
            wiki,
            "git_commit",
            &format!("git:{repo}@{}", c.sha),
            doc.as_bytes(),
            &opts,
        )?;
        items += 1;
        if r.get("deduplicated").and_then(Value::as_bool).unwrap_or(false) {
            deduplicated += 1;
        } else {
            ingested += 1;
        }
        sources_out.push(r["source_id"].clone());
    }
    Ok(json!({
        "connector": "git",
        "repo": repo,
        "items": items,
        "ingested": ingested,
        "deduplicated": deduplicated,
        "sources": sources_out,
    }))
}

// --------------------------------------------------------------- feeds ----

/// Minimal ElementTree-shaped DOM: `name` is the expat-expanded tag
/// (`{uri}local` when namespaced, else `local`), `text` is the text before
/// the first child element only (ET `el.text` — child tails are dropped).
struct Elem {
    name: String,
    text: String,
    attrs: Vec<(String, String)>,
    children: Vec<Elem>,
}

fn malformed(msg: &str) -> EngineError {
    EngineError::Txn(format!("feed connector: malformed XML: {msg}"))
}

/// `ET.fromstring` (connectors.py:117-118): strict parse, ns-expanded tags.
fn parse_xml(data: &[u8]) -> Result<Elem, EngineError> {
    let mut reader = NsReader::from_reader(data);
    let mut stack: Vec<(Elem, bool /*first child seen*/)> = Vec::new();
    let mut root: Option<Elem> = None;
    loop {
        let (ns, event) = reader
            .read_resolved_event()
            .map_err(|e| malformed(&e.to_string()))?;
        let is_empty_elem = matches!(event, Event::Empty(_));
        match event {
            Event::Start(e) | Event::Empty(e) => {
                let local =
                    String::from_utf8_lossy(e.name().local_name().as_ref()).into_owned();
                let name = match ns {
                    ResolveResult::Bound(n) => {
                        format!(
                            "{{{}}}{}",
                            String::from_utf8_lossy(n.as_ref()),
                            local
                        )
                    }
                    ResolveResult::Unbound => local,
                    ResolveResult::Unknown(prefix) => {
                        return Err(malformed(&format!(
                            "unbound prefix {}",
                            String::from_utf8_lossy(prefix.as_ref())
                        )));
                    }
                };
                let mut attrs = Vec::new();
                for attr in e.attributes() {
                    let attr = attr.map_err(|e2| malformed(&e2.to_string()))?;
                    let (ans, alocal) = reader.resolve(attr.key, true);
                    let aname = match ans {
                        ResolveResult::Bound(n) => format!(
                            "{{{}}}{}",
                            String::from_utf8_lossy(n.as_ref()),
                            String::from_utf8_lossy(alocal.as_ref())
                        ),
                        // attributes never take the default ns; unknown
                        // prefix stays prefixed (ET-compatible enough for
                        // the fields connectors read)
                        _ => String::from_utf8_lossy(attr.key.as_ref()).into_owned(),
                    };
                    let aval = attr
                        .decode_and_unescape_value(reader.decoder())
                        .map_err(|e2| malformed(&e2.to_string()))?;
                    attrs.push((aname, aval.into_owned()));
                }
                if let Some((parent, seen)) = stack.last_mut() {
                    *seen = true;
                    let _ = parent;
                }
                let elem = Elem {
                    name,
                    text: String::new(),
                    attrs,
                    children: Vec::new(),
                };
                if is_empty_elem {
                    match stack.last_mut() {
                        Some((parent, _)) => parent.children.push(elem),
                        None => {
                            if root.is_some() {
                                return Err(malformed("junk after document element"));
                            }
                            root = Some(elem);
                        }
                    }
                } else {
                    stack.push((elem, false));
                }
            }
            Event::End(_) => match stack.pop() {
                Some((elem, _)) => match stack.last_mut() {
                    Some((parent, _)) => parent.children.push(elem),
                    None => {
                        if root.is_some() {
                            return Err(malformed("junk after document element"));
                        }
                        root = Some(elem);
                    }
                },
                None => return Err(malformed("mismatched tag")),
            },
            Event::Text(t) => {
                let s = t.unescape().map_err(|e| malformed(&e.to_string()))?;
                match stack.last_mut() {
                    Some((parent, seen)) if !*seen => parent.text.push_str(&s),
                    Some(_) => {} // child tail — ET drops it from el.text
                    None => {
                        if !s.trim().is_empty() {
                            return Err(malformed("junk after document element"));
                        }
                    }
                }
            }
            Event::CData(t) => {
                let s = t.decode().map_err(|e| malformed(&e.to_string()))?;
                if let Some((parent, seen)) = stack.last_mut() {
                    if !*seen {
                        parent.text.push_str(&s);
                    }
                }
            }
            Event::Decl(_) | Event::PI(_) | Event::DocType(_) | Event::Comment(_) => {}
            Event::Eof => break,
        }
    }
    if !stack.is_empty() {
        return Err(malformed("unclosed token"));
    }
    root.ok_or_else(|| malformed("no element found"))
}

/// `_Strip`/`_text` (connectors.py:90-105): HTMLParser over el.text keeps
/// only handle_data pieces — tags/comments/CDATA sections dropped, charrefs
/// decoded per piece, whitespace collapsed.
fn elem_text(el: Option<&Elem>) -> String {
    match el {
        None => String::new(),
        Some(e) => collapse_ws(&html_strip(&e.text)),
    }
}

/// HTMLParser-shaped stripper: returns concatenated character data with
/// entity refs decoded inside each data piece (convert_charrefs=True).
fn html_strip(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            if s[i..].starts_with("<!--") {
                match s[i + 4..].find("-->") {
                    Some(end) => {
                        i += 4 + end + 3;
                        continue;
                    }
                    None => break,
                }
            }
            if s[i..].starts_with("<![CDATA[") {
                // HTMLParser drops marked sections in html content
                match s[i + 9..].find("]]>") {
                    Some(end) => {
                        i += 9 + end + 3;
                        continue;
                    }
                    None => break,
                }
            }
            // tag: '>' inside quoted attr values does not end it
            let mut j = i + 1;
            let mut quote = 0u8;
            while j < b.len() {
                let c = b[j];
                if quote != 0 {
                    if c == quote {
                        quote = 0;
                    }
                } else if c == b'"' || c == b'\'' {
                    quote = c;
                } else if c == b'>' {
                    break;
                }
                j += 1;
            }
            if j < b.len() {
                i = j + 1;
                continue;
            }
            // unterminated '<' — HTMLParser emits it as data
            out.push_str(&html_unescape(&s[i..]));
            return out;
        }
        let next = s[i..].find('<').map(|k| i + k).unwrap_or(s.len());
        out.push_str(&html_unescape(&s[i..next]));
        i = next;
    }
    out
}

/// convert_charrefs: `&name;`/`&#123;`/`&#x1F;` → char; unknown entities
/// stay literal, matching HTMLParser.
fn html_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';') {
            let ent = &after[..semi];
            if !ent.is_empty() && ent.len() <= 32 && !ent.contains(char::is_whitespace) {
                if let Some(decoded) = decode_entity(ent) {
                    out.push_str(&decoded);
                    rest = &after[semi + 1..];
                    continue;
                }
            }
        }
        out.push('&');
        rest = after;
    }
    out.push_str(rest);
    out
}

fn decode_entity(ent: &str) -> Option<String> {
    if let Some(num) = ent.strip_prefix('#') {
        let (digits, radix) = match num.strip_prefix(['x', 'X']) {
            Some(h) => (h, 16),
            None => (num, 10),
        };
        return u32::from_str_radix(digits, radix)
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string());
    }
    let c = match ent {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "copy" => "\u{a9}",
        "reg" => "\u{ae}",
        "trade" => "\u{2122}",
        "hellip" => "\u{2026}",
        "mdash" => "\u{2014}",
        "ndash" => "\u{2013}",
        "laquo" => "\u{ab}",
        "raquo" => "\u{bb}",
        "times" => "\u{d7}",
        "divide" => "\u{f7}",
        _ => return None,
    };
    Some(c.to_string())
}

/// `re.sub(r"\s+", " ", s).strip()` on the joined data pieces.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `_tag` (connectors.py:108-112): direct child whose tag is `name` or
/// ends `}name` (namespace-suffix match, not prefix match).
fn tag<'e>(el: &'e Elem, name: &str) -> Option<&'e Elem> {
    el.children
        .iter()
        .find(|c| c.name == name || c.name.ends_with(&format!("}}{name}")))
}

fn ends_ns_tag(name: &str, local: &str) -> bool {
    name.ends_with(&format!("}}{local}"))
}

fn descendants<'e>(el: &'e Elem, out: &mut Vec<&'e Elem>) {
    out.push(el);
    for c in &el.children {
        descendants(c, out);
    }
}

/// Parsed feed item fields (connectors.py:138-146 dict shape).
#[derive(Debug)]
pub struct FeedItem {
    pub title: String,
    pub link: String,
    pub guid: String,
    pub updated: String,
    pub content: String,
}

const ATOM_NS: &str = "{http://www.w3.org/2005/Atom}";

/// `feed_items` (connectors.py:115-147): RSS 2.0 / RDF / Atom item fields.
pub fn feed_items(data: &[u8], limit: usize) -> Result<Vec<FeedItem>, EngineError> {
    let root = parse_xml(data)?;
    let items: Vec<&Elem> = if root.name == "rss" || ends_ns_tag(&root.name, "rss") {
        let chan = tag(&root, "channel");
        // `chan or root` — an empty <channel/> element is falsy in ET
        let host = match chan {
            Some(c) if !c.children.is_empty() => c,
            _ => &root,
        };
        host.children
            .iter()
            .filter(|c| c.name == "item")
            .collect()
    } else if root.name == format!("{ATOM_NS}feed") {
        root.children
            .iter()
            .filter(|c| c.name == format!("{ATOM_NS}entry"))
            .collect()
    } else if ends_ns_tag(&root.name, "RDF") {
        let mut all = Vec::new();
        descendants(&root, &mut all);
        all.into_iter()
            .filter(|c| ends_ns_tag(&c.name, "item"))
            .collect()
    } else {
        return Err(EngineError::Txn(format!(
            "feed connector: not an RSS/Atom feed (root '{}')",
            root.name
        )));
    };
    let mut out = Vec::new();
    for it in items.into_iter().take(limit) {
        let guid = {
            let g = elem_text(tag(it, "guid"));
            if !g.is_empty() {
                g
            } else {
                elem_text(tag(it, "id"))
            }
        };
        let mut link = elem_text(tag(it, "link"));
        if link.is_empty() {
            if let Some(lh) = it
                .children
                .iter()
                .find(|c| c.name == format!("{ATOM_NS}link"))
            {
                link = lh
                    .attrs
                    .iter()
                    .find(|(k, _)| k == "href")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
            }
        }
        let title = elem_text(tag(it, "title"));
        let updated = {
            let u = elem_text(tag(it, "updated"));
            if !u.is_empty() {
                u
            } else {
                let p = elem_text(tag(it, "pubDate"));
                if !p.is_empty() {
                    p
                } else {
                    elem_text(tag(it, "published"))
                }
            }
        };
        let content = {
            let d = elem_text(tag(it, "description"));
            if !d.is_empty() {
                d
            } else {
                let s = elem_text(tag(it, "summary"));
                if !s.is_empty() {
                    s
                } else {
                    elem_text(tag(it, "content"))
                }
            }
        };
        out.push(FeedItem {
            title,
            link: link.clone(),
            guid: if !guid.is_empty() { guid } else { link },
            updated,
            content,
        });
    }
    Ok(out)
}

/// `ingest_feed` (connectors.py:150-168): each item becomes a `feed_item`
/// source keyed `feed:<ref>#<guid|title>` under root_origin `feed:<ref>`.
pub fn ingest_feed(
    wiki: &Wiki,
    rref: &str,
    data: &[u8],
    limit: usize,
    force: bool,
) -> Result<Value, EngineError> {
    let mut items = 0_i64;
    let mut ingested = 0_i64;
    let mut deduplicated = 0_i64;
    let mut warnings = 0_i64;
    let mut sources_out: Vec<Value> = Vec::new();
    for it in feed_items(data, limit)? {
        let heading = if !it.title.is_empty() { &it.title } else { &it.guid };
        let doc = format!(
            "# {heading}\n\n- link: {}\n- updated: {}\n\n{}\n",
            it.link, it.updated, it.content
        );
        let item_key = if !it.guid.is_empty() { &it.guid } else { &it.title };
        let opts = IngestOpts {
            normalized_content: Some(doc.clone()),
            parser_name: Some(PARSER_NAME.into()),
            root_origin: Some(format!("feed:{rref}")),
            force,
            ..Default::default()
        };
        let r = sources::ingest(
            wiki,
            "feed_item",
            &format!("feed:{rref}#{item_key}"),
            doc.as_bytes(),
            &opts,
        )?;
        items += 1;
        if r.get("deduplicated").and_then(Value::as_bool).unwrap_or(false) {
            deduplicated += 1;
        } else {
            ingested += 1;
        }
        warnings += r
            .get("warnings")
            .and_then(Value::as_array)
            .map(|a| a.len() as i64)
            .unwrap_or(0);
        sources_out.push(r["source_id"].clone());
    }
    Ok(json!({
        "connector": "feed",
        "feed": rref,
        "items": items,
        "ingested": ingested,
        "deduplicated": deduplicated,
        "sources": sources_out,
        "warnings": warnings,
    }))
}

// ------------------------------------------------------------- presets ----

/// Fetch failure taxonomy (connectors.py:309-316): `Txn` propagates
/// verbatim like Python's `except TxnError: raise`; `Other` is wrapped in
/// "preset <p> fetch/parse failed for <ref>: <e>" like the catch-all.
#[derive(Debug)]
pub enum FetchError {
    Txn(String),
    Other(String),
}

/// fetch callable shape: `fetch(url, headers) -> bytes`. Token rule: the
/// bearer lives only inside `headers`, never in strings ingest_preset emits.
pub type Fetch = dyn Fn(&str, &[(String, String)]) -> Result<Vec<u8>, FetchError>;

/// Python `str(v)` for the scalar shapes preset items carry.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python truthiness for `item.get(k) or ...` idioms.
fn py_truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn py_get<'v>(item: &'v Value, key: &str) -> &'v Value {
    item.get(key).unwrap_or(&Value::Null)
}

/// `str(item.get(key) or "")`.
fn py_str_or_empty(item: &Value, key: &str) -> String {
    let v = py_get(item, key);
    if py_truthy(Some(v)) {
        py_str(v)
    } else {
        String::new()
    }
}

/// `_map_github_issue` (connectors.py:181-190).
fn map_github_issue(item: &Value) -> Cand {
    let body = py_str_or_empty(item, "body");
    let body = body.trim();
    Cand {
        subject: format!(
            "issue #{}: {}",
            py_str(py_get(item, "number")),
            py_str(py_get(item, "title"))
        ),
        predicate: "state".into(),
        value: py_str_or_empty(item, "state"),
        // body preserved as verbatim evidence via an s: locator
        locator: (!body.is_empty()).then(|| format!("s:{body}")),
    }
}

/// `_map_rest_item` (connectors.py:193-201).
fn map_rest_item(item: &Value) -> Cand {
    let body = py_str_or_empty(item, "body");
    let body = body.trim();
    Cand {
        subject: py_str_or_empty(item, "title"),
        predicate: "item".into(),
        value: py_str(py_get(item, "id")),
        locator: (!body.is_empty()).then(|| format!("s:{body}")),
    }
}

/// Candidate-shaped record (subject/predicate/value + verbatim locator).
struct Cand {
    subject: String,
    predicate: String,
    value: String,
    #[allow(dead_code)] // carried for parity with the Python cand dict
    locator: Option<String>,
}

struct Preset {
    /// env var supplying a bearer token (value never enters output)
    env: Option<&'static str>,
    /// key holding the item array; None = response is a top-level array
    items: Option<&'static str>,
    required: &'static [&'static str],
    map: fn(&Value) -> Cand,
    accept: &'static str,
    /// item filter — e.g. GitHub /issues also returns PRs
    skip: Option<fn(&Value) -> bool>,
    /// bearer only to refs whose hostname ends in one of these suffixes
    auth_hosts: Option<&'static [&'static str]>,
}

/// `PRESETS` (connectors.py:214-233) — github-issues + generic REST.
fn preset_config(preset: &str) -> Option<Preset> {
    match preset {
        "github-issues" => Some(Preset {
            env: Some("GITHUB_TOKEN"),
            items: None,
            required: &["number", "title", "state"],
            map: map_github_issue,
            accept: "application/vnd.github+json",
            skip: Some(|i| i.get("pull_request").is_some()),
            auth_hosts: Some(&[".github.com", ".ghe.com"]),
        }),
        "rest" => Some(Preset {
            env: None,
            items: Some("items"),
            required: &["title", "body", "id"],
            map: map_rest_item,
            accept: "application/json",
            skip: None,
            auth_hosts: None,
        }),
        _ => None,
    }
}

/// `list_presets` (connectors.py:236-237): sorted preset names.
pub fn list_presets() -> Vec<&'static str> {
    vec!["github-issues", "rest"]
}

fn url_scheme(url: &str) -> String {
    match url.split_once(':') {
        Some((s, _))
            if !s.is_empty()
                && s.chars().next().unwrap().is_ascii_alphabetic()
                && s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) =>
        {
            s.to_lowercase()
        }
        _ => String::new(),
    }
}

/// urllib.parse `hostname` — lowercase, brackets/userinfo/port stripped.
fn url_hostname(url: &str) -> String {
    let rest = match url.split_once("//") {
        Some((_, r)) => r,
        None => return String::new(),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let hostport = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(b) = hostport.strip_prefix('[') {
        b.split(']').next().unwrap_or("")
    } else {
        hostport.split(':').next().unwrap_or("")
    };
    host.to_lowercase()
}

/// `_default_fetch` (connectors.py:240-261): http/https only, 60 s timeout,
/// body truncated at 50 MB. ureq's SameHost auth-forwarding replicates the
/// _HttpOnlyRedirect rule (credentials never cross origins).
fn default_fetch(url: &str, headers: &[(String, String)]) -> Result<Vec<u8>, FetchError> {
    let scheme = url_scheme(url);
    if scheme != "http" && scheme != "https" {
        return Err(FetchError::Txn(format!(
            "preset fetch: only http/https refs allowed (got '{url}')"
        )));
    }
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(60)))
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
            .build(),
    );
    let mut req = agent.get(url);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req.call().map_err(|e| FetchError::Other(e.to_string()))?;
    let mut body = resp.into_body();
    let mut data = Vec::new();
    body.as_reader()
        .take(50 * 1024 * 1024)
        .read_to_end(&mut data)
        .map_err(|e| FetchError::Other(e.to_string()))?;
    Ok(data)
}

/// `_preset_headers` (connectors.py:264-279): Accept + User-Agent always;
/// bearer only when the env var is set AND the ref host matches
/// auth_hosts (when declared). The token never leaves this dict.
fn preset_headers(cfg: &Preset, rref: &str) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Accept".to_string(), cfg.accept.to_string()),
        (
            "User-Agent".to_string(),
            format!("wikicore-connector/{PARSER_VERSION}"),
        ),
    ];
    if let Some(env) = cfg.env {
        if let Ok(token) = std::env::var(env) {
            if !token.is_empty() {
                let allowed = match cfg.auth_hosts {
                    None => true,
                    Some(suffixes) => {
                        let host = url_hostname(rref);
                        suffixes.iter().any(|s| host.ends_with(s))
                    }
                };
                if allowed {
                    headers.push((
                        "Authorization".to_string(),
                        format!("Bearer {token}"),
                    ));
                }
            }
        }
    }
    headers
}

/// `_preset_doc` (connectors.py:282-288).
fn preset_doc(item_id: &str, cand: &Cand, item: &Value) -> String {
    let body = py_str_or_empty(item, "body");
    let body = body.trim();
    let mut doc = format!(
        "# {}\n\n- preset item id: {}\n- predicate: {}\n- value: {}\n",
        cand.subject, item_id, cand.predicate, cand.value
    );
    if !body.is_empty() {
        doc += &format!("\n## body\n\n{body}\n");
    }
    doc
}

/// `ingest_preset` (connectors.py:291-364) with the default urllib-parity
/// fetch — production path for `ingest --connector`.
pub fn ingest_preset(
    wiki: &Wiki,
    preset: &str,
    rref: &str,
    limit: usize,
    force: bool,
) -> Result<Value, EngineError> {
    ingest_preset_with(wiki, preset, rref, limit, force, &default_fetch)
}

/// `ingest_preset(..., fetch=...)`: items ingest as `feed_item` sources
/// keyed `preset:<preset>:<ref>#<item-id>` so re-runs dedup per item.
/// `skipped` counts malformed items plus deduplicated re-runs.
pub fn ingest_preset_with(
    wiki: &Wiki,
    preset: &str,
    rref: &str,
    limit: usize,
    force: bool,
    fetch: &Fetch,
) -> Result<Value, EngineError> {
    let cfg = preset_config(preset).ok_or_else(|| {
        EngineError::Txn(format!(
            "unknown connector preset: '{preset}' — valid: {}",
            list_presets().join(", ")
        ))
    })?;
    let headers = preset_headers(&cfg, rref);
    let raw = fetch(rref, &headers).map_err(|e| match e {
        FetchError::Txn(m) => EngineError::Txn(m),
        FetchError::Other(m) => EngineError::Txn(format!(
            "preset '{preset}' fetch/parse failed for '{rref}': {m}"
        )),
    })?;
    let data: Value = serde_json::from_slice(&raw).map_err(|e| {
        EngineError::Txn(format!(
            "preset '{preset}' fetch/parse failed for '{rref}': {e}"
        ))
    })?;
    let items_value: Value = if let Some(key) = cfg.items {
        if !data.is_object() {
            return Err(EngineError::Txn(format!(
                "preset '{preset}': expected JSON object with '{key}' key in '{rref}'"
            )));
        }
        data.get(key).cloned().unwrap_or(Value::Array(vec![]))
    } else {
        data
    };
    let items = items_value.as_array().ok_or_else(|| {
        let at = cfg.items.unwrap_or("<root>");
        EngineError::Txn(format!(
            "preset '{preset}': expected an item list at '{at}' in '{rref}'"
        ))
    })?;
    let mut items_count = 0_i64;
    let mut ingested = 0_i64;
    let mut deduplicated = 0_i64;
    let mut warnings = 0_i64;
    let mut skipped = 0_i64;
    let mut new_in_store = 0_i64;
    let mut sources_out: Vec<Value> = Vec::new();
    let mut source_id = Value::Null;
    let mut version = Value::Null;
    for item in items.iter().take(limit) {
        items_count += 1;
        if !item.is_object() {
            skipped += 1;
            continue;
        }
        let skip = cfg.skip.map(|s| s(item)).unwrap_or(false)
            || cfg
                .required
                .iter()
                .any(|k| item.get(k).map_or(true, |v| v.is_null()));
        if skip {
            skipped += 1;
            continue;
        }
        let cand = (cfg.map)(item);
        // `item.get("id") or item.get("number")` — first truthy wins
        let item_id = item
            .get("id")
            .filter(|v| py_truthy(Some(v)))
            .or_else(|| item.get("number"));
        if !py_truthy(item_id) {
            return Err(EngineError::Txn(format!(
                "preset '{preset}': item in '{rref}' has no usable id/number"
            )));
        }
        let item_id = py_str(item_id.unwrap());
        let doc = preset_doc(&item_id, &cand, item);
        let opts = IngestOpts {
            normalized_content: Some(doc.clone()),
            parser_name: Some(PARSER_NAME.into()),
            root_origin: Some(format!("preset:{preset}:{rref}")),
            force,
            ..Default::default()
        };
        let r = sources::ingest(
            wiki,
            "feed_item",
            &format!("preset:{preset}:{rref}#{item_id}"),
            doc.as_bytes(),
            &opts,
        )?;
        sources_out.push(r["source_id"].clone());
        if source_id.is_null() {
            source_id = r["source_id"].clone();
            version = r["version"].clone();
        }
        if r.get("deduplicated").and_then(Value::as_bool).unwrap_or(false) {
            deduplicated += 1;
            skipped += 1;
        } else {
            ingested += 1;
            new_in_store += 1;
        }
        warnings += r
            .get("warnings")
            .and_then(Value::as_array)
            .map(|a| a.len() as i64)
            .unwrap_or(0);
    }
    Ok(json!({
        "connector": "preset",
        "preset": preset,
        "ref": rref,
        "items": items_count,
        "ingested": ingested,
        "deduplicated": deduplicated,
        "warnings": warnings,
        "source_id": source_id,
        "version": version,
        "sources": sources_out,
        "new_in_store": new_in_store,
        "skipped": skipped,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::init_wiki;

    fn wiki() -> (tempfile::TempDir, Wiki) {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        init_wiki(&wiki, "t").unwrap();
        (dir, wiki)
    }

    fn ok_body(
        body: &str,
    ) -> impl Fn(&str, &[(String, String)]) -> Result<Vec<u8>, FetchError> + '_ {
        move |_: &str, _: &[(String, String)]| Ok(body.as_bytes().to_vec())
    }

    #[test]
    fn list_presets_sorted() {
        assert_eq!(list_presets(), vec!["github-issues", "rest"]);
    }

    #[test]
    fn rest_preset_ingests_and_dedups() {
        let (_d, w) = wiki();
        let body = r#"{"items":[
            {"id": 1, "title": "alpha", "body": "first"},
            {"id": 2, "title": "beta", "body": "second"},
            {"id": 3, "title": "nobody"},
            "junk"
        ]}"#;
        let r = ingest_preset_with(&w, "rest", "http://x/api", 50, false, &ok_body(body)).unwrap();
        assert_eq!(r["items"], 4);
        assert_eq!(r["ingested"], 2);
        assert_eq!(r["skipped"], 2);
        assert_eq!(r["new_in_store"], 2);
        assert!(r["source_id"].is_string());
        assert_eq!(r["sources"].as_array().unwrap().len(), 2);
        // re-run dedups per item: skipped swallows deduplicated rows
        let r2 = ingest_preset_with(&w, "rest", "http://x/api", 50, false, &ok_body(body)).unwrap();
        assert_eq!(r2["deduplicated"], 2);
        assert_eq!(r2["skipped"], 4);
        assert_eq!(r2["ingested"], 0);
        assert_eq!(r2["source_id"], r["source_id"]);
    }

    #[test]
    fn github_preset_headers_and_skip() {
        let (_d, w) = wiki();
        std::env::set_var("GITHUB_TOKEN", "tok-secret-123");
        let captured = std::rc::Rc::new(std::sync::Mutex::new(Vec::new()));
        let cap2 = captured.clone();
        let fetch = move |_url: &str, headers: &[(String, String)]| {
            cap2.lock().unwrap().push(headers.to_vec());
            Ok(br#"[
                {"number": 5, "title": "bug", "state": "open",
                 "body": "it broke", "id": 11},
                {"number": 6, "title": "pr", "state": "open",
                 "pull_request": {"url": "x"}, "id": 12},
                {"number": 7, "title": null, "state": "closed", "id": 13}
            ]"#
            .to_vec())
        };
        let r = ingest_preset_with(
            &w,
            "github-issues",
            "https://api.github.com/repos/o/r/issues",
            50,
            false,
            &fetch,
        )
        .unwrap();
        std::env::remove_var("GITHUB_TOKEN");
        let headers = captured.lock().unwrap();
        assert_eq!(headers.len(), 1);
        let h = &headers[0];
        assert!(h.iter().any(|(k, v)| k == "Accept"
            && v == "application/vnd.github+json"));
        assert!(h
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer tok-secret-123"));
        // token must never appear in any emitted string
        assert!(!r.to_string().contains("tok-secret-123"), "{r}");
        assert_eq!(r["items"], 3);
        assert_eq!(r["ingested"], 1); // PR skipped, null title skipped
        assert_eq!(r["skipped"], 2);
    }

    #[test]
    fn github_token_not_sent_off_host() {
        std::env::set_var("GITHUB_TOKEN", "tok-secret-123");
        let captured = std::rc::Rc::new(std::sync::Mutex::new(Vec::new()));
        let cap2 = captured.clone();
        let fetch = move |_url: &str, headers: &[(String, String)]| {
            cap2.lock().unwrap().push(headers.to_vec());
            Ok(b"[]".to_vec())
        };
        // default_fetch would reject non-http(s); closure is fine, host gate
        // happens in preset_headers before fetch is ever called
        let wdir = tempfile::tempdir().unwrap();
        let w = Wiki::open(wdir.path()).unwrap();
        init_wiki(&w, "t").unwrap();
        ingest_preset_with(
            &w,
            "github-issues",
            "https://evil.example.com/issues",
            50,
            false,
            &fetch,
        )
        .unwrap();
        std::env::remove_var("GITHUB_TOKEN");
        let headers = captured.lock().unwrap();
        assert!(!headers[0].iter().any(|(k, _)| k == "Authorization"));
    }

    #[test]
    fn unknown_preset_and_fetch_failures() {
        let (_d, w) = wiki();
        let err = ingest_preset(&w, "bogus", "http://x", 50, false).unwrap_err();
        let m = format!("{err}");
        assert!(m.contains("unknown connector preset"), "{m}");
        assert!(m.contains("github-issues"), "{m}");

        let boom = |_: &str, _: &[(String, String)]| Err(FetchError::Other("dns gone".into()));
        let err = ingest_preset_with(&w, "rest", "http://x/api", 50, false, &boom).unwrap_err();
        let m = format!("{err}");
        assert!(m.contains("fetch/parse failed"), "{m}");
        assert!(m.contains("dns gone"), "{m}");

        // Txn-class fetch failures pass through unwrapped
        let txn = |_: &str, _: &[(String, String)]| {
            Err(FetchError::Txn("only http/https refs allowed".into()))
        };
        let err = ingest_preset_with(&w, "rest", "http://x/api", 50, false, &txn).unwrap_err();
        assert_eq!(format!("{err}"), "only http/https refs allowed");

        // non-http(s) ref rejected by the default fetch
        let err = ingest_preset(&w, "rest", "file:///tmp/x.json", 50, false).unwrap_err();
        assert!(format!("{err}").contains("only http/https"), "{err}");
    }

    #[test]
    fn preset_shape_validation() {
        let (_d, w) = wiki();
        // rest preset expects {"items": [...]}
        let err = ingest_preset_with(&w, "rest", "http://x", 50, false, &ok_body("[1,2]"))
            .unwrap_err();
        assert!(format!("{err}").contains("expected JSON object"), "{err}");
        let err = ingest_preset_with(&w, "rest", "http://x", 50, false, &ok_body("{}"))
            .unwrap();
        // missing "items" key -> empty list, items=0
        assert_eq!(err["items"], 0);
        let err = ingest_preset_with(&w, "rest", "http://x", 50, false, &ok_body("{\"items\":{}}"))
            .unwrap_err();
        assert!(format!("{err}").contains("expected an item list"), "{err}");
        let err = ingest_preset_with(&w, "rest", "http://x", 50, false, &ok_body("{bad json"))
            .unwrap_err();
        assert!(format!("{err}").contains("fetch/parse failed"), "{err}");
        // github preset: items live at the root
        let r = ingest_preset_with(&w, "github-issues", "https://api.github.com/x", 50,
            false, &ok_body("{\"not\":\"array\"}")).unwrap_err();
        assert!(format!("{r}").contains("expected an item list"), "{r}");
    }

    const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>t</title>
  <item><title>First</title>
    <link>https://e/1</link><guid>g1</guid>
    <pubDate>Tue, 01 Jan 2030</pubDate>
    <description>hello &amp; &lt;b&gt;bold&lt;/b&gt; world</description>
  </item>
  <item><title>Second</title>
    <link>https://e/2</link><guid>g2</guid>
    <description><![CDATA[<p>raw</p> here]]></description>
  </item>
</channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <entry>
    <title>A1</title><id>urn:a1</id>
    <link href="https://e/a1"/>
    <updated>2030-01-01T00:00:00Z</updated>
    <summary>atom sum</summary>
  </entry>
</feed>"#;

    #[test]
    fn feed_items_rss() {
        let items = feed_items(RSS.as_bytes(), 50).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "First");
        assert_eq!(items[0].guid, "g1");
        assert_eq!(items[0].updated, "Tue, 01 Jan 2030");
        // html tags stripped, xml entities decoded, whitespace collapsed
        assert_eq!(items[0].content, "hello & bold world");
        assert_eq!(items[1].content, "raw here");
    }

    #[test]
    fn feed_items_atom() {
        let items = feed_items(ATOM.as_bytes(), 50).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].guid, "urn:a1");
        assert_eq!(items[0].link, "https://e/a1");
        assert_eq!(items[0].updated, "2030-01-01T00:00:00Z");
        assert_eq!(items[0].content, "atom sum");
    }

    #[test]
    fn feed_items_malformed_and_unknown() {
        let err = feed_items(b"<rss><channel><item>", 50).unwrap_err();
        assert!(format!("{err}").contains("malformed XML"), "{err}");
        let err = feed_items(b"<html><body/></html>", 50).unwrap_err();
        let m = format!("{err}");
        assert!(m.contains("not an RSS/Atom feed"), "{m}");
        assert!(m.contains("html"), "{m}");
    }

    #[test]
    fn feed_items_limit() {
        assert_eq!(feed_items(RSS.as_bytes(), 1).unwrap().len(), 1);
    }

    #[test]
    fn ingest_feed_writes_sources_and_dedups() {
        let (_d, w) = wiki();
        let r = ingest_feed(&w, "file:///f.xml", RSS.as_bytes(), 50, false).unwrap();
        assert_eq!(r["connector"], "feed");
        assert_eq!(r["items"], 2);
        assert_eq!(r["ingested"], 2);
        assert_eq!(r["deduplicated"], 0);
        assert_eq!(r["sources"].as_array().unwrap().len(), 2);
        let r2 = ingest_feed(&w, "file:///f.xml", RSS.as_bytes(), 50, false).unwrap();
        assert_eq!(r2["deduplicated"], 2);
        assert_eq!(r2["ingested"], 0);
    }

    #[test]
    fn git_connector_rejects_non_repo() {
        let (_d, w) = wiki();
        let dir = tempfile::tempdir().unwrap();
        let err = ingest_git(&w, dir.path().to_str().unwrap(), 200, false).unwrap_err();
        let m = format!("{err}");
        assert!(m.contains("git connector"), "{m}");
    }
}
