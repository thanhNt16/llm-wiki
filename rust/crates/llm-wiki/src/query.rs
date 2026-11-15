//! `query-prepare` — read-only query preparation (port of
//! `wikicore/query.py:prepare`, PRD §33-36). Returns matches + conflicts +
//! gaps for the agent to synthesize into an answer with citations — or to
//! refuse when evidence is insufficient (§35). This module NEVER writes.
//!
//! Two lanes (spec §10.2): a fresh `index.sqlite` serves the BM25 lane
//! (`ranker:"bm25"` — FTS5 MATCH recall over `claims_fts`, then the same
//! BM25-lite scoring as the canonical scan); a stale/missing/corrupt index
//! falls back to the canonical-scan legacy lane (`ranker:"legacy"`,
//! ftx.health contract). Recall in the BM25 lane is `claims_fts` ∪
//! alias/anchor recall — a provable superset of every positively-scored
//! claim — so the ordered `matches[]` are identical under either lane
//! (`query_ranking_parity` gate, spec §10.1).

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{json, Map, Value};

use crate::claims;
use crate::err::EngineError;
use crate::frontmatter::{self, PAGE_KINDS};
use crate::index::{self, Index};
use crate::store::Wiki;
use crate::synthesize::norm_tokens;

// BM25-lite retrieval (F1 paraphrase / F2 predicate-IDF): per-field
// Robertson IDF over the whole claim corpus down-weights corpus-common
// predicates like `status`, while the subject-alias index lets "stc-108" /
// "stc 108" reach claims filed under the `stc-108` subject.
const K1: f64 = 1.2;
const B: f64 = 0.75;
const W_SUBJECT: f64 = 3.0;
const W_PREDICATE: f64 = 2.0;
const W_VALUE: f64 = 1.0;
const MATCH_LIMIT: usize = 10;
// headroom over MATCH_LIMIT; widened pool feeds conflicts (PRD §34,
// spec §10.4 — units are claims)
const FTS_LIMIT: usize = 60;
const SUPERSEDED_LIMIT: usize = 5;

/// `contextpack.py:17-18` — STOPWORDS.
static STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from",
    "has", "have", "how", "i", "in", "is", "it", "its", "of", "on", "or",
    "that", "the", "this", "to", "was", "we", "what", "when", "where",
    "which", "who", "why", "will", "with", "you", "our", "us", "do", "does",
    "did",
];

fn is_stopword(t: &str) -> bool {
    STOPWORDS.contains(&t)
}

/// Python `str(v)` for the JSON values a claim field can hold: `None`,
/// `True`/`False`, bare strings, numbers; containers serialize as JSON.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// `_text(v)` — coerce a possibly-missing field to a tokenizable string.
fn text_of(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(other) => py_str(other),
    }
}

/// `_norm_list(s)` — norm_tokens with multiplicity preserved (term
/// frequencies). Splits on non-alnum, drops pieces <3 chars, then stems.
fn norm_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for piece in s.to_lowercase().split(|c: char| !c.is_ascii_alphanumeric()) {
        if piece.chars().count() < 3 {
            continue;
        }
        out.extend(norm_tokens(piece));
    }
    out
}

/// `_slug_tokens(s)` — raw subject-slug tokens without the norm length cut:
/// split parts plus the joined form (`stc-108` → {stc, 108, stc108}), so
/// digits and short parts that norm_tokens drops still participate in
/// alias matching.
fn slug_tokens(s: &str) -> BTreeSet<String> {
    let parts: Vec<String> = s
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect();
    let mut out: BTreeSet<String> = parts.iter().cloned().collect();
    if parts.len() > 1 {
        out.insert(parts.concat());
    }
    out
}

/// `_doc_tokens(s)` — subject-field token multiset: normalized tokens plus
/// raw slug tokens.
fn doc_tokens(s: &str) -> Vec<String> {
    let mut out = norm_list(s);
    out.extend(slug_tokens(s));
    out
}

