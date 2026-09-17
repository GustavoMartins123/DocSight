use docsight_core::{
    DocsightError, Document, DocumentSource, HeaderFooterKind, HeaderFooterVariant, SectionStart,
};
use docsight_ooxml::parse_docx;
use std::io::{Cursor, Write};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const HEADER_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
const FOOTER_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";

fn body(content: &str) -> String {
    format!(
        r#"<w:document xmlns:w="{W_NS}" xmlns:r="{R_NS}"><w:body>{content}</w:body></w:document>"#
    )
}

fn relationships(entries: &[(&str, &str, &str)]) -> String {
    let items: String = entries
        .iter()
        .map(|(id, kind, target)| {
            format!(r#"<Relationship Id="{id}" Type="{kind}" Target="{target}"/>"#)
        })
        .collect();
    format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{items}</Relationships>"#
    )
}

fn part(root: &str, paragraphs: &str) -> String {
    format!(r#"<w:{root} xmlns:w="{W_NS}">{paragraphs}</w:{root}>"#)
}

fn package(parts: &[(&str, String)]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    writer.start_file("[Content_Types].xml", options)?;
    writer.write_all(
        b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>",
    )?;
    for (name, content) in parts {
        writer.start_file(*name, options)?;
        writer.write_all(content.as_bytes())?;
    }
    Ok(writer.finish()?.into_inner())
}

fn parse(
    parts: &[(&str, String)],
) -> Result<(DocumentSource, Document), Box<dyn std::error::Error>> {
    let source = DocumentSource::from_bytes(package(parts)?)?;
    let document = parse_docx(&source)?;
    Ok((source, document))
}

fn codes(document: &Document) -> Vec<&str> {
    document
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect()
}

const LETTER: &str = r#"<w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/>"#;

#[test]
fn paragraph_section_breaks_produce_ordered_sections_with_block_boundaries() -> TestResult {
    let document = body(&format!(
        r#"<w:p><w:r><w:t>first</w:t></w:r></w:p>
        <w:p><w:pPr><w:sectPr><w:pgSz w:w="15840" w:h="12240" w:orient="landscape"/><w:pgMar w:top="-1080" w:right="1080" w:bottom="1080" w:left="1080" w:header="360" w:footer="540" w:gutter="0"/></w:sectPr></w:pPr><w:r><w:t>end of first</w:t></w:r></w:p>
        <w:p><w:r><w:t>second</w:t></w:r></w:p>
        <w:sectPr><w:type w:val="continuous"/>{LETTER}<w:cols w:num="2"/><w:titlePg/><w:pgNumType w:start="5" w:fmt="lowerRoman"/></w:sectPr>"#
    ));
    let (source, parsed) = parse(&[("word/document.xml", document)])?;
    assert_eq!(parsed.sections.len(), 2);
    let first = &parsed.sections[0];
    let second = &parsed.sections[1];
    assert_eq!(
        first.id,
        source.object_id("sect", "/word/document.xml::body/p[2]/pPr/sectPr")
    );
    assert_eq!(first.section_index, 1);
    assert_eq!(
        first.last_block_id,
        Some(source.object_id("p", "/word/document.xml::body/p[2]"))
    );
    assert_eq!(
        (first.page_width_pt, first.page_height_pt),
        (Some(792.0), Some(612.0))
    );
    assert_eq!(first.margin_top_pt, Some(54.0));
    assert_eq!(
        (first.header_distance_pt, first.footer_distance_pt),
        (Some(18.0), Some(27.0))
    );
    assert_eq!(first.start, SectionStart::NextPage);

    assert_eq!(
        second.id,
        source.object_id("sect", "/word/document.xml::body/sectPr[1]")
    );
    assert_eq!(second.section_index, 2);
    assert_eq!(
        second.last_block_id,
        Some(source.object_id("p", "/word/document.xml::body/p[3]"))
    );
    assert_eq!(second.start, SectionStart::Continuous);
    assert_eq!(second.columns, 2);
    assert!(second.title_page);
    assert_eq!(second.page_number_start, Some(5));
    assert_eq!(second.page_number_format.as_deref(), Some("lowerRoman"));

    let warnings = codes(&parsed);
    assert!(warnings.contains(&"DOCX_SECTION_COLUMNS_UNSUPPORTED"));
    assert!(!warnings.contains(&"DOCX_SECTIONS_COLLAPSED"));
    assert!(!warnings.contains(&"DOCX_SECTION_GEOMETRY_DEFAULTED"));
    let columns = parsed
        .warnings
        .iter()
        .find(|warning| warning.code == "DOCX_SECTION_COLUMNS_UNSUPPORTED")
        .ok_or("columns warning")?;
    assert_eq!(columns.object.as_ref(), Some(&second.id));
    Ok(())
}

#[test]
fn single_section_documents_keep_their_section_identity() -> TestResult {
    let document = body(&format!(
        r#"<w:p><w:r><w:t>only</w:t></w:r></w:p><w:sectPr>{LETTER}</w:sectPr>"#
    ));
    let (source, parsed) = parse(&[("word/document.xml", document)])?;
    assert_eq!(parsed.sections.len(), 1);
    let section = &parsed.sections[0];
    assert_eq!(
        section.id,
        source.object_id("sect", "/word/document.xml::body/sectPr[1]")
    );
    assert_eq!(section.section_index, 1);
    assert_eq!(section.start, SectionStart::NextPage);
    assert_eq!(section.columns, 1);
    assert!(parsed.warnings.is_empty(), "{:?}", codes(&parsed));
    Ok(())
}

#[test]
fn headers_and_footers_resolve_variants_inheritance_and_page_fields() -> TestResult {
    let document = body(&format!(
        r#"<w:p><w:pPr><w:sectPr>
            <w:headerReference w:type="default" r:id="rHeader"/>
            <w:headerReference w:type="first" r:id="rCover"/>
            <w:footerReference w:type="default" r:id="rFooter"/>
            <w:titlePg/>{LETTER}</w:sectPr></w:pPr><w:r><w:t>cover</w:t></w:r></w:p>
        <w:p><w:r><w:t>body</w:t></w:r></w:p>
        <w:sectPr><w:footerReference w:type="default" r:id="rSection"/>{LETTER}</w:sectPr>"#
    ));
    let complex_page = r#"<w:p><w:r><w:t xml:space="preserve">Page </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> PAGE \* MERGEFORMAT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r><w:r><w:t xml:space="preserve"> of </w:t></w:r><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>NUMPAGES</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>9</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;
    let (_, parsed) = parse(&[
        ("word/document.xml", document),
        (
            "word/_rels/document.xml.rels",
            relationships(&[
                ("rHeader", HEADER_TYPE, "header1.xml"),
                ("rCover", HEADER_TYPE, "header2.xml"),
                ("rFooter", FOOTER_TYPE, "footer1.xml"),
                ("rSection", FOOTER_TYPE, "footer2.xml"),
            ]),
        ),
        (
            "word/header1.xml",
            part("hdr", r#"<w:p><w:r><w:t>Report</w:t></w:r></w:p>"#),
        ),
        (
            "word/header2.xml",
            part(
                "hdr",
                r#"<w:p><w:r><w:t>Cover</w:t></w:r><w:del w:id="1" w:author="a"><w:r><w:delText>gone</w:delText></w:r></w:del></w:p>"#,
            ),
        ),
        ("word/footer1.xml", part("ftr", complex_page)),
        (
            "word/footer2.xml",
            part(
                "ftr",
                r#"<w:p><w:r><w:t>Section pages </w:t></w:r><w:fldSimple w:instr=" SECTIONPAGES "><w:r><w:t>4</w:t></w:r></w:fldSimple><w:r><w:tab/><w:t>end</w:t></w:r></w:p>"#,
            ),
        ),
        (
            "word/settings.xml",
            format!(r#"<w:settings xmlns:w="{W_NS}"><w:evenAndOddHeaders/></w:settings>"#),
        ),
    ])?;
    let first = &parsed.sections[0];
    let second = &parsed.sections[1];
    assert!(first.even_and_odd_headers && second.even_and_odd_headers);
    assert_eq!(first.header_text.as_deref(), Some("Report"));
    assert_eq!(
        first.footer_text.as_deref(),
        Some("Page [PAGE] of [NUMPAGES]")
    );
    let cover = first
        .header_footer(HeaderFooterKind::Header, HeaderFooterVariant::First)
        .ok_or("first page header")?;
    assert_eq!(cover.text, "Cover");
    assert_eq!(cover.part, "/word/header2.xml");
    assert!(!cover.inherited);

    let inherited = second
        .header_footer(HeaderFooterKind::Header, HeaderFooterVariant::First)
        .ok_or("inherited first page header")?;
    assert!(inherited.inherited);
    assert_eq!(inherited.text, "Cover");
    assert_eq!(second.header_text.as_deref(), Some("Report"));
    let footer = second
        .header_footer(HeaderFooterKind::Footer, HeaderFooterVariant::Default)
        .ok_or("second section footer")?;
    assert!(!footer.inherited);
    assert_eq!(footer.text, "Section pages [SECTIONPAGES]\tend");
    assert!(!second.title_page);
    assert!(parsed.warnings.is_empty(), "{:?}", codes(&parsed));
    Ok(())
}

#[test]
fn unsupported_header_fields_keep_cached_results_with_a_diagnostic() -> TestResult {
    let document = body(&format!(
        r#"<w:p/><w:sectPr><w:footerReference w:type="default" r:id="rFooter"/>{LETTER}</w:sectPr>"#
    ));
    let (_, parsed) = parse(&[
        ("word/document.xml", document),
        (
            "word/_rels/document.xml.rels",
            relationships(&[("rFooter", FOOTER_TYPE, "footer1.xml")]),
        ),
        (
            "word/footer1.xml",
            part(
                "ftr",
                r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>DATE \@ "yyyy"</w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>2026</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#,
            ),
        ),
    ])?;
    assert_eq!(parsed.sections[0].footer_text.as_deref(), Some("2026"));
    let warning = parsed
        .warnings
        .iter()
        .find(|warning| warning.code == "DOCX_HEADER_FOOTER_FIELD_CACHED")
        .ok_or("cached field warning")?;
    assert!(warning.message.contains("DATE"));
    assert!(warning.message.contains("/word/footer1.xml"));
    Ok(())
}

#[test]
fn geometry_and_reference_gaps_are_diagnosed_per_section() -> TestResult {
    let document = body(
        r#"<w:p/><w:sectPr><w:headerReference w:type="default" r:id="rMissing"/><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:gutter="360"/></w:sectPr>"#,
    );
    let (_, parsed) = parse(&[("word/document.xml", document)])?;
    let section = &parsed.sections[0];
    assert!(section.headers_footers.is_empty());
    let warnings = codes(&parsed);
    assert!(warnings.contains(&"DOCX_HEADER_FOOTER_UNRESOLVED"));
    assert!(warnings.contains(&"DOCX_SECTION_GUTTER_IGNORED"));
    assert!(!warnings.contains(&"DOCX_SECTION_GEOMETRY_DEFAULTED"));

    let document = body(r#"<w:p/><w:sectPr><w:pgSz w:w="12240"/></w:sectPr>"#);
    let (_, parsed) = parse(&[("word/document.xml", document)])?;
    let warning = parsed
        .warnings
        .iter()
        .find(|warning| warning.code == "DOCX_SECTION_GEOMETRY_DEFAULTED")
        .ok_or("defaulted geometry warning")?;
    assert!(warning.message.contains("pgSz/@h"));
    assert!(warning.message.contains("pgMar/@top"));
    assert_eq!(warning.object.as_ref(), Some(&parsed.sections[0].id));
    Ok(())
}

#[test]
fn documents_without_a_final_section_synthesize_one_for_trailing_blocks() -> TestResult {
    let document = body(&format!(
        r#"<w:p><w:pPr><w:sectPr>{LETTER}</w:sectPr></w:pPr></w:p><w:p><w:r><w:t>trailing</w:t></w:r></w:p>"#
    ));
    let (source, parsed) = parse(&[("word/document.xml", document)])?;
    assert_eq!(parsed.sections.len(), 2);
    let trailing = &parsed.sections[1];
    assert_eq!(trailing.section_index, 2);
    assert_eq!(
        trailing.last_block_id,
        Some(source.object_id("p", "/word/document.xml::body/p[2]"))
    );
    assert!(codes(&parsed).contains(&"DOCX_SECTION_DEFAULTED"));
    Ok(())
}

#[test]
fn malformed_section_properties_fail_closed() -> TestResult {
    let cases = [
        r#"<w:sectPr><w:type w:val="sideways"/></w:sectPr>"#.to_owned(),
        r#"<w:sectPr><w:cols w:num="0"/></w:sectPr>"#.to_owned(),
        r#"<w:sectPr><w:pgNumType w:start="first"/></w:sectPr>"#.to_owned(),
        r#"<w:sectPr><w:pgSz w:w="wide" w:h="15840"/></w:sectPr>"#.to_owned(),
        r#"<w:sectPr><w:headerReference w:type="odd" r:id="rHeader"/></w:sectPr>"#.to_owned(),
    ];
    for section in cases {
        let document = body(&format!("<w:p/>{section}"));
        let bytes = package(&[("word/document.xml", document)])?;
        let source = DocumentSource::from_bytes(bytes)?;
        assert!(
            matches!(
                parse_docx(&source),
                Err(DocsightError::MalformedDocument { .. })
            ),
            "{section}"
        );
    }
    let document = body(&format!(
        r#"<w:p/><w:sectPr><w:footerReference w:type="default" r:id="rFooter"/>{LETTER}</w:sectPr>"#
    ));
    let bytes = package(&[
        ("word/document.xml", document),
        (
            "word/_rels/document.xml.rels",
            relationships(&[("rFooter", FOOTER_TYPE, "footer1.xml")]),
        ),
        (
            "word/footer1.xml",
            part("ftr", r#"<w:p><w:r><w:fldChar/></w:r></w:p>"#),
        ),
    ])?;
    let source = DocumentSource::from_bytes(bytes)?;
    assert!(matches!(
        parse_docx(&source),
        Err(DocsightError::MalformedDocument { .. })
    ));

    let duplicate = body(&format!(
        r#"<w:p/><w:sectPr><w:footerReference w:type="default" r:id="rFooter"/><w:footerReference w:type="default" r:id="rFooter"/>{LETTER}</w:sectPr>"#
    ));
    let bytes = package(&[
        ("word/document.xml", duplicate),
        (
            "word/_rels/document.xml.rels",
            relationships(&[("rFooter", FOOTER_TYPE, "footer1.xml")]),
        ),
        (
            "word/footer1.xml",
            part("ftr", r#"<w:p><w:r><w:t>footer</w:t></w:r></w:p>"#),
        ),
    ])?;
    let source = DocumentSource::from_bytes(bytes)?;
    assert!(matches!(
        parse_docx(&source),
        Err(DocsightError::MalformedDocument { .. })
    ));

    let unterminated = body(&format!(
        r#"<w:p/><w:sectPr><w:footerReference w:type="default" r:id="rFooter"/>{LETTER}</w:sectPr>"#
    ));
    let bytes = package(&[
        ("word/document.xml", unterminated),
        (
            "word/_rels/document.xml.rels",
            relationships(&[("rFooter", FOOTER_TYPE, "footer1.xml")]),
        ),
        (
            "word/footer1.xml",
            part(
                "ftr",
                r#"<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText>PAGE</w:instrText></w:r></w:p>"#,
            ),
        ),
    ])?;
    let source = DocumentSource::from_bytes(bytes)?;
    assert!(matches!(
        parse_docx(&source),
        Err(DocsightError::MalformedDocument { .. })
    ));
    Ok(())
}

#[test]
fn layout_flags_cascade_through_defaults_styles_and_direct_formatting() -> TestResult {
    let styles = format!(
        r#"<w:styles xmlns:w="{W_NS}">
          <w:docDefaults><w:pPrDefault><w:pPr><w:widowControl w:val="0"/></w:pPr></w:pPrDefault></w:docDefaults>
          <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:pPr><w:keepLines/></w:pPr></w:style>
          <w:style w:type="paragraph" w:styleId="Lead"><w:name w:val="Lead"/><w:basedOn w:val="Normal"/><w:pPr><w:keepNext/><w:pageBreakBefore/></w:pPr></w:style>
        </w:styles>"#
    );
    let document = body(&format!(
        r#"<w:p><w:r><w:t>default style</w:t></w:r></w:p>
        <w:p><w:pPr><w:pStyle w:val="Lead"/></w:pPr><w:r><w:t>lead</w:t></w:r></w:p>
        <w:p><w:pPr><w:pStyle w:val="Lead"/><w:widowControl/><w:keepLines w:val="0"/><w:keepNext w:val="false"/></w:pPr><w:r><w:t>direct</w:t></w:r></w:p>
        <w:sectPr>{LETTER}</w:sectPr>"#
    ));
    let (_, parsed) = parse(&[
        ("word/document.xml", document.clone()),
        ("word/styles.xml", styles),
    ])?;
    let flags: Vec<_> = parsed.blocks.iter().map(|block| block.flags).collect();
    assert!(flags[0].keep_lines && !flags[0].keep_with_next && !flags[0].widow_control);
    assert!(flags[1].keep_lines && flags[1].keep_with_next && flags[1].page_break_before);
    assert!(!flags[1].widow_control);
    assert!(!flags[2].keep_lines && !flags[2].keep_with_next && flags[2].widow_control);
    assert!(flags[2].page_break_before);

    let (_, unstyled) = parse(&[("word/document.xml", document)])?;
    assert!(
        unstyled
            .blocks
            .iter()
            .all(|block| block.flags.widow_control)
    );
    assert!(
        unstyled
            .blocks
            .iter()
            .all(|block| !block.flags.keep_lines && !block.flags.keep_with_next)
    );
    Ok(())
}
