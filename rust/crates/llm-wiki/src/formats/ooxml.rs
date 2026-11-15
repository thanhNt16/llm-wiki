//! OOXML text formats: `word/document.xml` (docx) and `ppt/slides/*` (pptx).

use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::reader::Reader;

use crate::err::EngineError;
use crate::formats::zip::ZipArchive;
use crate::formats::{attr, md_table, strip_forbidden};

/// docx → markdown: paragraphs as lines, `Heading*` pStyles as `#` blocks,
/// numPr paragraphs as `- ` bullets, tables as pipe tables (spec §8).
pub(crate) fn docx(z: &ZipArchive) -> Result<String, EngineError> {
    let xml = z.member("word/document.xml")?;
    let mut r = Reader::from_reader(xml.as_slice());
    let mut out = String::new();
    // Paragraph state.
    let mut p_depth = 0u32; // active <w:p> nesting (annotations etc. can nest text)
    let mut in_ppr = false;
    let mut heading = 0usize;
    let mut list_item = false;
    let mut p_text = String::new();
    // Table state — stacks survive nested tables inside cells.
    let mut tbl_depth = 0u32;
    let mut row_stack: Vec<Vec<String>> = Vec::new();
    let mut cell_stack: Vec<String> = Vec::new();
    let mut tbl_rows: Vec<Vec<String>> = Vec::new();
    // Skip subtrees that aren't document prose.
    let mut skip_depth = 0u32;

    loop {
        match r.read_event().map_err(|e| EngineError::Txn(format!("docx xml: {e}")))? {
            Event::Start(e) => start(&e, &mut Ctx {
                out: &mut out,
                p_depth: &mut p_depth,
                in_ppr: &mut in_ppr,
                heading: &mut heading,
                list_item: &mut list_item,
                tbl_depth: &mut tbl_depth,
                row_stack: &mut row_stack,
                cell_stack: &mut cell_stack,
                tbl_rows: &mut tbl_rows,
                skip_depth: &mut skip_depth,
                p_text: &mut p_text,
                dec: r.decoder(),
            }),
            Event::Empty(e) => {
                if skip_depth > 0 {
                    continue;
                }
                let ln = e.local_name();
                match ln.as_ref() {
                    b"p" => flush_p(&mut out, &mut p_text, heading, list_item, &mut cell_stack),
                    // pStyle/numPr are empty elements in OOXML — Start won't fire.
                    b"pStyle" => {
                        if in_ppr && p_depth == 1 {
                            if let Some(v) = attr(&e, "val", r.decoder()) {
                                if let Some(l) = heading_level(&v) {
                                    heading = l;
                                }
                            }
                        }
                    }
                    b"numPr" => {
                        if in_ppr && p_depth == 1 {
                            list_item = true;
                        }
                    }
                    b"tab" | b"br" | b"cr" => {
                        if p_depth > 0 {
                            p_text.push(' ');
                        } else if let Some(c) = cell_stack.last_mut() {
                            c.push(' ');
                        }
                    }
                    b"tc" => {
                        if let Some(row) = row_stack.last_mut() {
                            row.push(String::new());
                        }
                    }
                    b"tr" => {
                        // `<w:tr/>` — an empty row still emits a table row.
                        if tbl_depth > 0 {
                            tbl_rows.push(Vec::new());
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => end(&e, &mut Ctx {
                out: &mut out,
                p_depth: &mut p_depth,
                in_ppr: &mut in_ppr,
                heading: &mut heading,
                list_item: &mut list_item,
                tbl_depth: &mut tbl_depth,
                row_stack: &mut row_stack,
                cell_stack: &mut cell_stack,
                tbl_rows: &mut tbl_rows,
                skip_depth: &mut skip_depth,
                p_text: &mut p_text,
                dec: r.decoder(),
            }),
            Event::Text(t) => {
                if skip_depth == 0 {
                    let s = t
                        .unescape()
                        .map(|c| c.into_owned())
                        .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned());
                    if p_depth > 0 {
                        p_text.push_str(&s);
                    } else if let Some(c) = cell_stack.last_mut() {
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
                    } else if let Some(c) = cell_stack.last_mut() {
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

struct Ctx<'a> {
    out: &'a mut String,
    p_depth: &'a mut u32,
    in_ppr: &'a mut bool,
    heading: &'a mut usize,
    list_item: &'a mut bool,
    tbl_depth: &'a mut u32,
    row_stack: &'a mut Vec<Vec<String>>,
    cell_stack: &'a mut Vec<String>,
    tbl_rows: &'a mut Vec<Vec<String>>,
    skip_depth: &'a mut u32,
    p_text: &'a mut String,
    dec: quick_xml::Decoder,
}

fn start(e: &BytesStart, c: &mut Ctx) {
    let ln = e.local_name();
    if *c.skip_depth > 0 {
        *c.skip_depth += 1;
        return;
    }
    match ln.as_ref() {
        // Field codes / phonetic guides / deleted text aren't prose.
        b"instrText" | b"fldSimple" | b"del" | b"delText" | b"rPh" => {
            *c.skip_depth += 1;
        }
        b"tbl" => *c.tbl_depth += 1,
        b"tr" => {
            if *c.tbl_depth > 0 {
                c.row_stack.push(Vec::new());
            }
        }
        b"tc" => {
            if *c.tbl_depth > 0 {
                c.cell_stack.push(String::new());
            }
        }
        b"p" => {
            *c.p_depth += 1;
            if *c.p_depth == 1 {
                c.p_text.clear();
                *c.heading = 0;
                *c.list_item = false;
            }
        }
        b"pPr" => *c.in_ppr = true,
        b"pStyle" => {
            if *c.in_ppr && *c.p_depth == 1 {
                if let Some(v) = attr(e, "val", c.dec) {
                    if let Some(l) = heading_level(&v) {
                        *c.heading = l;
                    }
                }
            }
        }
        b"numPr" => {
            if *c.in_ppr && *c.p_depth == 1 {
                *c.list_item = true;
            }
        }
        b"tab" | b"br" | b"cr" => {
            if *c.p_depth > 0 {
                c.p_text.push(' ');
            } else if let Some(cell) = c.cell_stack.last_mut() {
                cell.push(' ');
            }
        }
        _ => {}
    }
}

fn end(e: &BytesEnd, c: &mut Ctx) {
    let ln = e.local_name();
    if *c.skip_depth > 0 {
        *c.skip_depth -= 1;
        return;
    }
    match ln.as_ref() {
        b"tbl" => {
            if *c.tbl_depth > 0 {
                *c.tbl_depth -= 1;
            }
            if *c.tbl_depth == 0 {
                if !c.tbl_rows.is_empty() {
                    c.out.push_str(&md_table(c.tbl_rows));
                    c.out.push('\n');
                    c.tbl_rows.clear();
                }
            }
        }
        b"tr" => {
            if let Some(row) = c.row_stack.pop() {
                if c.cell_stack.is_empty() {
                    c.tbl_rows.push(row);
                } else if let Some(cell) = c.cell_stack.last_mut() {
                    // Nested table inside a cell: flatten cells into cell text.
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
        b"tc" => {
            if let Some(text) = c.cell_stack.pop() {
                if let Some(row) = c.row_stack.last_mut() {
                    row.push(text);
                }
            }
        }
        b"pPr" => *c.in_ppr = false,
        b"p" => {
            if *c.p_depth > 0 {
                *c.p_depth -= 1;
            }
            if *c.p_depth == 0 {
                flush_p(c.out, c.p_text, *c.heading, *c.list_item, c.cell_stack);
            }
        }
        _ => {}
    }
}

/// Emit a finished paragraph — inside a cell it appends to the cell text;
/// at top level it becomes a heading / bullet / plain line.
fn flush_p(
    out: &mut String,
    p_text: &mut String,
    heading: usize,
    list_item: bool,
    cell_stack: &mut Vec<String>,
) {
    let text = strip_forbidden(p_text);
    let text = text.trim();
    if let Some(cell) = cell_stack.last_mut() {
        if !text.is_empty() {
            if !cell.is_empty() {
                cell.push(' ');
            }
            cell.push_str(text);
        }
        p_text.clear();
        return;
    }
    p_text.clear();
    if text.is_empty() {
        return;
    }
    if heading > 0 {
        out.push_str(&"#".repeat(heading.min(6)));
        out.push(' ');
        out.push_str(text);
    } else if list_item {
        out.push_str("- ");
        out.push_str(text);
    } else {
        out.push_str(text);
    }
    out.push('\n');
}

/// `Heading1` / `heading 3` / `Heading 2`-style names → level 1..=6.
fn heading_level(style: &str) -> Option<usize> {
    let s = style.trim();
    let digits: String = s
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    let head = s.to_ascii_lowercase();
    if !head.starts_with("heading") {
        return None;
    }
    let n: usize = digits.parse().ok()?;
    (n >= 1).then(|| n.min(6))
}

/// pptx → markdown: `ppt/slides/slideN.xml` in slide order, each slide a
/// `## Slide N` section of `a:p` lines separated by a blank line.
pub(crate) fn pptx(z: &ZipArchive) -> Result<String, EngineError> {
    let mut slides: Vec<(u64, String)> = z
        .names()
        .filter_map(|n| {
            let rest = n.strip_prefix("ppt/slides/slide")?;
            let num = rest.strip_suffix(".xml")?;
            num.parse::<u64>().ok().map(|i| (i, n.to_string()))
        })
        .collect();
    slides.sort_unstable_by_key(|(i, _)| *i);
    if slides.is_empty() {
        return Err(EngineError::Txn("pptx contains no slides".into()));
    }
    let mut out = String::new();
    for (n, name) in slides {
        let xml = z.member(&name)?;
        let slide = slide_text(&xml)?;
        out.push_str(&format!("## Slide {n}\n\n"));
        out.push_str(&slide);
        out.push('\n');
    }
    Ok(out)
}

/// One slide: each `a:p` is a line; `a:br` and `a:fld` text join runs.
fn slide_text(xml: &[u8]) -> Result<String, EngineError> {
    let mut r = Reader::from_reader(xml);
    let mut out = String::new();
    let mut in_p = false;
    let mut p_text = String::new();
    loop {
        match r.read_event().map_err(|e| EngineError::Txn(format!("pptx xml: {e}")))? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"p" => {
                    in_p = true;
                    p_text.clear();
                }
                b"br" => {
                    if in_p {
                        p_text.push(' ');
                    }
                }
                _ => {}
            },
            Event::Empty(e) => {
                if e.local_name().as_ref() == b"p" {
                    emit_slide_line(&mut out, &mut p_text);
                }
            }
            Event::End(e) => {
                if e.local_name().as_ref() == b"p" {
                    in_p = false;
                    emit_slide_line(&mut out, &mut p_text);
                }
            }
            Event::Text(t) => {
                if in_p {
                    p_text.push_str(
                        &t.unescape()
                            .map(|c| c.into_owned())
                            .unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned()),
                    );
                }
            }
            Event::CData(t) => {
                if in_p {
                    p_text.push_str(
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
    Ok(out)
}

fn emit_slide_line(out: &mut String, p_text: &mut String) {
    let text = strip_forbidden(p_text);
    let text = text.trim();
    p_text.clear();
    if !text.is_empty() {
        out.push_str(text);
        out.push('\n');
    }
}
