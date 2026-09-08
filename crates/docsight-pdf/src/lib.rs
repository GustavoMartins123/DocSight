mod content;
mod font;
mod raster;
mod reconstruction;
mod syntax;

use content::{DisplayCommand, TextRun, fonts_from_resources, parse_content};
use docsight_core::{
    Diagnostic, DiagnosticSeverity, DocsightError, Document, DocumentFormat, DocumentMetadata,
    DocumentSource, ObjectId, Page, Rect, SourceSpan,
};
use raster::{MAX_DPI, MIN_DPI};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Take};
use syntax::{ObjectRef, StreamValue, Value, Xref, malformed, parse_object, parse_xref};

pub const ENGINE_NAME: &str = "docsight-pdf-native";
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MAX_PAGES: u32 = 10_000;
pub use raster::MIN_DPI as PDF_MIN_DPI;
pub use raster::{
    MAX_DPI as PDF_MAX_DPI, MAX_RASTER_PIXELS as PDF_MAX_RASTER_PIXELS,
    glyph_coverage as pdf_glyph_coverage,
};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PdfPageInfo {
    pub number: u32,
    pub width_pt: f32,
    pub height_pt: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PdfInfo {
    pub engine: &'static str,
    pub engine_version: &'static str,
    pub page_count: u32,
    pub pages: Vec<PdfPageInfo>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PdfTextSpan {
    pub id: ObjectId,
    pub text: String,
    pub bbox: Rect,
    pub reading_order: u32,
    pub font_size_pt: f32,
    pub font_name: String,
    pub baseline_y_pt: f32,
    pub argb: u32,
    pub bold: bool,
    pub clipped: bool,
    pub synthetic: bool,
    pub confidence: f32,
    pub source: SourceSpan,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PdfPage {
    pub number: u32,
    pub width_pt: f32,
    pub height_pt: f32,
    pub spans: Vec<PdfTextSpan>,
    #[serde(skip_serializing)]
    pub warnings: Vec<Diagnostic>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RasterizedPage {
    pub page: u32,
    pub dpi: u16,
    pub bbox: Rect,
    pub width_px: u32,
    pub height_px: u32,
    pub png: Vec<u8>,
    pub pixels: Vec<u8>,
    pub warnings: Vec<Diagnostic>,
}

pub struct PdfDocument<'a> {
    source: &'a DocumentSource,
    store: ObjectStore<'a>,
    pages: Vec<PageRecord>,
}

impl<'a> PdfDocument<'a> {
    pub fn open(source: &'a DocumentSource) -> Result<Self, DocsightError> {
        if source.format() != DocumentFormat::Pdf {
            return Err(DocsightError::UnsupportedOperation {
                operation: "PDF parsing".to_owned(),
                format: source.format(),
            });
        }
        let xref = parse_xref(source.bytes())?;
        if xref.trailer.contains_key("Encrypt") {
            return Err(DocsightError::EncryptedDocument);
        }
        let root = match xref.trailer.get("Root") {
            Some(Value::Ref(reference)) => *reference,
            _ => return Err(malformed("trailer has no indirect Root reference")),
        };
        let store = ObjectStore {
            bytes: source.bytes(),
            xref,
        };
        let mut pages = Vec::new();
        let mut active = BTreeSet::new();
        let catalog = store.resolve_dict(&Value::Ref(root))?;
        if !matches!(catalog.get("Type"), Some(Value::Name(value)) if value == "Catalog") {
            return Err(malformed("Root object is not a Catalog"));
        }
        let pages_root = match catalog.get("Pages") {
            Some(Value::Ref(reference)) => *reference,
            _ => return Err(malformed("Catalog has no indirect Pages reference")),
        };
        collect_pages(
            &store,
            pages_root,
            InheritedPage::default(),
            0,
            &mut active,
            &mut pages,
        )?;
        if pages.len() > MAX_PAGES as usize {
            return Err(DocsightError::ResourceLimit {
                resource: "PDF pages".to_owned(),
                limit: u64::from(MAX_PAGES),
            });
        }
        Ok(Self {
            source,
            store,
            pages,
        })
    }

    pub fn page_count(&self) -> u32 {
        self.pages.len() as u32
    }

    pub fn info(&self) -> Result<PdfInfo, DocsightError> {
        let pages = self
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| {
                let number = u32::try_from(index + 1).map_err(|_| page_limit())?;
                Ok(PdfPageInfo {
                    number,
                    width_pt: page.media_box.width(),
                    height_pt: page.media_box.height(),
                })
            })
            .collect::<Result<Vec<_>, DocsightError>>()?;
        Ok(PdfInfo {
            engine: ENGINE_NAME,
            engine_version: ENGINE_VERSION,
            page_count: self.page_count(),
            pages,
        })
    }

    pub fn page(&self, number: u32) -> Result<PdfPage, DocsightError> {
        let parsed = self.parse_page(number)?;
        let spans = parsed
            .text_runs
            .into_iter()
            .enumerate()
            .map(|(index, run)| self.make_span(number, index, run, parsed.approximated_font))
            .collect::<Result<Vec<_>, DocsightError>>()?;
        let mut warnings = Vec::new();
        if parsed.approximated_font {
            warnings.push(font_approximation_warning(number));
        }
        if parsed.approximated_graphics {
            warnings.push(graphics_approximation_warning(number));
        }
        if parsed.omitted_xobjects {
            warnings.push(xobject_placeholder_warning(number));
        }
        let page = self.page_record(number)?;
        Ok(PdfPage {
            number,
            width_pt: page.media_box.width(),
            height_pt: page.media_box.height(),
            spans,
            warnings,
        })
    }

    pub fn locate_span(&self, object: &str) -> Result<(u32, Rect), DocsightError> {
        for page_number in 1..=self.page_count() {
            let page = self.page(page_number)?;
            if let Some(span) = page.spans.iter().find(|span| span.id.as_str() == object) {
                return Ok((page_number, span.bbox));
            }
        }
        Err(DocsightError::ObjectNotFound {
            object: object.to_owned(),
        })
    }

    pub fn to_document(&self) -> Result<Document, DocsightError> {
        let mut blocks = Vec::new();
        let mut pages = Vec::new();
        let mut all_warnings = Vec::new();
        let mut global_reading_order = 0_u32;

        for page_num in 1..=self.page_count() {
            let page_record = self.page_record(page_num)?;
            let parsed = self.parse_page(page_num)?;
            if parsed.approximated_font {
                all_warnings.push(font_approximation_warning(page_num));
            }
            if parsed.approximated_graphics {
                all_warnings.push(graphics_approximation_warning(page_num));
            }
            if parsed.omitted_xobjects {
                all_warnings.push(xobject_placeholder_warning(page_num));
            }

            let reconstructed = reconstruction::reconstruct_page_semantics(
                page_num,
                self.source.sha256(),
                page_record.media_box.width(),
                page_record.media_box.height(),
                &parsed.text_runs,
                &parsed.commands,
                &mut global_reading_order,
            )?;
            blocks.extend(reconstructed.blocks);

            pages.push(Page {
                number: page_num,
                width_pt: page_record.media_box.width(),
                height_pt: page_record.media_box.height(),
                block_ids: reconstructed.block_ids,
                overlays: Vec::new(),
            });
        }

        Ok(Document {
            id: self.source.id(),
            sha256: self.source.sha256().to_owned(),
            format: DocumentFormat::Pdf,
            size_bytes: self.source.size_bytes(),
            metadata: DocumentMetadata {
                title: None,
                author: None,
                subject: None,
                producer: Some(ENGINE_NAME.to_owned()),
            },
            styles: Vec::new(),
            sections: Vec::new(),
            pages,
            blocks,
            resources: Vec::new(),
            links: Vec::new(),
            comments: Vec::new(),
            tracked_changes: docsight_core::TrackedChanges::default(),
            warnings: all_warnings,
        })
    }

    pub fn rasterize(
        &self,
        number: u32,
        dpi: u16,
        bbox: Option<Rect>,
    ) -> Result<RasterizedPage, DocsightError> {
        if !(MIN_DPI..=MAX_DPI).contains(&dpi) {
            return Err(DocsightError::InvalidArgument {
                message: format!("DPI must be between {MIN_DPI} and {MAX_DPI}"),
            });
        }
        let page = self.page_record(number)?;
        let public_page = Rect::new(0.0, 0.0, page.media_box.width(), page.media_box.height())?;
        let target = match bbox {
            Some(bbox) => bbox,
            None => public_page,
        };
        let parsed = self.parse_page(number)?;
        let mut warnings = vec![renderer_warning(number)];
        if parsed.approximated_font {
            warnings.push(font_approximation_warning(number));
        }
        if parsed.approximated_graphics {
            warnings.push(graphics_approximation_warning(number));
        }
        if parsed.omitted_xobjects {
            warnings.push(xobject_placeholder_warning(number));
        }
        let raster = raster::rasterize(&parsed.commands, public_page, target, dpi)?;
        Ok(RasterizedPage {
            page: number,
            dpi,
            bbox: target,
            width_px: raster.width,
            height_px: raster.height,
            png: raster.png,
            pixels: raster.pixels,
            warnings,
        })
    }

    fn parse_page(&self, number: u32) -> Result<ParsedPage, DocsightError> {
        let page = self.page_record(number)?;
        let resources = match page.resources.as_ref() {
            Some(value) => self.store.resolve_dict(value)?,
            None if page.contents.is_empty() => BTreeMap::new(),
            None => {
                return Err(malformed(format!(
                    "page {number} has no Resources dictionary"
                )));
            }
        };
        let fonts = fonts_from_resources(
            &resources,
            |value| self.store.resolve(value),
            |value| {
                let value = self.store.resolve(value)?;
                match value {
                    Value::Stream(stream) => decode_stream(&stream),
                    _ => Err(malformed("ToUnicode must resolve to a stream")),
                }
            },
        )?;
        let xobjects = match resources.get("XObject") {
            Some(value) => self.store.resolve_dict(value)?.into_keys().collect(),
            None => BTreeSet::new(),
        };
        let content = self.read_content_streams(&page.contents)?;
        let parsed = parse_content(
            &content,
            page.media_box.x0,
            page.media_box.y1,
            &fonts,
            &xobjects,
        )?;
        Ok(ParsedPage {
            commands: parsed.commands,
            text_runs: parsed.text_runs,
            approximated_font: parsed.approximated_font,
            approximated_graphics: parsed.approximated_graphics,
            omitted_xobjects: parsed.omitted_xobjects,
        })
    }

    fn read_content_streams(&self, contents: &[Value]) -> Result<Vec<u8>, DocsightError> {
        let mut combined = Vec::new();
        for value in contents {
            let value = self.store.resolve(value)?;
            let stream = match value {
                Value::Stream(stream) => stream,
                _ => return Err(malformed("Contents must resolve to a stream")),
            };
            let data = decode_stream(&stream)?;
            let projected = combined
                .len()
                .checked_add(data.len())
                .and_then(|length| length.checked_add(1))
                .ok_or_else(content_limit)?;
            if projected > docsight_core::MAX_INSPECT_BYTES as usize {
                return Err(content_limit());
            }
            combined.extend_from_slice(&data);
            combined.push(b'\n');
        }
        Ok(combined)
    }

    fn page_record(&self, number: u32) -> Result<&PageRecord, DocsightError> {
        let zero_based = number
            .checked_sub(1)
            .ok_or_else(|| DocsightError::ObjectNotFound {
                object: format!("page {number}"),
            })?;
        let index = usize::try_from(zero_based).map_err(|_| DocsightError::ObjectNotFound {
            object: format!("page {number}"),
        })?;
        self.pages
            .get(index)
            .ok_or_else(|| DocsightError::ObjectNotFound {
                object: format!("page {number}"),
            })
    }

    fn make_span(
        &self,
        page: u32,
        index: usize,
        run: TextRun,
        approximated: bool,
    ) -> Result<PdfTextSpan, DocsightError> {
        let ordinal = u32::try_from(index + 1).map_err(|_| DocsightError::ResourceLimit {
            resource: format!("text spans on PDF page {page}"),
            limit: u64::from(u32::MAX),
        })?;
        let source_path = format!("/pdf/page[{page}]/content/span[{ordinal}]");
        Ok(PdfTextSpan {
            id: self.source.object_id("span", &source_path),
            text: run.text,
            bbox: run.bbox,
            reading_order: ordinal,
            font_size_pt: run.font_size,
            font_name: run.font_name,
            baseline_y_pt: run.baseline_y,
            argb: run.argb,
            bold: run.bold,
            clipped: !run.clips.is_empty(),
            synthetic: false,
            confidence: if approximated { 0.75 } else { 1.0 },
            source: SourceSpan::with_range(source_path, run.source_offset, run.source_length),
        })
    }
}

struct ParsedPage {
    commands: Vec<DisplayCommand>,
    text_runs: Vec<TextRun>,
    approximated_font: bool,
    approximated_graphics: bool,
    omitted_xobjects: bool,
}

struct ObjectStore<'a> {
    bytes: &'a [u8],
    xref: Xref,
}

impl ObjectStore<'_> {
    fn object(&self, reference: ObjectRef) -> Result<Value, DocsightError> {
        let offset = self.xref.entries.get(&reference).copied().ok_or_else(|| {
            malformed(format!(
                "missing xref entry for object {}",
                reference.number
            ))
        })?;
        parse_object(self.bytes, offset, reference, &self.xref.entries)
    }

    fn resolve(&self, value: &Value) -> Result<Value, DocsightError> {
        let mut resolved = value.clone();
        for _ in 0..64 {
            match resolved {
                Value::Ref(reference) => resolved = self.object(reference)?,
                _ => return Ok(resolved),
            }
        }
        Err(DocsightError::ResourceLimit {
            resource: "PDF indirect reference depth".to_owned(),
            limit: 64,
        })
    }

    fn resolve_dict(&self, value: &Value) -> Result<BTreeMap<String, Value>, DocsightError> {
        match self.resolve(value)? {
            Value::Dict(dict) => Ok(dict),
            _ => Err(malformed("object must resolve to a dictionary")),
        }
    }
}

