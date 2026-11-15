//! Human review queue — `.state/review-queue.jsonl` (review.py port).
//! Semantic uncertainty lands here instead of being hidden (PRD §25-26).
//! Actions are ordinary transactions — nothing special-cased.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{json, Value};

use crate::claims;
use crate::deps as deps_mod;
use crate::err::EngineError;
use crate::schema::{self, py_repr_str};
use crate::store::Wiki;
use crate::topics as topics_mod;
use crate::txn::{Txn, TxnOpts};
use crate::util;

/// review.py:15
pub const QUEUE: &str = ".state/review-queue.jsonl";

/// review.py:31-35 — every dispatchable action.
pub const ACTIONS: &[&str] = &[
    "accept",
    "reject",
    "merge",
    "mark_duplicate",
    "mark_authoritative",
    "mark_superseded",
    "set_scope",
    "set_validity",
    "defer",
    "reopen",
    "set_topic_kind",
];

/// `review.add_item` (review.py:38) — schema-validated queue append
/// staged inside `txn`. Peers (compile/dream/synthesize) call this shape:
/// `{id: review_<ulid>, kind, problem, evidence[], affected[], status:
/// "open", risk, proposed_resolution, created_at}`.
pub fn add_item(
    txn: &mut Txn,
    kind: &str,
    problem: &str,
    evidence: Vec<Value>,
    affected: Vec<Value>,
    risk: &str,
    proposed: &str,
) -> Result<String, EngineError> {
    let item = json!({
        "id": crate::ids::new("review"),
        "created_at": util::now_iso(),
        "kind": kind,
        "status": "open",
        "problem": problem,
        "evidence": evidence,
        "affected": affected,
        "risk": risk,
        "proposed_resolution": proposed,
    });
    let schema_v: Value = serde_json::from_str(schema::REVIEW_ITEM_SCHEMA)?;
    let errors = schema::validate(&item, &schema_v);
    if !errors.is_empty() {
        return Err(EngineError::Txn(format!(
            "review item invalid: {}",
            errors.join("; ")
        )));
    }
    let mut queue = txn_queue(txn)?;
    let id = item["id"].as_str().unwrap_or("").to_string();
    queue.push(item);
    txn.stage_write(QUEUE, dump(&queue).as_bytes())?;
    Ok(id)
}

/// `review._dump` (review.py:60) — one `json.dumps` line per item.
fn dump(queue: &[Value]) -> String {
    let mut out = String::new();
    for x in queue {
        out.push_str(&util::py_json_dumps(x));
        out.push('\n');
    }
    out
}

/// Raw queue read for read-only consumers (pages/graph): all items,
/// unfiltered — `wiki.read_jsonl(QUEUE)` parity.
pub fn read_queue(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
    wiki.read_jsonl(QUEUE)
}

/// `review._txn_queue` (review.py:64) — staged write to QUEUE wins over
/// the on-disk file so multiple `add_item` calls compose.
fn txn_queue(txn: &Txn) -> Result<Vec<Value>, EngineError> {
    let staged = txn.staging_path(QUEUE);
    let text = match std::fs::read_to_string(&staged) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::read_to_string(txn.wiki.p(QUEUE)) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
                Err(e) => return Err(e.into()),
            }
        }
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if !line.is_empty() {
            out.push(serde_json::from_str(line)?);
        }
    }
    Ok(out)
}

/// `review.list_items` (review.py:79) — `status=None` lifts the status
/// filter entirely (`review list --all`).
pub fn list_items(
    wiki: &Wiki,
    kind: Option<&str>,
    status: Option<&str>,
) -> Result<Vec<Value>, EngineError> {
    let items = wiki.read_jsonl(QUEUE)?;
    let mut out = Vec::new();
    for item in items {
        if let Some(k) = kind {
            if item.get("kind").and_then(Value::as_str) != Some(k) {
                continue;
            }
        }
        if let Some(s) = status {
            if item.get("status").and_then(Value::as_str) != Some(s) {
                continue;
            }
        }
        out.push(item);
    }
    Ok(out)
}

