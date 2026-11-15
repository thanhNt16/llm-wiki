//! ODF `content.xml` extractor — one walker serves odt (text:h/text:p →
//! headings/lines, lists → `- `) and ods (table:table → pipe tables with
//! `## name` headers when the doc has more than one table). Nested tables
//! inside cells flatten into the cell text (spec §8).

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::err::EngineError;
use crate::formats::zip::ZipArchive;
use crate::formats::{attr, md_table, strip_forbidden};

/// Cap on repeated-row/column expansion and total emitted rows — real ODS
/// sheets repeat empties liberally; unbounded repetition turns a 1 KB file
/// into a multi-GB table.
const MAX_REPEAT: u64 = 64;
const MAX_ROWS: usize = 4096;

pub(crate) fn odf(z: &ZipArchive) -> Result<String, EngineError> {
    let xml = z.member("content.xml")?;
    let table_count = count_tables(&xml)?;
    let mut r = Reader::from_reader(xml.as_slice());
    let dec = r.decoder();
    let mut out = String::new();

    // Paragraph state — a stack survives `p` inside `annotation`/`note`.
    let mut p_depth = 0u32;
    let mut p_heading = 0usize;
    let mut p_text = String::new();
    // List state.
    let mut list_depth = 0u32;
    let mut item_depth = 0u32;
    // Table state — outer `table:table` drives headers; nested tables
    // inside cells flatten into cell text.
    let mut tbl_depth = 0u32;
    let mut row_stack: Vec<Vec<String>> = Vec::new();
    // Cell stack: (text, repeat, span).
    let mut cell_stack: Vec<(String, u64, u64)> = Vec::new();
    let mut tbl_rows: Vec<Vec<String>> = Vec::new();
    let mut tbl_name = String::new();
    // Skip subtrees (annotations, tracked deletions).
    let mut skip_depth = 0u32;

    macro_rules! flush_p {
        () => {{
            let text = strip_forbidden(&p_text);
            let text = text.trim();
            if let Some((cell, ..)) = cell_stack.last_mut() {
                if !text.is_empty() {
                    if !cell.is_empty() {
                        cell.push(' ');
                    }
                    cell.push_str(text);
                }
            } else if !text.is_empty() {
                if item_depth > 0 {
                    out.push_str("- ");
                } else if p_heading > 0 {
                    out.push_str(&"#".repeat(p_heading.min(6)));
                    out.push(' ');
                }
                out.push_str(text);
                out.push('\n');
            }
            p_text.clear();
        }};
    }

    loop {
        match r
            .read_event()
            .map_err(|e| EngineError::Txn(format!("odf xml: {e}")))?
        {
            Event::Start(e) => {
                if skip_depth > 0 {
                    skip_depth += 1;
                    continue;
                }
                match e.local_name().as_ref() {
                    b"annotation" | b"tracked-changes" => skip_depth += 1,
                    b"table" => {
                        tbl_depth += 1;
                        if tbl_depth == 1 {
                            tbl_rows.clear();
                            tbl_name = attr(&e, "name", dec).unwrap_or_default();
                        }
                    }
                    b"table-row" => {
                        if tbl_depth > 0 {
                            row_stack.push(Vec::new());
                        }
                    }
                    b"table-cell" => {
                        if tbl_depth > 0 {
                            let rep = attr(&e, "number-columns-repeated", dec)
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(1)
                                .min(MAX_REPEAT);
                            let span = attr(&e, "number-columns-spanned", dec)
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(1)
                                .min(MAX_REPEAT);
                            cell_stack.push((String::new(), rep, span));
                        }
                    }
                    b"covered-table-cell" => {
                        if tbl_depth > 0 {
                            if let Some(row) = row_stack.last_mut() {
                                row.push(String::new());
                            }
                        }
                    }
                    b"h" => {
                        p_depth += 1;
                        if p_depth == 1 {
                            p_text.clear();
                            p_heading = attr(&e, "level", dec)
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(1)
                                .clamp(1, 6);
                        }
                    }
                    b"p" => {
                        p_depth += 1;
                        if p_depth == 1 {
                            p_text.clear();
                            p_heading = 0;
                        }
                    }
                    b"list" => list_depth += 1,
                    b"list-item" => {
                        if list_depth > 0 {
                            item_depth += 1;
                        }
                    }
                    b"tab" | b"line-break" => {
                        if p_depth > 0 {
                            p_text.push(' ');
                        } else if let Some((c, ..)) = cell_stack.last_mut() {
                            c.push(' ');
                        }
                    }
                    b"s" => {
                        let n: usize = attr(&e, "c", dec)
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1)
                            .min(64);
                        if p_depth > 0 {
                            for _ in 0..n {
                                p_text.push(' ');
                            }
                        } else if let Some((c, ..)) = cell_stack.last_mut() {
                            for _ in 0..n {
                                c.push(' ');
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => {
                if skip_depth > 0 {
                    continue;
                }
                match e.local_name().as_ref() {
                    b"p" | b"h" => flush_p!(),
                    b"table-cell" => {
                        if tbl_depth > 0 {
                            let rep = attr(&e, "number-columns-repeated", dec)
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(1)
                                .min(MAX_REPEAT);
                            if let Some(row) = row_stack.last_mut() {
                                for _ in 0..rep {
                                    row.push(String::new());
                                }
                            }
                        }
                    }
                    b"covered-table-cell" => {
                        if tbl_depth > 0 {
                            if let Some(row) = row_stack.last_mut() {
                                row.push(String::new());
                            }
                        }
                    }
                    b"tab" | b"line-break" => {
                        if p_depth > 0 {
                            p_text.push(' ');
                        } else if let Some((c, ..)) = cell_stack.last_mut() {
                            c.push(' ');
                        }
                    }
                    b"s" => {
                        let n: usize = attr(&e, "c", dec)
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1)
                            .min(64);
                        if p_depth > 0 {
                            for _ in 0..n {
                                p_text.push(' ');
                            }
                        } else if let Some((c, ..)) = cell_stack.last_mut() {
                            for _ in 0..n {
                                c.push(' ');
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                let ln = e.local_name();
                if skip_depth > 0 {
                    skip_depth -= 1;
                    continue;
                }
                match ln.as_ref() {
                    b"table" => {
                        if tbl_depth > 0 {
                            tbl_depth -= 1;
                        }
                        if tbl_depth == 0 && !tbl_rows.is_empty() {
                            if table_count > 1 {
                                out.push_str(&format!("## {tbl_name}\n\n"));
                            }
                            out.push_str(&md_table(&tbl_rows));
                            out.push('\n');
                            tbl_rows.clear();
                        }
                    }
                    b"table-row" => {
                        if let Some(row) = row_stack.pop() {
                            if cell_stack.is_empty() {
                                if tbl_rows.len() < MAX_ROWS
                                    && row.iter().any(|c| !c.is_empty())
                                {
                                    tbl_rows.push(row);
                                }
                            } else if let Some((cell, ..)) = cell_stack.last_mut() {
                                // Nested table inside a cell: flatten cells.
                                for col in &row {
                                    if !col.is_empty() {
                                        if !cell.is_empty() {
                                            cell.push(' ');
                                        }
                                        cell.push_str(col);
                                    }
                                }
                            }
                        }
                    }
                    b"table-cell" => {
                        if let Some((text, rep, span)) = cell_stack.pop() {
                            if let Some(row) = row_stack.last_mut() {
                                if rep > 1 {
                                    for _ in 0..rep {
                                        row.push(text.clone());
                                    }
                                } else {
                                    row.push(text);
                                    for _ in 1..span {
                                        row.push(String::new());
                                    }
                                }
                            }
                        }
                    }
                    b"h" | b"p" => {
                        if p_depth > 0 {
                            p_depth -= 1;
                        }
                        if p_depth == 0 {
                            flush_p!();
                        }
                    }
                    b"list" => list_depth = list_depth.saturating_sub(1),
                    b"list-item" => {
                        item_depth = item_depth.saturating_sub(1);
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if skip_depth == 0 {
                    let s = t
                        .unescape()
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned());
                    if p_depth > 0 {
                        p_text.push_str(&s);
                    } else if let Some((c, ..)) = cell_stack.last_mut() {
                        c.push_str(&s);
                    }
                }
            }
            Event::CData(t) => {
                if skip_depth == 0 {
                    let s = t
                        .decode()
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned());
                    if p_depth > 0 {
                        p_text.push_str(&s);
                    } else if let Some((c, ..)) = cell_stack.last_mut() {
                        c.push_str(&s);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// Count top-level `table:table` elements — drives the `## <name>` header
/// decision without buffering whole tables.
fn count_tables(xml: &[u8]) -> Result<usize, EngineError> {
    let mut r = Reader::from_reader(xml);
    let mut depth = 0u32;
    let mut count = 0usize;
    loop {
        match r
            .read_event()
            .map_err(|e| EngineError::Txn(format!("odf xml: {e}")))?
        {
            Event::Start(e) => {
                if e.local_name().as_ref() == b"table" {
                    depth += 1;
                    if depth == 1 {
                        count += 1;
                    }
                }
            }
            Event::End(e) => {
                if e.local_name().as_ref() == b"table" && depth > 0 {
                    depth -= 1;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(count)
}