/// `_query_alias_keys(question)` — alias lookup keys from the raw question:
/// every lowercase split part, each part's normalized form, and joined
/// adjacent pairs — so "stc-108", "stc108" and "stc 108" all reach the
/// `stc-108` subject alias. Stopwords never act as alias/anchor keys.
fn query_alias_keys(question: &str) -> HashSet<String> {
    let parts: Vec<String> = question
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect();
    let mut keys: HashSet<String> = parts.iter().cloned().collect();
    for (i, p) in parts.iter().enumerate() {
        keys.extend(norm_tokens(p));
        if i + 1 < parts.len() {
            keys.insert(format!("{}{}", parts[i], parts[i + 1]));
        }
    }
    keys.retain(|k| !is_stopword(k));
    keys
}

/// `_page_files(wiki)` — sorted wiki/<kind>/<slug>.md paths for the
/// title-alias index.
fn page_files(wiki: &Wiki) -> Vec<String> {
    let mut out = Vec::new();
    for kind in PAGE_KINDS {
        let dir = wiki.p(&format!("wiki/{kind}"));
        if !dir.is_dir() {
            continue;
        }
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".md") && name != "index.md" {
                    out.push(format!("wiki/{kind}/{name}"));
                }
            }
        }
    }
    out.sort();
    out
}

/// Per-claim field docs (subject/predicate/value token multisets).
struct FieldDocs {
    subject: Vec<String>,
    predicate: Vec<String>,
    value: Vec<String>,
}

/// `_build_index(all_claims)` — per-field token multisets, document
/// frequencies and average lengths. Every claim is indexed — the
/// as-of/terminal filters are applied after ranking — and malformed claims
/// contribute empty field docs.
struct CorpusIndex {
    docs: Vec<FieldDocs>,
    /// per-field: token -> document frequency
    df: [HashMap<String, usize>; 3],
    /// per-field average document length
    avgdl: [f64; 3],
    n_docs: usize,
}

const F_SUBJECT: usize = 0;
const F_PREDICATE: usize = 1;
const F_VALUE: usize = 2;

fn build_index(all_claims: &[Value]) -> CorpusIndex {
    let mut docs = Vec::with_capacity(all_claims.len());
    let mut df: [HashMap<String, usize>; 3] = Default::default();
    let mut dl_sum = [0usize; 3];
    for c in all_claims {
        let fields = FieldDocs {
            subject: doc_tokens(&text_of(c.get("subject"))),
            predicate: norm_list(&text_of(c.get("predicate"))),
            value: norm_list(&text_of(c.get("value"))),
        };
        for (f, toks) in [
            (F_SUBJECT, &fields.subject),
            (F_PREDICATE, &fields.predicate),
            (F_VALUE, &fields.value),
        ] {
            dl_sum[f] += toks.len();
            for t in toks.iter().collect::<HashSet<_>>() {
                *df[f].entry(t.clone()).or_insert(0) += 1;
            }
        }
        docs.push(fields);
    }
    let n = all_claims.len() as f64;
    let avgdl = [
        if n > 0.0 { dl_sum[0] as f64 / n } else { 0.0 },
        if n > 0.0 { dl_sum[1] as f64 / n } else { 0.0 },
        if n > 0.0 { dl_sum[2] as f64 / n } else { 0.0 },
    ];
    CorpusIndex {
        docs,
        df,
        avgdl,
        n_docs: all_claims.len(),
    }
}

/// `_idf(df_t, n_docs)` — Robertson IDF: positive even for terms seen in
/// every document.
fn idf(df_t: usize, n_docs: usize) -> f64 {
    ((n_docs as f64 - df_t as f64 + 0.5) / (df_t as f64 + 0.5) + 1.0).ln()
}

/// `_field_score(q_terms, idf, counts, dl, avgdl)` — weighted-within-field
/// BM25 sum over the query terms present in one field document.
fn field_score(
    q_terms: &BTreeSet<String>,
    idf_map: &HashMap<String, f64>,
    counts: &HashMap<String, usize>,
    dl: usize,
    avgdl: f64,
) -> f64 {
    let k = if avgdl != 0.0 {
        K1 * (1.0 - B + B * dl as f64 / avgdl)
    } else {
        K1
    };
    let mut total = 0.0;
    for t in q_terms {
        if let Some(&tf) = counts.get(t) {
            if tf > 0 {
                total += idf_map[t] * (tf as f64 * (K1 + 1.0)) / (tf as f64 + k);
            }
        }
    }
    total
}

