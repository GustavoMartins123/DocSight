use crate::release::{
    archive::{extract_verified, verify_archive},
    native_target,
};
use crate::smoke::validate_png_bytes;
use crate::tooling::common::*;
use crate::tooling::process::{ProcessLimits, ProcessResult, Runner, isolated_environment};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const RENDER_PAGE: u32 = 1;
pub const RENDER_DPI: u32 = 72;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(45);
const COMMAND_OUTPUT_BYTES: usize = 16_777_216;
const MAX_PNG_BYTES: u64 = 67_108_864;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Structure {
    pub pages: u64,
    pub paragraphs: u64,
    pub headings: u64,
    pub tables: u64,
    pub figures: u64,
    pub blocks_by_kind: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TextSummary {
    pub blocks: u64,
    pub characters: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Geometry {
    pub page: u32,
    pub width_pt: f64,
    pub height_pt: f64,
    pub spans: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RenderSummary {
    pub page: u32,
    pub dpi: u32,
    pub width_px: u64,
    pub height_px: u64,
    pub png_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DiffSummary {
    pub semantic_changes: u64,
    pub layout_changed_pages: u64,
    pub pages_before: u64,
    pub pages_after: u64,
    pub tables: BTreeMap<String, u64>,
    pub images: BTreeMap<String, u64>,
}

/// What the engine reports for one document, reduced to the facts a reviewer can check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub structure: Structure,
    pub text: TextSummary,
    pub geometry: Geometry,
    pub diagnostics: Vec<String>,
    pub render: RenderSummary,
}

pub struct EngineSession<'a, R: Runner> {
    runner: &'a mut R,
    binary: PathBuf,
    work: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    pub version: String,
    pub revision: String,
    pub target: String,
    pub archive_sha256: String,
    _temporary: tempfile::TempDir,
}

impl<'a, R: Runner> EngineSession<'a, R> {
    pub fn open(archive: &Path, runner: &'a mut R) -> Result<Self> {
        let manifest = verify_archive(archive)?;
        require(
            manifest.target == native_target()?,
            "QUALITY_HOST_MISMATCH",
            "Quality measurement requires a native archive",
        )?;
        let archive_sha256 = sha256_file(archive)?;
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().canonicalize()?;
        let (binary, manifest) = extract_verified(archive, &root.join("package"))?;
        let work = root.join("work");
        fs::create_dir(&work)?;
        let environment = isolated_environment()?;
        let version = runner.run(
            &[binary.as_os_str().to_owned(), "--version".into()],
            &work,
            &ProcessLimits {
                timeout: Duration::from_secs(10),
                output_bytes: 4096,
            },
            Some(&environment),
        )?;
        require(
            version.termination.is_none()
                && version.returncode == 0
                && version.stderr.is_empty()
                && text(&version.stdout)?
                    .trim()
                    .starts_with(&format!("docsight {}", manifest.version)),
            "QUALITY_VERSION_MISMATCH",
            "Executable version differs from archive manifest",
        )?;
        Ok(Self {
            runner,
            binary,
            work,
            environment,
            version: manifest.version,
            revision: manifest.revision,
            target: manifest.target,
            archive_sha256,
            _temporary: temporary,
        })
    }

    pub fn run(&mut self, arguments: &[OsString]) -> Result<ProcessResult> {
        let mut command = vec![
            self.binary.as_os_str().to_owned(),
            "--agent".into(),
            "--sandbox".into(),
        ];
        command.extend(arguments.iter().cloned());
        let result = self.runner.run(
            &command,
            &self.work,
            &ProcessLimits {
                timeout: COMMAND_TIMEOUT,
                output_bytes: COMMAND_OUTPUT_BYTES,
            },
            Some(&self.environment),
        )?;
        require(
            result.termination.is_none() && (0..=255).contains(&result.returncode),
            "QUALITY_PROCESS_FAILURE",
            "Engine command exceeded its budget or terminated abnormally",
        )?;
        Ok(result)
    }

    pub fn agent(&mut self, arguments: &[OsString]) -> Result<Value> {
        let result = self.run(arguments)?;
        require(
            result.returncode == 0 && result.stderr.is_empty(),
            "QUALITY_ENGINE_ERROR",
            "Engine command failed on a document that is expected to succeed",
        )?;
        let value = parse_json(&result.stdout)?;
        require(
            value.get("schema").and_then(Value::as_str) == Some("docsight.agent/v2")
                && value.get("result").is_some_and(Value::is_object)
                && value
                    .pointer("/limits/truncated")
                    .is_none_or(|truncated| truncated == &Value::Bool(false)),
            "QUALITY_PROTOCOL",
            "Engine output is not a complete agent result envelope",
        )?;
        Ok(value)
    }

    pub fn observe(&mut self, document: &Path) -> Result<Observation> {
        let path: OsString = document.as_os_str().to_owned();
        let inspect = self.agent(&["inspect".into(), path.clone()])?;
        let listing = self.agent(&["text".into(), path.clone()])?;
        let page = self.agent(&["page".into(), path.clone(), RENDER_PAGE.to_string().into()])?;
        let output = self.work.join("quality-render.png");
        if output.try_exists()? {
            fs::remove_file(&output)?;
        }
        let render = self.agent(&[
            "render".into(),
            path,
            "--page".into(),
            RENDER_PAGE.to_string().into(),
            "--dpi".into(),
            RENDER_DPI.to_string().into(),
            "--out".into(),
            output.as_os_str().to_owned(),
        ])?;
        let png = read_bytes(&output, MAX_PNG_BYTES)?;
        validate_png_bytes(&png)?;
        fs::remove_file(&output)?;
        Ok(Observation {
            structure: structure(&inspect)?,
            text: text_summary(&listing)?,
            geometry: geometry(&page)?,
            diagnostics: warning_codes(&inspect)?,
            render: RenderSummary {
                page: RENDER_PAGE,
                dpi: RENDER_DPI,
                width_px: count(&render, "/result/width_px")?,
                height_px: count(&render, "/result/height_px")?,
                png_sha256: digest(&png),
            },
        })
    }

    pub fn diff(&mut self, before: &Path, after: &Path) -> Result<DiffSummary> {
        let value = self.agent(&[
            "diff".into(),
            before.as_os_str().to_owned(),
            after.as_os_str().to_owned(),
        ])?;
        let summary = value
            .pointer("/result/summary")
            .ok_or_else(protocol_error)?;
        Ok(DiffSummary {
            semantic_changes: count(summary, "/semantic_changes")?,
            layout_changed_pages: count(summary, "/layout_changed_pages")?,
            pages_before: count(summary, "/pages_before")?,
            pages_after: count(summary, "/pages_after")?,
            tables: counters(summary, "/tables")?,
            images: counters(summary, "/images")?,
        })
    }
}

/// The measured values a document class declares signals for.
pub fn signal_values(structure: &Structure) -> BTreeMap<String, u64> {
    BTreeMap::from([
        ("pages".to_owned(), structure.pages),
        ("paragraphs".to_owned(), structure.paragraphs),
        ("headings".to_owned(), structure.headings),
        ("tables".to_owned(), structure.tables),
        ("figures".to_owned(), structure.figures),
    ])
}

/// A page raster covers the page it renders to within one pixel on each axis.
pub fn render_covers_page(render: &RenderSummary, geometry: &Geometry) -> bool {
    let scale = f64::from(render.dpi) / 72.0;
    let covers = |pixels: u64, points: f64| {
        let exact = points * scale;
        exact.is_finite() && (pixels as f64 - exact).abs() <= 1.0
    };
    render.page == geometry.page
        && covers(render.width_px, geometry.width_pt)
        && covers(render.height_px, geometry.height_pt)
}

fn structure(inspect: &Value) -> Result<Structure> {
    let blocks_by_kind = inspect
        .pointer("/result/blocks_by_kind")
        .and_then(Value::as_object)
        .ok_or_else(protocol_error)?
        .iter()
        .map(|(kind, value)| {
            value
                .as_u64()
                .map(|count| (kind.clone(), count))
                .ok_or_else(protocol_error)
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(Structure {
        pages: count(inspect, "/result/pages")?,
        paragraphs: count(inspect, "/result/paragraphs")?,
        headings: count(inspect, "/result/headings")?,
        tables: count(inspect, "/result/tables")?,
        figures: count(inspect, "/result/figures")?,
        blocks_by_kind,
    })
}

fn text_summary(listing: &Value) -> Result<TextSummary> {
    let blocks = listing
        .pointer("/result/blocks")
        .and_then(Value::as_array)
        .ok_or_else(protocol_error)?;
    let mut canonical = String::new();
    let mut characters = 0u64;
    for block in blocks {
        let kind = block
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(protocol_error)?;
        let content = block
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(protocol_error)?;
        characters = characters.saturating_add(content.chars().count() as u64);
        canonical.push_str(kind);
        canonical.push('\t');
        canonical.push_str(content);
        canonical.push('\n');
    }
    Ok(TextSummary {
        blocks: blocks.len() as u64,
        characters,
        sha256: digest(canonical.as_bytes()),
    })
}

fn geometry(page: &Value) -> Result<Geometry> {
    let number = |pointer: &str| {
        page.pointer(pointer)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(protocol_error)
    };
    Ok(Geometry {
        page: u32::try_from(count(page, "/result/number")?).map_err(|_| protocol_error())?,
        width_pt: number("/result/width_pt")?,
        height_pt: number("/result/height_pt")?,
        spans: page
            .pointer("/result/spans")
            .and_then(Value::as_array)
            .map(|spans| spans.len() as u64)
            .ok_or_else(protocol_error)?,
    })
}

fn warning_codes(value: &Value) -> Result<Vec<String>> {
    let mut codes = BTreeSet::new();
    if let Some(warnings) = value.get("warnings") {
        for warning in array(warnings)? {
            codes.insert(
                warning
                    .get("code")
                    .and_then(Value::as_str)
                    .ok_or_else(protocol_error)?
                    .to_owned(),
            );
        }
    }
    Ok(codes.into_iter().collect())
}

fn count(value: &Value, pointer: &str) -> Result<u64> {
    match value.pointer(pointer) {
        Some(Value::Null) => Ok(0),
        Some(number) => number.as_u64().ok_or_else(protocol_error),
        None => Err(protocol_error()),
    }
}

fn counters(value: &Value, pointer: &str) -> Result<BTreeMap<String, u64>> {
    value
        .pointer(pointer)
        .and_then(Value::as_object)
        .ok_or_else(protocol_error)?
        .iter()
        .map(|(name, value)| {
            value
                .as_u64()
                .map(|count| (name.clone(), count))
                .ok_or_else(protocol_error)
        })
        .collect()
}

fn protocol_error() -> ToolError {
    ToolError::new(
        "QUALITY_PROTOCOL",
        "Engine output lacks a field the quality measurement requires",
    )
}
