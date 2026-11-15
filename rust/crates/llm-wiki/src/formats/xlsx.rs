//! xlsx → markdown pipe tables: sharedStrings + workbook sheet order +
//! per-worksheet cell grid (spec §8).

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::err::EngineError;
use crate::formats::zip::ZipArchive;
use crate::formats::{attr, col_idx, md_table};

fn xerr(what: &str, e: quick_xml::Error) -> EngineError {
    EngineError::Txn(format!("xlsx {what}: {e}"))
}

/// All worksheets → `## <name>`-headed pipe tables (headers only when the
/// book has more than one sheet — note-per-sheet).
pub(crate) fn xlsx(z: &ZipArchive) -> Result<String, EngineError> {
    let shared = if z.has("xl/sharedStrings.xml") {
        parse_shared(&z.member("xl/sharedStrings.xml")?)?
    } else {
        Vec::new()
    };
    let mut sheets: Vec<(Option<String>, String)> = Vec::new();
    if z.has("xl/workbook.xml") {
        let book = z.member("xl/workbook.xml")?;
        let rels = if z.has("xl/_rels/workbook.xml.rels") {
            parse_rels(&z.member("xl/_rels/workbook.xml.rels")?)?
        } else {
            Vec::new()
        };
        for (i, (name, rid)) in parse_sheets(&book)?.into_iter().enumerate() {
            let path = rid
                .and_then(|rid| {
                    rels.iter()
                        .find(|(id, _)| id == &rid)
                        .map(|(_, t)| rel_target(t))
                })
                .unwrap_or_else(|| format!("xl/worksheets/sheet{}.xml", i + 1));
            sheets.push((Some(name), path));
        }
    }
    if sheets.is_empty() {
        // No workbook part (or no <sheet> entries): take every worksheet
        // member in sheet-number order.
        let mut paths: Vec<(u64, String)> = z
            .names()
            .filter_map(|n| {
                let rest = n.strip_prefix("xl/worksheets/sheet")?;
                let num = rest.strip_suffix(".xml")?;
                num.parse::<u64>().ok().map(|i| (i, n.to_string()))
            })
            .collect();
        paths.sort_unstable_by_key(|(i, _)| *i);
        for (i, p) in paths {
            sheets.push((None, p));
            let _ = i;
        }
    }
    if sheets.is_empty() {
        return Err(EngineError::Txn("xlsx contains no worksheets".into()));
    }
    let multi = sheets.len() > 1;
    let mut out = String::new();
    for (i, (name, path)) in sheets.iter().enumerate() {
        if multi {
            out.push_str(&format!(
                "## {}\n\n",
                name.clone().unwrap_or_else(|| format!("Sheet {}", i + 1))
            ));
        }
        let xml = z.member(path)?;
        let rows = parse_sheet(&xml, &shared)?;
        let table = md_table(&rows);
        if table.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&table);
            out.push('\n');
        }
    }
    Ok(out)
}