/// Alias-expansion result plus the recall maps it built (`alias`,
/// `anchor`) — the BM25 lane reuses them for candidate recall so its match
/// set is a superset of the legacy lane's positives.
struct Expansion {
    terms: HashSet<String>,
    warnings: Vec<String>,
    /// slug/norm token -> subject strings using it (subject-field tokens)
    alias: HashMap<String, HashSet<String>>,
    /// pred/value token -> subject strings using it (non-stopword)
    anchor: HashMap<String, HashSet<String>>,
}

/// `_expand_subject_terms` — F1 subject-alias expansion: tokens added to
/// the subject-field query only. Three deterministic sources: slug
/// aliases, content anchors, and page titles.
fn expand_subject_terms(
    wiki: &Wiki,
    question: &str,
    q_tokens: &HashSet<String>,
    claims_by_id: &HashMap<String, usize>,
    all_claims: &[Value],
    docs: &[FieldDocs],
) -> Expansion {
    let mut warnings = Vec::new();
    let mut alias: HashMap<String, HashSet<String>> = HashMap::new();
    let mut anchor: HashMap<String, HashSet<String>> = HashMap::new();
    for (i, c) in all_claims.iter().enumerate() {
        let subj = text_of(c.get("subject"));
        if subj.is_empty() {
            continue;
        }
        for key in slug_tokens(&subj).iter().cloned().collect::<Vec<_>>() {
            alias.entry(key).or_default().insert(subj.clone());
        }
        for key in norm_tokens(&subj) {
            alias.entry(key).or_default().insert(subj.clone());
        }
        let mut pv: HashSet<String> = HashSet::new();
        pv.extend(docs[i].predicate.iter().cloned());
        pv.extend(docs[i].value.iter().cloned());
        for t in pv {
            if !is_stopword(&t) {
                anchor.entry(t).or_default().insert(subj.clone());
            }
        }
    }

    let q_keys = query_alias_keys(question);
    let mut expanded: HashSet<String> = HashSet::new();
    for key in &q_keys {
        if let Some(hit) = alias.get(key) {
            if hit.len() == 1 {
                expanded.insert(key.clone());
                continue; // name match wins — suppress content anchor
            }
        }
        if let Some(ahit) = anchor.get(key) {
            if ahit.len() == 1 {
                expanded.extend(doc_tokens(ahit.iter().next().unwrap()));
            }
        }
    }

    for rel in page_files(wiki) {
        let front = match std::fs::read_to_string(wiki.p(&rel)) {
            Ok(text) => match frontmatter::split_front(&text) {
                Ok((f, _body)) => f,
                Err(e) => {
                    warnings.push(format!("alias index skipped {rel}: {e}"));
                    continue;
                }
            },
            Err(e) => {
                warnings.push(format!("alias index skipped {rel}: {e}"));
                continue;
            }
        };
        let title = front.get("title").cloned().unwrap_or(Value::Null);
        if title.is_null() {
            continue;
        }
        let title_toks: HashSet<String> = norm_tokens(&py_str(&title))
            .into_iter()
            .filter(|t| !is_stopword(t))
            .collect();
        let hit_tokens: HashSet<&String> = title_toks
            .iter()
            .filter(|t| q_keys.contains(*t))
            .collect();
        // require the WHOLE title phrase to be echoed by the question —
        // one shared generic token must not bridge an unrelated page's
        // deps into the query (retired-rollout-plan bleed, query.py:180)
        if title_toks.is_empty() || title_toks.len() != hit_tokens.len() {
            continue;
        }
        if !hit_tokens
            .iter()
            .any(|t| anchor.get(*t).map(|s| s.len()).unwrap_or(0) <= 1)
        {
            continue;
        }
        let dep_list = match front.get("deps") {
            Some(Value::Array(l)) => l,
            _ => continue,
        };
        for dep in dep_list {
            let did = py_str(dep).split('@').next().unwrap_or("").to_string();
            if let Some(&ci) = claims_by_id.get(&did) {
                expanded.extend(doc_tokens(&text_of(
                    all_claims[ci].get("subject"),
                )));
            }
        }
    }
    for t in q_tokens {
        expanded.remove(t);
    }
    Expansion {
        terms: expanded,
        warnings,
        alias,
        anchor,
    }
}

