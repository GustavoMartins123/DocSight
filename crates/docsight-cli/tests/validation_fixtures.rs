use docsight_core::{Document, DocumentSource};
use std::path::{Path, PathBuf};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn validation_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/validation")
}

fn project_reference(text: &str) -> Option<String> {
    let normalized = text.to_lowercase();
    for phrase in [
        "docsight",
        "project specification",
        "implementation plan",
        "implementation phase",
        "milestone",
        "roadmap",
        "plano de implementação",
        "fase de implementação",
    ] {
        if normalized.contains(phrase) {
            return Some(phrase.to_owned());
        }
    }
    normalized
        .split(|character: char| !character.is_ascii_alphanumeric())
        .find(|token| {
            token
                .strip_prefix("ds")
                .or_else(|| token.strip_prefix('m'))
                .is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                })
        })
        .map(str::to_owned)
}

fn document_text(document: &Document) -> String {
    let mut fragments: Vec<String> = document.blocks.iter().map(|block| block.text()).collect();
    for section in &document.sections {
        fragments.extend(
            section
                .headers_footers
                .iter()
                .map(|entry| entry.text.clone()),
        );
    }
    for text in [
        &document.metadata.title,
        &document.metadata.author,
        &document.metadata.subject,
    ]
    .into_iter()
    .flatten()
    {
        fragments.push(text.clone());
    }
    fragments.join("\n")
}

fn assert_generic_document(path: &Path) -> TestResult {
    let source = DocumentSource::open(path)?;
    let document = docsight_ingest::ingest(&source)?;
    let text = document_text(&document);
    assert!(
        !text.trim().is_empty(),
        "{} has no sample text",
        path.display()
    );
    assert!(
        project_reference(&text).is_none(),
        "{} contains project-specific validation data: {:?}",
        path.display(),
        project_reference(&text)
    );
    Ok(())
}

#[test]
fn validation_documents_use_generic_fictional_content() -> TestResult {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(validation_directory())? {
        let path = entry?.path();
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        {
            continue;
        }
        if path.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("docx") || extension.eq_ignore_ascii_case("pdf")
        }) {
            paths.push(path);
        }
    }
    paths.sort();
    assert!(paths.len() >= 10);
    for path in paths {
        assert_generic_document(&path)?;
    }
    Ok(())
}

#[test]
fn generic_content_guard_rejects_project_phases_and_specification_text() {
    for text in [
        "Project specification",
        "DOCSIGHT internal notes",
        "Implementation phase",
        "Milestone M12",
        "Review DS17",
        "M0 through M9",
        "Fase de implementação",
    ] {
        assert!(project_reference(text).is_some(), "{text}");
    }
    for text in [
        "Regional Facilities Survey",
        "Condition C1 and inventory A2",
        "North building review notes",
        "Quarterly revenue and expenses",
    ] {
        assert!(project_reference(text).is_none(), "{text}");
    }
}

#[test]
fn generic_report_keeps_long_document_and_structural_coverage() -> TestResult {
    let source = DocumentSource::open(validation_directory().join("sample_report.docx"))?;
    let document = docsight_ingest::ingest(&source)?;
    assert!(document.pages.len() >= 9);
    assert_eq!(document.tables().count(), 16);
    assert_eq!(document.headings().count(), 64);
    assert!(document.blocks.len() > 100);
    assert!(
        document
            .sections
            .iter()
            .any(|section| !section.headers_footers.is_empty())
    );
    assert!(
        document
            .blocks
            .iter()
            .any(|block| matches!(block.content, docsight_core::BlockContent::ListItem(_)))
    );
    Ok(())
}