/// `review.show` (review.py:91).
pub fn show(wiki: &Wiki, item_id: &str) -> Result<Value, EngineError> {
    for item in wiki.read_jsonl(QUEUE)? {
        if item.get("id").and_then(Value::as_str) == Some(item_id) {
            return Ok(item);
        }
    }
    Err(EngineError::Txn(format!(
        "unknown review item: {item_id}"
    )))
}

/// `review.act` (review.py:98) — one action, one commit; receipt carries
/// the mutated item.
pub fn act(
    wiki: &Wiki,
    item_id: &str,
    action: &str,
    params: &Value,
    base_revision: i64,
) -> Result<Value, EngineError> {
    let mut txn = Txn::begin_with(
        wiki,
        &crate::ids::new_run(),
        TxnOpts {
            command: Some("wiki-review".to_string()),
            ..Default::default()
        },
    )?;
    txn.base_revision = base_revision;
    let mut queue = wiki.read_jsonl(QUEUE)?;
    let mut cache = HashMap::new();
    let item = act_in_txn(wiki, &mut txn, &mut queue, item_id, action, params, &mut cache)?;
    txn.changes.insert("review_actions".to_string(), json!(1));
    let mut receipt = txn.commit()?;
    receipt["item"] = item;
    Ok(receipt)
}