/// Python `time.strftime("%Y-%m-%d", time.gmtime())` — today's UTC date.
fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

/// Hinnant civil-from-days (same algorithm as claims::date_minus_one).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `ftx.search` MATCH expression — double-quoted OR-joined query tokens
/// (recall-first, bm25 orders). Empty → None, exactly like Python.
fn fts_match_query(question: &str) -> Option<String> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    for ch in question.to_lowercase().chars() {
        if ch.is_alphanumeric() {
            cur.push(ch);
        } else if !cur.is_empty() {
            toks.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        toks.push(cur);
    }
    toks.retain(|t| !is_stopword(t));
    if toks.is_empty() {
        return None;
    }
    Some(
        toks.iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(" OR "),
    )
}

/// `ftx.search` — best-first `(claim_id, bm25)` recall over `claims_fts`,
/// or None when the index can't answer (empty query / MATCH error → the
/// caller's alias/anchor recall still covers every BM25-lite positive).
fn fts_recall(idx: &Index, question: &str, limit: usize) -> HashSet<String> {
    let Some(matcher) = fts_match_query(question) else {
        return HashSet::new();
    };
    let mut stmt = match idx.conn().prepare(
        "SELECT claim_id, bm25(claims_fts) FROM claims_fts \
         WHERE claims_fts MATCH ?1 ORDER BY bm25(claims_fts) LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return HashSet::new(),
    };
    let rows = stmt.query_map(rusqlite::params![matcher, limit as i64], |r| {
        r.get::<_, String>(0)
    });
    match rows {
        Ok(mapped) => mapped.flatten().collect(),
        Err(_) => HashSet::new(),
    }
}

/// Python truthiness for the claim fields `prepare` copies verbatim
/// (`scope`/`evidence`/`superseded_by` are emitted only when truthy).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// A scored match row — `parts` rides along to fill `score_breakdown`.
struct Scored {
    entry: Value,
    score: f64,
}

