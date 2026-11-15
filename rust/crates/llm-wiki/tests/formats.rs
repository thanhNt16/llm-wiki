//! `formats` module: in-memory office zips → markdown, crow's zip limits.

use flate2::write::DeflateEncoder;
use flate2::Compression;
use llm_wiki::formats::{self, Format};
use std::io::Write;

fn le16(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

fn le32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// Hand-roll a zip: `members` = (name, method, stored-bytes, claimed-unc-size).
/// CRC is zero — the reader doesn't verify it. `method`: 0 stored, 8 deflate.
fn zip_ex(members: &[(&str, u16, &[u8], u32)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, method, data, unc) in members {
        let off = out.len() as u32;
        out.extend(le32(0x0403_4B50));
        out.extend(le16(20));
        out.extend(le16(0));
        out.extend(le16(*method));
        out.extend(le16(0));
        out.extend(le16(0));
        out.extend(le32(0));
        out.extend(le32(data.len() as u32));
        out.extend(le32(*unc));
        out.extend(le16(name.len() as u16));
        out.extend(le16(0));
        out.extend(name.as_bytes());
        out.extend(*data);

        cd.extend(le32(0x0201_4B50));
        cd.extend(le16(20));
        cd.extend(le16(20));
        cd.extend(le16(0));
        cd.extend(le16(*method));
        cd.extend(le16(0));
        cd.extend(le16(0));
        cd.extend(le32(0));
        cd.extend(le32(data.len() as u32));
        cd.extend(le32(*unc));
        cd.extend(le16(name.len() as u16));
        cd.extend(le16(0));
        cd.extend(le16(0));
        cd.extend(le16(0));
        cd.extend(le16(0));
        cd.extend(le32(0));
        cd.extend(le32(off));
        cd.extend(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    let n = members.len() as u16;
    out.extend(&cd);
    out.extend(le32(0x0605_4B50));
    out.extend(le16(0));
    out.extend(le16(0));
    out.extend(le16(n));
    out.extend(le16(n));
    out.extend(le32(cd.len() as u32));
    out.extend(le32(cd_off));
    out.extend(le16(0));
    out
}

fn zip(members: &[(&str, &[u8])]) -> Vec<u8> {
    zip_ex(
        &members
            .iter()
            .map(|(n, d)| (*n, 0u16, *d, d.len() as u32))
            .collect::<Vec<_>>(),
    )
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = DeflateEncoder::new(Vec::new(), Compression::best());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

const DOC_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Title One</w:t></w:r></w:p>
<w:p><w:r><w:t>First </w:t></w:r><w:r><w:t>para</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>bullet one</w:t></w:r></w:p>
<w:p><w:r><w:t>a &amp; b</w:t></w:r></w:p>
<w:tbl>
<w:tr><w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
</w:body></w:document>"#;

#[test]
fn docx_extracts_markdown() {
    let z = zip(&[("word/document.xml", DOC_XML.as_bytes())]);
    let md = formats::extract(Format::Docx, &z).unwrap();
    assert!(md.contains("# Title One"), "{md}");
    assert!(md.contains("First para"), "{md}");
    assert!(md.contains("- bullet one"), "{md}");
    assert!(md.contains("a & b"), "{md}");
    assert!(md.contains("| A | B |"), "{md}");
    assert!(md.contains("| 1 | 2 |"), "{md}");
    assert!(md.contains("| --- |"), "{md}");
}

const SLIDE1: &str = r#"<?xml version="1.0"?>
<p:sld xmlns:p="x" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
<p:cSld><p:spTree><p:sp><p:txBody>
<a:p><a:r><a:t>Hello slide</a:t></a:r></a:p>
<a:p><a:r><a:t>line two</a:t></a:r></a:p>
</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;

const SLIDE2: &str = r#"<?xml version="1.0"?>
<p:sld xmlns:p="x" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
<p:cSld><p:spTree><p:sp><p:txBody>
<a:p><a:r><a:t>second</a:t></a:r></a:p>
</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#;

#[test]
fn pptx_extracts_slides_in_order() {
    // Deliberately unordered central directory: sort must be by slide number.
    let z = zip(&[
        ("ppt/slides/slide2.xml", SLIDE2.as_bytes()),
        ("ppt/slides/slide1.xml", SLIDE1.as_bytes()),
    ]);
    let md = formats::extract(Format::Pptx, &z).unwrap();
    let one = md.find("## Slide 1").unwrap();
    let two = md.find("## Slide 2").unwrap();
    assert!(one < two, "{md}");
    assert!(md.contains("Hello slide"), "{md}");
    assert!(md.contains("line two"), "{md}");
    assert!(md.contains("second"), "{md}");
}

const SHARED: &str = r#"<?xml version="1.0"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2" uniqueCount="2">
<si><t>Alpha</t></si>
<si><t>Beta</t></si>
</sst>"#;

const SHEET_XML: &str = r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheetData>
<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
<row r="2"><c r="A2"><v>42</v></c><c r="B2" t="inlineStr"><is><t>in|line</t></is></c></row>
</sheetData></worksheet>"#;

const WORKBOOK: &str = r#"<?xml version="1.0"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="FirstSheet" sheetId="1" r:id="rId1"/></sheets></workbook>"#;

const RELS: &str = r#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;

#[test]
fn xlsx_extracts_pipe_table() {
    let z = zip(&[
        ("xl/sharedStrings.xml", SHARED.as_bytes()),
        ("xl/workbook.xml", WORKBOOK.as_bytes()),
        ("xl/_rels/workbook.xml.rels", RELS.as_bytes()),
        ("xl/worksheets/sheet1.xml", SHEET_XML.as_bytes()),
    ]);
    let md = formats::extract(Format::Xlsx, &z).unwrap();
    assert!(md.contains("| Alpha | Beta |"), "{md}");
    assert!(md.contains("| 42 | in\\|line |"), "{md}");
    // Single sheet → no ## header.
    assert!(!md.contains("## FirstSheet"), "{md}");
}

const TWO_SHEET_BOOK: &str = r#"<?xml version="1.0"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets>
<sheet name="S1" sheetId="1" r:id="rId1"/>
<sheet name="S2" sheetId="2" r:id="rId2"/>
</sheets></workbook>"#;

const TWO_RELS: &str = r#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Target="worksheets/sheet2.xml"/>
</Relationships>"#;

const SHEET2_XML: &str = r#"<?xml version="1.0"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData><row><c><v>7</v></c></row></sheetData></worksheet>"#;

#[test]
fn xlsx_multi_sheet_headers() {
    let z = zip(&[
        ("xl/workbook.xml", TWO_SHEET_BOOK.as_bytes()),
        ("xl/_rels/workbook.xml.rels", TWO_RELS.as_bytes()),
        ("xl/worksheets/sheet1.xml", SHEET_XML.as_bytes()),
        ("xl/worksheets/sheet2.xml", SHEET2_XML.as_bytes()),
    ]);
    let md = formats::extract(Format::Xlsx, &z).unwrap();
    assert!(md.contains("## S1"), "{md}");
    assert!(md.contains("## S2"), "{md}");
    assert!(md.contains("| 7 |"), "{md}");
}

const ODT_CONTENT: &str = r#"<?xml version="1.0"?>
<office:document-content xmlns:office="o" xmlns:text="t" xmlns:table="tb">
<office:body><office:text>
<text:h text:outline-level="1">Heading</text:h>
<text:p>body text</text:p>
<text:list><text:list-item><text:p>item one</text:p></text:list-item></text:list>
</office:text></office:body></office:document-content>"#;

#[test]
fn odt_extracts_headings_lines_lists() {
    let z = zip(&[("content.xml", ODT_CONTENT.as_bytes())]);
    let md = formats::extract(Format::Odt, &z).unwrap();
    assert!(md.contains("# Heading"), "{md}");
    assert!(md.contains("body text"), "{md}");
    assert!(md.contains("- item one"), "{md}");
}

const ODS_CONTENT: &str = r#"<?xml version="1.0"?>
<office:document-content xmlns:office="o" xmlns:text="t" xmlns:table="tb">
<office:body><office:spreadsheet>
<table:table table:name="Data">
<table:table-row>
<table:table-cell office:value-type="string"><text:p>Name</text:p></table:table-cell>
<table:table-cell office:value-type="string"><text:p>Val</text:p></table:table-cell>
</table:table-row>
<table:table-row>
<table:table-cell office:value-type="string"><text:p>x</text:p></table:table-cell>
<table:table-cell office:value-type="float" office:value="9"><text:p>9</text:p></table:table-cell>
</table:table-row>
</table:table>
</office:spreadsheet></office:body></office:document-content>"#;

#[test]
fn ods_extracts_pipe_table() {
    let z = zip(&[("content.xml", ODS_CONTENT.as_bytes())]);
    let md = formats::extract(Format::Ods, &z).unwrap();
    assert!(md.contains("| Name | Val |"), "{md}");
    assert!(md.contains("| x | 9 |"), "{md}");
    assert!(!md.contains("## Data"), "single table needs no header: {md}");
}

#[test]
fn ods_repeated_cells_expand() {
    let doc = r#"<?xml version="1.0"?>
<office:document-content xmlns:office="o" xmlns:text="t" xmlns:table="tb">
<office:body><office:spreadsheet>
<table:table table:name="R">
<table:table-row>
<table:table-cell><text:p>A</text:p></table:table-cell>
<table:table-cell table:number-columns-repeated="3"><text:p></text:p></table:table-cell>
<table:table-cell><text:p>B</text:p></table:table-cell>
</table:table-row>
</table:table>
</office:spreadsheet></office:body></office:document-content>"#;
    let z = zip(&[("content.xml", doc.as_bytes())]);
    let md = formats::extract(Format::Ods, &z).unwrap();
    assert!(md.contains("| A |  |  |  | B |"), "{md}");
}

#[test]
fn detect_ext_only_rust_readers() {
    assert_eq!(formats::detect_ext("a.docx"), Some(Format::Docx));
    assert_eq!(formats::detect_ext("a.pptx"), Some(Format::Pptx));
    assert_eq!(formats::detect_ext("a.xlsx"), Some(Format::Xlsx));
    assert_eq!(formats::detect_ext("a.odt"), Some(Format::Odt));
    assert_eq!(formats::detect_ext("a.ods"), Some(Format::Ods));
    assert_eq!(formats::detect_ext("a.DOCX"), Some(Format::Docx));
    // Legacy + pdf stay on the markitdown lane.
    assert_eq!(formats::detect_ext("a.doc"), None);
    assert_eq!(formats::detect_ext("a.xls"), None);
    assert_eq!(formats::detect_ext("a.ppt"), None);
    assert_eq!(formats::detect_ext("a.pdf"), None);
    assert_eq!(formats::detect_ext("a.txt"), None);
    assert_eq!(formats::detect_ext("a"), None);
}

#[test]
fn extract_named_combines_detect_and_extract() {
    let z = zip(&[("word/document.xml", DOC_XML.as_bytes())]);
    let md = formats::extract_named("f.docx", &z).unwrap().unwrap();
    assert!(md.contains("# Title One"));
    assert!(formats::extract_named("f.doc", &z).unwrap().is_none());
}

#[test]
fn parser_name_is_rust_oxml() {
    assert_eq!(formats::PARSER_NAME, "rust-oxml");
}

#[test]
fn corrupt_zip_errors() {
    let garbage = b"not a zip at all, just bytes";
    assert!(formats::extract(Format::Docx, garbage).is_err());
    // Truncated real zip.
    let z = zip(&[("word/document.xml", DOC_XML.as_bytes())]);
    let short = &z[..z.len() / 2];
    assert!(formats::extract(Format::Docx, short).is_err());
}

#[test]
fn missing_part_errors_not_panics() {
    let z = zip(&[("some/other.xml", b"<x/>")]);
    let r = formats::extract(Format::Docx, &z);
    assert!(r.is_err());
    let msg = r.unwrap_err().to_string();
    assert!(msg.contains("word/document.xml"), "{msg}");
}

#[test]
fn deflated_members_read() {
    let d = deflate(DOC_XML.as_bytes());
    let z = zip_ex(&[("word/document.xml", 8, &d, DOC_XML.len() as u32)]);
    let md = formats::extract(Format::Docx, &z).unwrap();
    assert!(md.contains("# Title One"), "{md}");
}

#[test]
fn oversize_claimed_member_errors_not_panics() {
    // Directory claims 70MB uncompressed for a stored member — rejected
    // before any decompression is attempted.
    let small = b"tiny";
    let z = zip_ex(&[(
        "word/document.xml",
        0,
        small,
        (70 * 1024 * 1024) as u32,
    )]);
    let r = formats::extract(Format::Docx, &z);
    assert!(r.is_err());
    assert!(
        r.unwrap_err().to_string().contains("exceeds limits"),
        "must be a limit error"
    );
}
#[test]
fn high_ratio_bomb_errors() {
    // 64 KB of 'A' deflates to ~85 bytes ≈ 770x — the archive's ~200 bytes
    // against 64 KB claimed uncompressed clears the 100x cap. The check
    // runs on central-directory sizes before any decompression.
    let filler = vec![b'A'; 65536];
    let doc = deflate(&filler);
    let z = zip_ex(&[("word/document.xml", 8, &doc, filler.len() as u32)]);
    let r = formats::extract(Format::Docx, &z);
    assert!(r.is_err());
    let msg = r.unwrap_err().to_string();
    assert!(msg.contains("exceeds limits"), "{msg}");
}

#[test]
fn corrupt_deflate_stream_errors() {
    let bogus = b"this is not deflate data at all!!";
    let z = zip_ex(&[(
        "word/document.xml",
        8,
        bogus,
        bogus.len() as u32,
    )]);
    assert!(formats::extract(Format::Docx, &z).is_err());
}

#[test]
fn encrypted_member_errors() {
    // flags bit 0 set on the central-directory record.
    let mut z = zip(&[("word/document.xml", DOC_XML.as_bytes())]);
    // Patch the CD flags field: find CD sig then flags at +8.
    let cd = z
        .windows(4)
        .position(|w| w == [0x50, 0x4B, 0x01, 0x02])
        .unwrap();
    z[cd + 8] = 1;
    assert!(formats::extract(Format::Docx, &z).is_err());
}

#[test]
fn forbidden_xml_chars_stripped() {
    // &#x1; (U+0001) is not a legal XML-1.0 char; extraction must drop it
    // from output rather than erroring or passing it through.
    let doc = r#"<?xml version="1.0"?>
<w:document xmlns:w="w"><w:body>
<w:p><w:r><w:t>good&#x1;text</w:t></w:r></w:p>
</w:body></w:document>"#;
    let z = zip(&[("word/document.xml", doc.as_bytes())]);
    let md = formats::extract(Format::Docx, &z).unwrap();
    assert!(md.contains("goodtext"), "{md}");
    assert!(!md.chars().any(|c| c < '\u{20}' && c != '\n'), "{md:?}");
}

#[test]
fn too_many_members_errors() {
    let mut members = Vec::new();
    let body = b"x";
    for i in 0..10_001 {
        let name = format!("m{i}.xml");
        members.push((name, 0u16, body.to_vec()));
    }
    let refs: Vec<(&str, u16, &[u8], u32)> = members
        .iter()
        .map(|(n, m, d)| (n.as_str(), *m, d.as_slice(), 1))
        .collect();
    let z = zip_ex(&refs);
    let r = formats::extract(Format::Docx, &z);
    assert!(r.is_err());
    assert!(r.unwrap_err().to_string().contains("exceeds limits"));
}