/// `review._act_in_txn` (review.py:108) — apply one action against an
/// in-memory queue inside an open txn; stages the updated queue.
/// `claim_cache` lets batch actions see each other's mutations on the
/// same claim (live disk is stale until commit).
fn act_in_txn(
    wiki: &Wiki,
    txn: &mut Txn,
    queue: &mut Vec<Value>,
    item_id: &str,
    action: &str,
    params: &Value,
    claim_cache: &mut HashMap<String, Value>,
) -> Result<Value, EngineError> {
    let load = |cache: &mut HashMap<String, Value>, cid: &str| -> Result<Value, EngineError> {
        if let Some(c) = cache.get(cid) {
            return Ok(c.clone());
        }
        let c = claims::load_claim(wiki, cid)?;
        cache.insert(cid.to_string(), c.clone());
        Ok(c)
    };

    if !ACTIONS.contains(&action) {
        return Err(EngineError::Txn(format!(
            "unknown review action: {} (valid: {})",
            py_repr_str(action),
            ACTIONS.join(", ")
        )));
    }
    let params = match params {
        Value::Null => json!({}),
        other => other.clone(),
    };
    let idx = queue
        .iter()
        .position(|i| i.get("id").and_then(Value::as_str) == Some(item_id));
    let Some(idx) = idx else {
        return Err(EngineError::Txn(format!("unknown review item: {item_id}")));
    };
    let status = queue[idx].get("status").and_then(Value::as_str).unwrap_or("");
    if status == "resolved" && action != "reopen" {
        return Err(EngineError::Txn(format!(
            "review item {item_id} is already resolved — reopen it first"
        )));
    }
    let mut claim: Option<Value> = None;
    let mut changed_claim_ids: Vec<String> = Vec::new();
    if !matches!(action, "defer" | "reopen" | "set_topic_kind") {
        let cid = params.get("claim_id").and_then(Value::as_str);
        let Some(cid) = cid else {
            return Err(EngineError::Txn(format!(
                "action {} requires claim_id",
                py_repr_str(action)
            )));
        };
        claim = Some(load(claim_cache, cid)?);
    }

    let note = params.get("note").and_then(Value::as_str).unwrap_or("");

    match action {
        "accept" => {
            let mut c = claim.take().unwrap();
            c["status"] = json!("accepted");
            c["acceptance"] = json!({"reason": "human_review", "accepted_at": util::now_iso()});
            claims::save_claim(wiki, txn, &mut c)?;
            let cid = c["id"].as_str().unwrap_or("").to_string();
            claim_cache.insert(cid.clone(), c);
            changed_claim_ids.push(cid);
        }
        "reject" => {
            let mut c = claim.take().unwrap();
            c["status"] = json!("rejected");
            c["acceptance"] = json!({"reason": "human_review", "accepted_at": util::now_iso()});
            claims::save_claim(wiki, txn, &mut c)?;
            let cid = c["id"].as_str().unwrap_or("").to_string();
            claim_cache.insert(cid.clone(), c);
            changed_claim_ids.push(cid);
        }
        "merge" | "mark_duplicate" => {
            let merge_into = params.get("merge_into").and_then(Value::as_str);
            let Some(merge_into) = merge_into else {
                return Err(EngineError::Txn(format!(
                    "action {} requires merge_into",
                    py_repr_str(action)
                )));
            };
            let c = claim.take().unwrap();
            if merge_into == c["id"].as_str().unwrap_or("") {
                return Err(EngineError::Txn(format!(
                    "action {}: cannot merge a claim into itself",
                    py_repr_str(action)
                )));
            }
            let mut target = load(claim_cache, merge_into)?;
            claims::append_evidence(&mut target, &c);
            claims::save_claim(wiki, txn, &mut target)?;
            let tid = target["id"].as_str().unwrap_or("").to_string();
            claim_cache.insert(tid.clone(), target);
            changed_claim_ids.push(tid);
            let mut c = c;
            c["status"] = json!("rejected");
            c["acceptance"] = json!({
                "reason": format!("duplicate_of:{merge_into}"),
                "accepted_at": util::now_iso(),
            });
            claims::save_claim(wiki, txn, &mut c)?;
            let cid = c["id"].as_str().unwrap_or("").to_string();
            claim_cache.insert(cid.clone(), c);
            changed_claim_ids.push(cid);
        }
        "mark_authoritative" => {
            let mut c = claim.take().unwrap();
            c["authority"] = json!({
                "type": "explicit_project_decision",
                "source": format!("human_review:{item_id}"),
            });
            if c["status"].as_str() != Some("accepted") {
                c["status"] = json!("accepted");
                c["acceptance"] =
                    json!({"reason": "human_review", "accepted_at": util::now_iso()});
            }
            claims::save_claim(wiki, txn, &mut c)?;
            let cid = c["id"].as_str().unwrap_or("").to_string();
            claim_cache.insert(cid.clone(), c);
            changed_claim_ids.push(cid);
        }
        "mark_superseded" => {
            let target_id = params.get("target").and_then(Value::as_str);
            let Some(target_id) = target_id else {
                return Err(EngineError::Txn(
                    "mark_superseded requires target claim_id".to_string(),
                ));
            };
            let c = claim.take().unwrap();
            if target_id == c["id"].as_str().unwrap_or("") {
                return Err(EngineError::Txn(
                    "mark_superseded: cannot supersede a claim with itself".to_string(),
                ));
            }
            let mut survivor = load(claim_cache, target_id)?;
            // `claim` is the superseded one: gets superseded_by; the
            // survivor records claim.id in its supersedes list.
            let mut c = c;
            claims::supersede(&mut c, target_id, false, None);
            claims::save_claim(wiki, txn, &mut c)?;
            let cid = c["id"].as_str().unwrap_or("").to_string();
            claim_cache.insert(cid.clone(), c);
            changed_claim_ids.push(cid.clone());
            let sup = survivor
                .get("supersedes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !sup.iter().any(|v| v.as_str() == Some(cid.as_str())) {
                if !survivor["supersedes"].is_array() {
                    survivor["supersedes"] = json!([]);
                }
                survivor["supersedes"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(cid));
            }
            claims::save_claim(wiki, txn, &mut survivor)?;
            let sid = survivor["id"].as_str().unwrap_or("").to_string();
            claim_cache.insert(sid.clone(), survivor);
            changed_claim_ids.push(sid);
        }
        "set_scope" => {
            let scope = params.get("scope").cloned().unwrap_or(Value::Null);
            if !scope.is_object() {
                return Err(EngineError::Txn("set_scope requires scope dict".to_string()));
            }
            let mut c = claim.take().unwrap();
            c["scope"] = scope;
            claims::save_claim(wiki, txn, &mut c)?;
            let cid = c["id"].as_str().unwrap_or("").to_string();
            claim_cache.insert(cid.clone(), c);
            changed_claim_ids.push(cid);
        }
        "set_validity" => {
            let mut c = claim.take().unwrap();
            // review.py:190-191 — params.get(k, claim.get(k)): key-presence
            // semantics; an explicit null clears the field.
            c["valid_from"] = match params.get("valid_from") {
                Some(v) => v.clone(),
                None => c.get("valid_from").cloned().unwrap_or(Value::Null),
            };
            c["valid_to"] = match params.get("valid_to") {
                Some(v) => v.clone(),
                None => c.get("valid_to").cloned().unwrap_or(Value::Null),
            };
            claims::save_claim(wiki, txn, &mut c)?;
            let cid = c["id"].as_str().unwrap_or("").to_string();
            claim_cache.insert(cid.clone(), c);
            changed_claim_ids.push(cid);
        }
        "set_topic_kind" => {
            if queue[idx].get("kind").and_then(Value::as_str)
                != Some("topic_kind_conflict")
            {
                return Err(EngineError::Txn(
                    "set_topic_kind requires a topic_kind_conflict item".to_string(),
                ));
            }
            let subj = params
                .get("subject")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let new_kind = params
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_lowercase();
            let affected: Vec<String> = queue[idx]
                .get("affected")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if !affected.iter().any(|s| s == &subj) {
                return Err(EngineError::Txn(format!(
                    "subject {} not affected by item {item_id}",
                    py_repr_str(&subj)
                )));
            }
            let entry = topics_mod::get(wiki, &subj)?;
            let Some(entry) = entry else {
                return Err(EngineError::Txn(format!(
                    "subject {} is not a declared topic in conflict",
                    py_repr_str(&subj)
                )));
            };
            if entry.get("status").and_then(Value::as_str) != Some("conflict") {
                return Err(EngineError::Txn(format!(
                    "subject {} is not a declared topic in conflict",
                    py_repr_str(&subj)
                )));
            }
            let mut declared: Vec<String> = entry
                .get("alternate_kinds")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(k) = entry.get("kind").and_then(Value::as_str) {
                declared.push(k.to_string());
            }
            if !declared.iter().any(|k| k == &new_kind) {
                let mut list: Vec<String> =
                    declared.iter().cloned().collect::<HashSet<_>>().into_iter().collect();
                list.sort();
                return Err(EngineError::Txn(format!(
                    "kind {} was not declared for {} (declared: {})",
                    py_repr_str(&new_kind),
                    py_repr_str(&subj),
                    list.join(", ")
                )));
            }
            let old_dir = entry
                .get("kind")
                .and_then(Value::as_str)
                .and_then(page_dir);
            let subj2 = subj.clone();
            let new_kind2 = new_kind.clone();
            txn.stage_state_with(topics_mod::TOPICS_FILE, move |reg| {
                if !reg["topics"].is_object() {
                    reg["topics"] = json!({});
                }
                if let Some(e) = reg["topics"].as_object_mut().unwrap().get_mut(&subj2) {
                    e["kind"] = json!(new_kind2);
                    e["status"] = json!("resolved");
                }
            })?;
            let slug = topics_mod::slugify(&subj);
            let new_dir = page_dir(&new_kind);
            if old_dir.is_some() && old_dir != new_dir {
                let dir = wiki.p(&format!("wiki/{}", old_dir.unwrap()));
                let mut stale: Vec<String> = Vec::new();
                if dir.is_dir() {
                    for e in std::fs::read_dir(&dir)? {
                        let e = e?;
                        let name = e.file_name().to_string_lossy().into_owned();
                        if name == format!("{slug}.md")
                            || (name.starts_with(&format!("{slug}-"))
                                && name.ends_with(".md"))
                        {
                            stale.push(format!("wiki/{}/{name}", old_dir.unwrap()));
                        }
                    }
                }
                if !stale.is_empty() {
                    let refs: Vec<&str> = stale.iter().map(String::as_str).collect();
                    deps_mod::mark_stale(txn, &refs)?;
                }
            }
        }
        "defer" => {}
        "reopen" => {
            if !matches!(status, "deferred" | "resolved") {
                return Err(EngineError::Txn(format!(
                    "reopen requires a deferred or resolved item (status={status})"
                )));
            }
        }
        _ => unreachable!("ACTIONS gate"),
    }

    let mut entry = json!({"action": action, "at": util::now_iso()});
    if !note.is_empty() {
        entry["note"] = json!(note);
    }
    {
        let item = &mut queue[idx];
        if !item["history"].is_array() {
            item["history"] = json!([]);
        }
        item["history"].as_array_mut().unwrap().push(entry);
        match action {
            "defer" => {
                item["status"] = json!("deferred");
                item["deferred_at"] = json!(util::now_iso());
                item.as_object_mut().unwrap().remove("resolved_at");
            }
            "reopen" => {
                item["status"] = json!("open");
                item.as_object_mut().unwrap().remove("deferred_at");
                item.as_object_mut().unwrap().remove("resolved_at");
                item["resolution"] = json!("");
            }
            _ => {
                item["status"] = json!("resolved");
                item["resolved_at"] = json!(util::now_iso());
            }
        }
    }
    txn.stage_write(QUEUE, dump(queue).as_bytes())?;

    // release gate: claim mutations invalidate dependent artifacts (PRD §58)
    if !changed_claim_ids.is_empty() {
        let refs: Vec<&str> = changed_claim_ids.iter().map(String::as_str).collect();
        deps_mod::invalidate(wiki, txn, &refs)?;
    }

    Ok(queue[idx].clone())
}