/// `query.prepare(wiki, question, as_of)` — the full payload. `ranker` is
/// the additive Rust-only key naming the serving lane (spec §10.2); the
/// minimal contractual set is unchanged (query.py:377-385).
pub fn prepare(
    wiki: &Wiki,
    question: &str,
    as_of: Option<&str>,
) -> Result<Value, EngineError> {
    let q_tokens: HashSet<String> = norm_tokens(question)
        .into_iter()
        .filter(|t| !is_stopword(t))
        .collect();
    // `if as_of:` — an empty string is falsy and stays on the current
    // lane, but the payload echoes the raw argument (query.py:381).
    let as_of_raw = as_of;
    let as_of = as_of.filter(|a| !a.is_empty());
    let all_claims = claims::list(wiki)?;
    // id -> subject raw value (review-item sibling check), id -> index
    let mut subject_of: Vec<Value> = Vec::with_capacity(all_claims.len());
    let mut claims_by_id: HashMap<String, usize> = HashMap::new();
    for (i, c) in all_claims.iter().enumerate() {
        subject_of.push(c.get("subject").cloned().unwrap_or(Value::Null));
        if let Some(id) = c.get("id").and_then(|v| v.as_str()) {
            claims_by_id.insert(id.to_string(), i);
        }
    }
    // subject string -> claim indexes (alias/anchor recall expansion)
    let mut by_subject: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, c) in all_claims.iter().enumerate() {
        let subj = text_of(c.get("subject"));
        if !subj.is_empty() {
            by_subject.entry(subj).or_default().push(i);
        }
    }

    let corpus = build_index(&all_claims);
    let expansion = expand_subject_terms(
        wiki,
        question,
        &q_tokens,
        &claims_by_id,
        &all_claims,
        &corpus.docs,
    );
    let warnings = expansion.warnings;
    let mut q_subject: BTreeSet<String> = q_tokens.iter().cloned().collect();
    q_subject.extend(expansion.terms.iter().cloned());
    let q_other: BTreeSet<String> = q_tokens.iter().cloned().collect();

    // Lane selection (spec §10.2 / ftx.health): a fresh watermark serves
    // BM25 over sqlite FTS; absent/stale/corrupt falls to the canonical
    // scan. ftx.search parity: an empty MATCH query also reports legacy.
    let ranker = match index::staleness_probe(wiki) {
        Ok(Some(built)) if built == wiki.revision().unwrap_or(-2) => {
            if fts_match_query(question).is_some() {
                "bm25"
            } else {
                "legacy"
            }
        }
        _ => "legacy",
    };

    // Candidate set. Legacy lane scores every claim. The BM25 lane scores
    // claims_fts recall ∪ alias recall (subject-token reach) ∪ anchor
    // recall (predicate/value-token reach) — alias/anchor indexes cover
    // every token a BM25-lite-positive claim can share with the query,
    // so the match set is provably identical to the full scan.
    let candidates: Option<HashSet<usize>> = if ranker == "bm25" {
        let mut cand: HashSet<usize> = HashSet::new();
        match Index::open_or_rebuild(wiki) {
            Ok(idx) => {
                cand.extend(
                    fts_recall(&idx, question, FTS_LIMIT)
                        .into_iter()
                        .filter_map(|id| claims_by_id.get(&id).copied()),
                );
            }
            Err(_) => {}
        }
        // subject-field reach: every expanded/query subject term
        for t in &q_subject {
            if let Some(subjects) = expansion.alias.get(t) {
                for s in subjects {
                    if let Some(idxs) = by_subject.get(s) {
                        cand.extend(idxs.iter().copied());
                    }
                }
            }
        }
        // predicate/value-field reach: every query term
        for t in &q_other {
            if let Some(subjects) = expansion.anchor.get(t) {
                for s in subjects {
                    if let Some(idxs) = by_subject.get(s) {
                        cand.extend(idxs.iter().copied());
                    }
                }
            }
        }
        Some(cand)
    } else {
        None
    };

    // Per-field idf tables (query.py:207-208).
    let idf_maps: [HashMap<String, f64>; 3] = [
        corpus.df[0]
            .iter()
            .map(|(t, &n)| (t.clone(), idf(n, corpus.n_docs)))
            .collect(),
        corpus.df[1]
            .iter()
            .map(|(t, &n)| (t.clone(), idf(n, corpus.n_docs)))
            .collect(),
        corpus.df[2]
            .iter()
            .map(|(t, &n)| (t.clone(), idf(n, corpus.n_docs)))
            .collect(),
    ];

    let today = today_utc();
    let as_of10: String = as_of.map(|a| a.chars().take(10).collect()).unwrap_or_default();
    let mut scored: Vec<Scored> = Vec::new();
    let mut excluded: Vec<Value> = Vec::new();
    let mut conflicts: Vec<Value> = Vec::new();
    for (idx, c) in all_claims.iter().enumerate() {
        if let Some(cand) = &candidates {
            if !cand.contains(&idx) {
                continue;
            }
        }
        let doc = &corpus.docs[idx];
        let mut counts: [HashMap<String, usize>; 3] = Default::default();
        for (f, toks) in [
            (F_SUBJECT, &doc.subject),
            (F_PREDICATE, &doc.predicate),
            (F_VALUE, &doc.value),
        ] {
            for t in toks {
                *counts[f].entry(t.clone()).or_insert(0) += 1;
            }
        }
        let p_subj = W_SUBJECT
            * field_score(
                &q_subject,
                &idf_maps[F_SUBJECT],
                &counts[F_SUBJECT],
                doc.subject.len(),
                corpus.avgdl[F_SUBJECT],
            );
        let p_pred = W_PREDICATE
            * field_score(
                &q_other,
                &idf_maps[F_PREDICATE],
                &counts[F_PREDICATE],
                doc.predicate.len(),
                corpus.avgdl[F_PREDICATE],
            );
        let p_val = W_VALUE
            * field_score(
                &q_other,
                &idf_maps[F_VALUE],
                &counts[F_VALUE],
                doc.value.len(),
                corpus.avgdl[F_VALUE],
            );
        let score = p_subj + p_pred + p_val;
        if score <= 0.0 {
            continue;
        }

        let valid_from = c.get("valid_from").cloned().unwrap_or(Value::Null);
        let valid_to = c.get("valid_to").cloned().unwrap_or(Value::Null);
        if let Some(ao) = as_of {
            // historical: the claim's validity window must cover that
            // date; string comparison is the convention (query.py:230).
            if truthy(&valid_from) && py_str(&valid_from) > ao.to_string() {
                continue;
            }
            if truthy(&valid_to) && py_str(&valid_to) < ao.to_string() {
                continue;
            }
            // Terminal claims count only when they were still live on
            // that date; the exact supersession moment is approximated by
            // updated_at, falling back to recorded_at (query.py:238-247).
            if claims::TERMINAL.contains(&text_of(c.get("status")).as_str()) {
                let ended = match c.get("updated_at") {
                    Some(v) if truthy(v) => Some(py_str(v)),
                    _ => c.get("recorded_at").filter(|v| truthy(v)).map(py_str),
                };
                if let Some(ended) = ended {
                    let ended10: String = ended.chars().take(10).collect();
                    if ended10 <= as_of10 {
                        continue;
                    }
                }
            }
        } else {
            // current: only still-valid knowledge; token-matched terminal
            // claims surface separately as `superseded`.
            let status = text_of(c.get("status"));
            if status == "superseded" || status == "rejected" {
                excluded.push(json!({
                    "claim_id": c.get("id").cloned().unwrap_or(Value::Null),
                    "subject": c.get("subject").cloned().unwrap_or(Value::Null),
                    "predicate": c.get("predicate").cloned().unwrap_or(Value::Null),
                    "value": c.get("value").cloned().unwrap_or(Value::Null),
                    "status": c.get("status").cloned().unwrap_or(Value::Null),
                    "replaced_by": c.get("superseded_by").cloned().unwrap_or(Value::Null),
                }));
                continue;
            }
            // valid_to is inclusive: only a date strictly before today
            // ends validity.
            if truthy(&valid_to) && py_str(&valid_to) < today {
                continue;
            }
        }

        let scope = match c.get("scope") {
            Some(v) if truthy(v) => v.clone(),
            _ => json!({}),
        };
        let evidence = match c.get("evidence") {
            Some(v) => v.clone(),
            None => json!([]),
        };
        let mut entry = Map::new();
        entry.insert(
            "claim_id".into(),
            c.get("id").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "subject".into(),
            c.get("subject").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "predicate".into(),
            c.get("predicate").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "value".into(),
            c.get("value").cloned().unwrap_or(Value::Null),
        );
        entry.insert(
            "unit".into(),
            c.get("unit").cloned().unwrap_or(Value::Null),
        );
        entry.insert("scope".into(), scope);
        entry.insert(
            "status".into(),
            c.get("status").cloned().unwrap_or(Value::Null),
        );
        entry.insert("valid_from".into(), valid_from.clone());
        entry.insert("valid_to".into(), valid_to.clone());
        entry.insert(
            "authority".into(),
            c.get("authority").cloned().unwrap_or(Value::Null),
        );
        entry.insert("evidence".into(), evidence);
        entry.insert(
            "score".into(),
            serde_json::Number::from_f64(score)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
        entry.insert(
            "score_breakdown".into(),
            json!({
                "subject": p_subj,
                "predicate": p_pred,
                "value": p_val,
                "total": score,
            }),
        );
        if let Some(sb) = c.get("superseded_by") {
            if truthy(sb) {
                entry.insert("superseded_by".into(), sb.clone());
            }
        }

        if text_of(c.get("status")) == "disputed" {
            conflicts.push(json!({
                "claim_id": c.get("id").cloned().unwrap_or(Value::Null),
                "reason": "disputed claim matches the question",
            }));
        }
        scored.push(Scored {
            entry: Value::Object(entry),
            score,
        });
    }

    // Deterministic content tie-break: claim ids are random ULIDs, so an
    // id-only key would reorder ties per corpus build (query.py:292-295).
    scored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| {
                text_of(a.entry.get("subject"))
                    .cmp(&text_of(b.entry.get("subject")))
            })
            .then_with(|| {
                text_of(a.entry.get("predicate"))
                    .cmp(&text_of(b.entry.get("predicate")))
            })
            .then_with(|| {
                text_of(a.entry.get("claim_id"))
                    .cmp(&text_of(b.entry.get("claim_id")))
            })
    });
    let mut ranked: Vec<Scored> = scored;
    // precision floor: drop tail matches below 40% of the top score — the
    // first match always survives (it is the max), so this only trims
    // noise, never empties a result that found anything.
    if let Some(top) = ranked.first() {
        let floor = top.score * 0.4;
        ranked.retain(|m| m.score >= floor);
    }
    // Contradiction pool (spec §10.4): the widened _FTS_LIMIT=60 window in
    // claim units — a conflicting claim ranked past the display window is
    // precisely the knowledge the caller needs (PRD §34).
    let pool: Vec<&Scored> = ranked.iter().take(FTS_LIMIT).collect();
    let matches: Vec<Value> = ranked
        .iter()
        .take(MATCH_LIMIT)
        .map(|s| s.entry.clone())
        .collect();

    // Unresolved review items touching the matched claims (C6): an item
    // surfaces when affected claim ids match, when affected claims are
    // siblings of a matched claim (same subject), or — only as a
    // fallback — when its problem text overlaps the question tokens.
    let open_items = wiki
        .read_jsonl(".state/review-queue.jsonl")?
        .into_iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("open"))
        .collect::<Vec<_>>();
    let matched_ids: HashSet<String> = matches
        .iter()
        .filter_map(|m| m.get("claim_id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    // raw subject values — Python compares with `in` (loose equality),
    // so a non-string subject still hits on exact Value equality.
    let matched_subjects: Vec<Value> = matches
        .iter()
        .map(|m| m.get("subject").cloned().unwrap_or(Value::Null))
        .collect();
    for item in &open_items {
        let mut hit = false;
        if let Some(affected) = item.get("affected").and_then(Value::as_array) {
            for a in affected {
                let cid = text_of(Some(a));
                let cid = cid.split('@').next().unwrap_or("").to_string();
                let subj = claims_by_id
                    .get(&cid)
                    .map(|&i| subject_of[i].clone());
                if matched_ids.contains(&cid)
                    || subj.map(|s| matched_subjects.contains(&s)).unwrap_or(false)
                {
                    hit = true;
                    break;
                }
            }
        }
        if !hit {
            let problem = item.get("problem").map(py_str).unwrap_or_default();
            if !norm_tokens(&problem)
                .intersection(&q_tokens)
                .collect::<Vec<_>>()
                .is_empty()
            {
                hit = true;
            }
        }
        if hit {
            conflicts.push(json!({
                "review_id": item.get("id").cloned().unwrap_or(Value::Null),
                "reason": item.get("problem").cloned().unwrap_or(Value::Null),
            }));
        }
    }

    // Accepted claims that contradict each other (same
    // subject+predicate+scope, different values, overlapping validity)
    // must surface as conflicts — over the widened pool (PRD §34).
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    for (i, m1) in pool.iter().enumerate() {
        for m2 in pool[i + 1..].iter() {
            let v1 = m1.entry.get("value").cloned().unwrap_or(Value::Null);
            let v2 = m2.entry.get("value").cloned().unwrap_or(Value::Null);
            if v1 == v2 {
                continue;
            }
            let id1 = text_of(m1.entry.get("claim_id"));
            let id2 = text_of(m2.entry.get("claim_id"));
            let key = (id1.clone(), id2.clone());
            if seen_pairs.contains(&key) || seen_pairs.contains(&(id2, id1)) {
                continue;
            }
            let same_sp = m1.entry.get("subject") == m2.entry.get("subject")
                && m1.entry.get("predicate") == m2.entry.get("predicate");
            if !same_sp {
                continue;
            }
            if m1.entry.get("scope") != m2.entry.get("scope") {
                continue; // different scope is NOT a contradiction (PRD §6)
            }
            let vf1 = m1.entry.get("valid_from").cloned().unwrap_or(Value::Null);
            let vt1 = m1.entry.get("valid_to").cloned().unwrap_or(Value::Null);
            let vf2 = m2.entry.get("valid_from").cloned().unwrap_or(Value::Null);
            let vt2 = m2.entry.get("valid_to").cloned().unwrap_or(Value::Null);
            if truthy(&vf1) && truthy(&vt2) && py_str(&vf1) > py_str(&vt2) {
                continue;
            }
            if truthy(&vf2) && truthy(&vt1) && py_str(&vf2) > py_str(&vt1) {
                continue;
            }
            seen_pairs.insert(key);
            conflicts.push(json!({
                "claims": [
                    m1.entry.get("claim_id").cloned().unwrap_or(Value::Null),
                    m2.entry.get("claim_id").cloned().unwrap_or(Value::Null),
                ],
                "reason": "same subject/predicate/scope with different values",
            }));
        }
    }

    // Knowledge gaps: content words with no coverage anywhere — but only
    // an honest gap signal: token gaps require a weak top match (zero
    // field overlap, or no predicate overlap — SP3-T1 #18 / SP3-F6).
    let mut known: HashSet<String> = HashSet::new();
    for c in &all_claims {
        known.extend(norm_tokens(&format!(
            "{} {} {}",
            py_str(&c.get("subject").cloned().unwrap_or(Value::Null)),
            py_str(&c.get("predicate").cloned().unwrap_or(Value::Null)),
            py_str(&c.get("value").cloned().unwrap_or(Value::Null)),
        )));
    }
    let weak = if let Some(top) = matches.first() {
        let top_overlap = norm_tokens(&format!(
            "{} {} {}",
            py_str(&top.get("subject").cloned().unwrap_or(Value::Null)),
            py_str(&top.get("predicate").cloned().unwrap_or(Value::Null)),
            py_str(&top.get("value").cloned().unwrap_or(Value::Null)),
        ))
        .intersection(&q_tokens)
        .count();
        let pred_zero = top
            .get("score_breakdown")
            .and_then(|b| b.get("predicate"))
            .and_then(Value::as_f64)
            .map(|v| v == 0.0)
            .unwrap_or(false);
        top_overlap == 0 || pred_zero
    } else {
        true
    };
    let mut gaps: Vec<String> = if weak {
        let mut g: Vec<String> = q_tokens
            .iter()
            .filter(|t| !known.contains(*t))
            .cloned()
            .collect();
        g.sort();
        g
    } else {
        Vec::new()
    };
    if !excluded.is_empty() && matches.is_empty() {
        gaps.push("stale_knowledge".to_string());
    }

    // decisions: first 5 canonical decision ids (query.py:384)
    let decisions: Vec<Value> = claims::list_decisions(wiki)?
        .iter()
        .take(5)
        .map(|d| d.get("id").cloned().unwrap_or(Value::Null))
        .collect();

    Ok(json!({
        "question": question,
        "as_of": as_of_raw,
        "matches": matches,
        "superseded": excluded.into_iter().take(SUPERSEDED_LIMIT).collect::<Vec<_>>(),
        "decisions": decisions,
        "conflicts": conflicts,
        "gaps": gaps,
        "warnings": warnings,
        // additive Rust-only key (spec §6.1/§10.2) — which lane served.
        "ranker": ranker,
    }))
}