#[derive(Clone, Default)]
struct InheritedPage {
    media_box: Option<NativeBox>,
    resources: Option<Value>,
}

struct PageRecord {
    media_box: NativeBox,
    resources: Option<Value>,
    contents: Vec<Value>,
}

#[derive(Clone, Copy)]
struct NativeBox {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl NativeBox {
    fn width(self) -> f32 {
        self.x1 - self.x0
    }

    fn height(self) -> f32 {
        self.y1 - self.y0
    }
}

fn collect_pages(
    store: &ObjectStore<'_>,
    reference: ObjectRef,
    inherited: InheritedPage,
    depth: usize,
    active: &mut BTreeSet<ObjectRef>,
    pages: &mut Vec<PageRecord>,
) -> Result<(), DocsightError> {
    if depth > 64 {
        return Err(DocsightError::ResourceLimit {
            resource: "PDF page tree depth".to_owned(),
            limit: 64,
        });
    }
    if !active.insert(reference) {
        return Err(malformed("cycle detected in PDF page tree"));
    }
    let dict = store.resolve_dict(&Value::Ref(reference))?;
    let node_type = match dict.get("Type") {
        Some(Value::Name(value)) => value.as_str(),
        _ => return Err(malformed("page tree node has no valid Type")),
    };
    let mut current = inherited;
    if let Some(value) = dict.get("MediaBox") {
        current.media_box = Some(parse_box(store, value)?);
    }
    if let Some(value) = dict.get("Resources") {
        current.resources = Some(value.clone());
    }
    match node_type {
        "Pages" => {
            let kids = match dict.get("Kids") {
                Some(Value::Array(values)) => values,
                _ => return Err(malformed("Pages node has no Kids array")),
            };
            for kid in kids {
                let kid = match kid {
                    Value::Ref(reference) => *reference,
                    _ => return Err(malformed("page tree Kid must be an indirect reference")),
                };
                collect_pages(store, kid, current.clone(), depth + 1, active, pages)?;
                if pages.len() > MAX_PAGES as usize {
                    return Err(page_limit());
                }
            }
        }
        "Page" => {
            let media_box = current
                .media_box
                .ok_or_else(|| malformed("Page has no inherited MediaBox"))?;
            let effective_box = match dict.get("CropBox") {
                Some(value) => parse_box(store, value)?,
                None => media_box,
            };
            let contents = match dict.get("Contents") {
                None => Vec::new(),
                Some(Value::Array(values)) => values.clone(),
                Some(value) => vec![value.clone()],
            };
            pages.push(PageRecord {
                media_box: effective_box,
                resources: current.resources,
                contents,
            });
        }
        _ => return Err(malformed("invalid page tree node Type")),
    }
    active.remove(&reference);
    Ok(())
}

fn parse_box(store: &ObjectStore<'_>, value: &Value) -> Result<NativeBox, DocsightError> {
    let value = store.resolve(value)?;
    let values = match value {
        Value::Array(values) if values.len() == 4 => values,
        _ => return Err(malformed("page box must be an array of four numbers")),
    };
    let x0 = pdf_number(&values[0])?;
    let y0 = pdf_number(&values[1])?;
    let x1 = pdf_number(&values[2])?;
    let y1 = pdf_number(&values[3])?;
    if !x0.is_finite()
        || !y0.is_finite()
        || !x1.is_finite()
        || !y1.is_finite()
        || x0 >= x1
        || y0 >= y1
    {
        return Err(malformed("page box has invalid geometry"));
    }
    Ok(NativeBox { x0, y0, x1, y1 })
}

fn pdf_number(value: &Value) -> Result<f32, DocsightError> {
    match value {
        Value::Int(value) => Ok(*value as f32),
        Value::Real(value) => Ok(*value),
        _ => Err(malformed("page box coordinate must be numeric")),
    }
}

const MAX_FLATE_OUTPUT_BYTES: u64 = docsight_core::MAX_INSPECT_BYTES;

fn decode_stream(stream: &StreamValue) -> Result<Vec<u8>, DocsightError> {
    if stream
        .dict
        .get("DecodeParms")
        .is_some_and(|value| !matches!(value, Value::Null))
    {
        return Err(DocsightError::UnsupportedFeature {
            feature: "PDF stream DecodeParms predictors".to_owned(),
        });
    }
    let Some(filter) = stream.dict.get("Filter") else {
        return Ok(stream.data.clone());
    };
    match filter {
        Value::Name(name) if name == "FlateDecode" => inflate_zlib(&stream.data),
        Value::Array(values) if values.len() == 1 => match &values[0] {
            Value::Name(name) if name == "FlateDecode" => inflate_zlib(&stream.data),
            Value::Name(name) => Err(DocsightError::UnsupportedFeature {
                feature: format!("PDF stream filter {name}"),
            }),
            _ => Err(malformed("stream Filter array must contain a name")),
        },
        Value::Array(_) => Err(DocsightError::UnsupportedFeature {
            feature: "PDF stream filter chains".to_owned(),
        }),
        Value::Name(name) => Err(DocsightError::UnsupportedFeature {
            feature: format!("PDF stream filter {name}"),
        }),
        _ => Err(malformed("stream Filter must be a name")),
    }
}

fn inflate_zlib(data: &[u8]) -> Result<Vec<u8>, DocsightError> {
    let bounded: Take<flate2::read::ZlibDecoder<&[u8]>> =
        flate2::read::ZlibDecoder::new(data).take(MAX_FLATE_OUTPUT_BYTES + 1);
    let mut decoder = bounded;
    let mut output = Vec::new();
    decoder
        .read_to_end(&mut output)
        .map_err(|error| DocsightError::MalformedDocument {
            message: format!("FlateDecode stream is invalid: {error}"),
        })?;
    if output.len() as u64 > MAX_FLATE_OUTPUT_BYTES {
        return Err(content_limit());
    }
    Ok(output)
}

fn font_approximation_warning(page: u32) -> Diagnostic {
    Diagnostic {
        code: "APPROXIMATED_PDF_FONT".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!("page {page} contains text without an embedded TrueType outline"),
        effect: "the renderer uses the declared deterministic fallback glyph set for that font"
            .to_owned(),
        object: None,
        page: Some(page),
    }
}

fn renderer_warning(page: u32) -> Diagnostic {
    Diagnostic {
        code: "INITIAL_PDF_RASTERIZER".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!("page {page} was rasterized by DOCSIGHT's deterministic native renderer"),
        effect: "vector fills and strokes use deterministic pixel coverage without full PDF antialiasing; embedded font outlines use coverage rasterization".to_owned(),
        object: None,
        page: Some(page),
    }
}

fn graphics_approximation_warning(page: u32) -> Diagnostic {
    Diagnostic {
        code: "PDF_GRAPHICS_STYLE_APPROXIMATED".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "page {page} uses PDF graphics state features with approximate raster semantics"
        ),
        effect: "line caps, joins, dash patterns, flatness, intents, transparency, or shading may differ or be omitted".to_owned(),
        object: None,
        page: Some(page),
    }
}

fn xobject_placeholder_warning(page: u32) -> Diagnostic {
    Diagnostic {
        code: "PDF_XOBJECT_PLACEHOLDER".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!(
            "page {page} contains XObject content represented as a figure placeholder"
        ),
        effect: "embedded image or form pixels are not decoded by the initial renderer".to_owned(),
        object: None,
        page: Some(page),
    }
}

fn page_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "PDF pages".to_owned(),
        limit: u64::from(MAX_PAGES),
    }
}

fn content_limit() -> DocsightError {
    DocsightError::ResourceLimit {
        resource: "decoded PDF content bytes".to_owned(),
        limit: docsight_core::MAX_INSPECT_BYTES,
    }
}

pub fn parse_pdf(source: &DocumentSource) -> Result<Document, DocsightError> {
    let pdf = PdfDocument::open(source)?;
    pdf.to_document()
}