/// `review._item_claim` (review.py:269) — first resolvable claim linked
/// by the item (affected preferred, then evidence).
fn item_claim(wiki: &Wiki, item: &Value) -> Result<Option<Value>, EngineError> {
    let mut seen = HashSet::new();
    let mut ids: Vec<Value> = item
        .get("affected")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    ids.extend(
        item.get("evidence")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    );
    for cid in ids {
        let Some(cid) = cid.as_str() else { continue };
        if seen.contains(cid) {
            continue;
        }
        seen.insert(cid.to_string());
        match claims::load_claim(wiki, cid) {
            Ok(c) => return Ok(Some(c)),
            Err(EngineError::Txn(_)) => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// `review._suggestion` (review.py:284) — conservative deterministic
/// suggestion (spec #14): reject beats accept, suspicious text beats
/// corroboration.
fn suggestion(wiki: &Wiki, item: &Value, claim: Option<&Value>) -> (String, Value) {
    let problem = item
        .get("problem")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let mut action = "defer".to_string();
    if problem.contains("injection") || problem.contains("secret") {
        action = "reject".to_string();
    } else if item.get("kind").and_then(Value::as_str) == Some("needs_review")
        && claim.is_some()
    {
        action = match claims::independence(wiki, claim.unwrap()) {
            Ok(n) if n >= 2 => "accept".to_string(),
            _ => "defer".to_string(),
        };
    }
    // claim-less items get only claim-free actions: accept/reject require
    // claim_id, so emitting them would abort a verbatim apply_batch.
    let Some(claim) = claim else {
        return ("defer".to_string(), json!({}));
    };
    if action == "accept" || action == "reject" {
        return (
            action,
            json!({"claim_id": claim["id"].as_str().unwrap_or("")}),
        );
    }
    (action, json!({}))
}

/// `review.triage` (review.py:309) — group open queue items and attach a
/// suggested action per item. Default key is the linked claim's subject;
/// any other `group_by` names an item field. Groups sorted by key.
pub fn triage(wiki: &Wiki, group_by: &str) -> Result<Value, EngineError> {
    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for item in list_items(wiki, None, Some("open"))? {
        let claim = item_claim(wiki, &item)?;
        let key = if group_by == "subject" {
            claim
                .as_ref()
                .and_then(|c| c.get("subject").and_then(Value::as_str))
                .unwrap_or("")
                .to_string()
        } else {
            match item.get(group_by) {
                Some(Value::String(s)) => s.clone(),
                Some(v) if crate::frontmatter::truthy(Some(v)) => v.to_string(),
                _ => String::new(),
            }
        };
        let (action, params) = suggestion(wiki, &item, claim.as_ref());
        let row = json!({
            "id": item.get("id").cloned().unwrap_or(Value::Null),
            "kind": item.get("kind").cloned().unwrap_or(json!("")),
            "problem": item.get("problem").cloned().unwrap_or(json!("")),
            "subject": claim
                .as_ref()
                .and_then(|c| c.get("subject").cloned())
                .unwrap_or(json!("")),
            "suggested_action": action,
            "params": params,
        });
        groups.entry(key).or_default().push(row);
    }
    Ok(json!({
        "groups": groups
            .into_iter()
            .map(|(k, rows)| json!({"key": k, "count": rows.len(), "items": rows}))
            .collect::<Vec<Value>>()
    }))
}

/// `review.apply_batch` (review.py:331) — a list of {id, action, params}
/// inside ONE Transaction: one commit, one revision bump; any failure
/// aborts the whole batch.
pub fn apply_batch(
    wiki: &Wiki,
    actions: &[Value],
    base_revision: i64,
) -> Result<Value, EngineError> {
    let mut txn = Txn::begin_with(
        wiki,
        &crate::ids::new_run(),
        TxnOpts {
            command: Some("wiki-review-batch".to_string()),
            ..Default::default()
        },
    )?;
    txn.base_revision = base_revision;
    let mut queue = wiki.read_jsonl(QUEUE)?;
    let mut claim_cache: HashMap<String, Value> = HashMap::new();
    let mut results = Vec::new();
    for spec in actions {
        let id = spec.get("id").and_then(Value::as_str).unwrap_or("");
        let action = spec.get("action").and_then(Value::as_str).unwrap_or("");
        let params = spec.get("params").cloned().unwrap_or(json!({}));
        let item = act_in_txn(
            wiki,
            &mut txn,
            &mut queue,
            id,
            action,
            &params,
            &mut claim_cache,
        )?;
        results.push(json!({
            "id": spec.get("id").cloned().unwrap_or(Value::Null),
            "action": spec.get("action").cloned().unwrap_or(Value::Null),
            "status": item.get("status").cloned().unwrap_or(Value::Null),
        }));
    }
    txn.changes
        .insert("review_actions".to_string(), json!(results.len() as i64));
    let receipt = txn.commit()?;
    let revision = match receipt.get("revision") {
        Some(Value::Object(m)) => m.get("after").cloned().unwrap_or(Value::Null),
        other => other.cloned().unwrap_or(Value::Null),
    };
    Ok(json!({
        "applied": results.len() as i64,
        "results": results,
        "revision": revision,
    }))
}

/// `review.summary` (review.py:351) — queue counts consumed by
/// overview/graph views.
pub fn summary(wiki: &Wiki) -> Result<Value, EngineError> {
    let items = wiki.read_jsonl(QUEUE)?;
    Ok(json!({
        "open": items
            .iter()
            .filter(|i| i.get("status").and_then(Value::as_str) == Some("open"))
            .count() as i64,
        "deferred": items
            .iter()
            .filter(|i| i.get("status").and_then(Value::as_str) == Some("deferred"))
            .count() as i64,
        "total": items.len() as i64,
    }))
}

/// `review.age_days` (review.py:359) — whole days since an ISO
/// timestamp; 0 when missing/unparseable.
pub fn age_days(ts: Option<&str>) -> i64 {
    let Some(ts) = ts else {
        return 0;
    };
    let Some(then) = util::parse_z_epoch(ts) else {
        return 0;
    };
    let now = util::unix_secs() as i64;
    std::cmp::max(0, (now - then) / 86_400)
}

/// topics.py PAGE_DIR lookup (kind → page dir).
fn page_dir(kind: &str) -> Option<&'static str> {
    topics_mod::PAGE_DIR
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, d)| *d)
}