/// `xl/_rels/workbook.xml.rels` → (rId, Target) pairs.
fn parse_rels(xml: &[u8]) -> Result<Vec<(String, String)>, EngineError> {
    let mut r = Reader::from_reader(xml);
    let mut rels = Vec::new();
    loop {
        match r.read_event().map_err(|e| xerr("rels", e))? {
            Event::Start(e) | Event::Empty(e) => {
                if e.local_name().as_ref() == b"Relationship" {
                    let id = attr(&e, "Id", r.decoder());
                    let target = attr(&e, "Target", r.decoder());
                    if let (Some(id), Some(t)) = (id, target) {
                        rels.push((id, t));
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(rels)
}

/// `xl/workbook.xml` → (sheet name, r:id) in document order.
fn parse_sheets(xml: &[u8]) -> Result<Vec<(String, Option<String>)>, EngineError> {
    let mut r = Reader::from_reader(xml);
    let mut sheets = Vec::new();
    loop {
        match r.read_event().map_err(|e| xerr("workbook", e))? {
            Event::Start(e) | Event::Empty(e) => {
                if e.local_name().as_ref() == b"sheet" {
                    let name = attr(&e, "name", r.decoder());
                    let rid = attr(&e, "id", r.decoder());
                    if let Some(name) = name {
                        sheets.push((name, rid));
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(sheets)
}

/// Rel target → archive member path (`worksheets/sheet1.xml`,
/// `/xl/worksheets/…`, or already-absolute `xl/…`).
fn rel_target(t: &str) -> String {
    let t = t.trim_start_matches('/');
    if t.starts_with("xl/") {
        t.to_string()
    } else {
        format!("xl/{t}")
    }
}

/// `xl/sharedStrings.xml` → concatenated `<t>` text per `<si>`
/// (`rPh` phonetic runs skipped).
fn parse_shared(xml: &[u8]) -> Result<Vec<String>, EngineError> {
    let mut r = Reader::from_reader(xml);
    let mut strings: Vec<String> = Vec::new();
    let mut in_si = false;
    let mut in_phonetic = 0u32;
    let mut cur = String::new();
    loop {
        match r.read_event().map_err(|e| xerr("sharedStrings", e))? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"si" => {
                    in_si = true;
                    cur.clear();
                }
                b"rPh" => in_phonetic += 1,
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                b"si" => {
                    in_si = false;
                    strings.push(std::mem::take(&mut cur));
                }
                b"rPh" => in_phonetic = in_phonetic.saturating_sub(1),
                _ => {}
            },
            Event::Text(t) => {
                if in_si && in_phonetic == 0 {
                    cur.push_str(
                        &t.unescape()
                            .map(|c| c.into_owned())
                            .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned()),
                    );
                }
            }
            Event::CData(t) => {
                if in_si && in_phonetic == 0 {
                    cur.push_str(
                        &t.decode()
                            .map(|c| c.into_owned())
                            .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned()),
                    );
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(strings)
}

/// One worksheet → row-major cell grid. `r="B2"` refs pad the row to the
/// right column; `t="s"` resolves through shared strings, `t="inlineStr"`
/// reads `<is><t>`, everything else emits `<v>` verbatim.
fn parse_sheet(xml: &[u8], shared: &[String]) -> Result<Vec<Vec<String>>, EngineError> {
    let mut r = Reader::from_reader(xml);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut in_row = false;
    let mut in_cell = false;
    let mut cell_type = String::new();
    let mut v_text = String::new(); // <v> content of the current cell
    let mut t_text = String::new(); // inline-string <t> content
    let mut text_slot = TextSlot::None;
    loop {
        match r.read_event().map_err(|e| xerr("worksheet", e))? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"row" => {
                    in_row = true;
                    row.clear();
                }
                b"c" => {
                    in_cell = true;
                    cell_type = attr(&e, "t", r.decoder()).unwrap_or_default();
                    v_text.clear();
                    t_text.clear();
                    text_slot = TextSlot::None;
                    if let Some(reference) = attr(&e, "r", r.decoder()) {
                        if let Some(ci) = col_idx(&reference) {
                            while row.len() < ci {
                                row.push(String::new());
                            }
                        }
                    }
                }
                b"v" => text_slot = TextSlot::V,
                b"is" => text_slot = TextSlot::Inline,
                b"t" => {
                    if in_cell && text_slot == TextSlot::Inline {
                        text_slot = TextSlot::InlineT;
                    }
                }
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                b"row" => {
                    // Empty `<row/>` — nothing to emit.
                }
                b"c" => {
                    if in_row {
                        if let Some(reference) = attr(&e, "r", r.decoder()) {
                            if let Some(ci) = col_idx(&reference) {
                                while row.len() < ci {
                                    row.push(String::new());
                                }
                            }
                        }
                        row.push(String::new());
                    }
                }
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref() {
                b"row" => {
                    in_row = false;
                    let done = std::mem::take(&mut row);
                    if done.iter().any(|c| !c.is_empty()) {
                        rows.push(done);
                    }
                }
                b"c" => {
                    in_cell = false;
                    let text = if cell_type == "s" {
                        v_text
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| shared.get(i))
                            .cloned()
                            .unwrap_or_default()
                    } else if !v_text.trim().is_empty() {
                        v_text.trim().to_string()
                    } else {
                        t_text.trim().to_string()
                    };
                    row.push(text);
                }
                b"v" | b"is" | b"t" => text_slot = TextSlot::None,
                _ => {}
            },
            Event::Text(t) => {
                let s = t
                    .unescape()
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned());
                match text_slot {
                    TextSlot::V if in_cell => v_text.push_str(&s),
                    TextSlot::InlineT if in_cell => t_text.push_str(&s),
                    _ => {}
                }
            }
            Event::CData(t) => {
                let s = t
                    .decode()
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned());
                match text_slot {
                    TextSlot::V if in_cell => v_text.push_str(&s),
                    TextSlot::InlineT if in_cell => t_text.push_str(&s),
                    _ => {}
                }
            }

            Event::Eof => break,
            _ => {}
        }
    }
    Ok(rows)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextSlot {
    None,
    V,
    Inline,
    InlineT,
}
