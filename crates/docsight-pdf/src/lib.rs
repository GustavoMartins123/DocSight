mod content;
mod filters;
mod font;
mod raster;
mod reconstruction;
mod syntax;

use content::{
    ClipRegion, ContentResources, DisplayCommand, ExtGraphicsState, FontInfo, FormXObject, LineCap,
    LineJoin, MAX_FORM_XOBJECT_DEPTH, Paint, PathSegment, Point, TextRun, VisualIssue,
    XObjectEntry, ext_graphics_states_from_resources, fonts_from_resources, parse_content,
};
use docsight_core::{
    Diagnostic, DiagnosticSeverity, DocsightError, Document, DocumentFormat, DocumentMetadata,
    DocumentSource, ErrorLocation, Hyperlink, IrVersion, ObjectId, Overlay, OverlayKind, Page,
    Rect, SourceSpan, validate_canonical,
};
use filters::decode_stream;
use raster::{MAX_DPI, MIN_DPI};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use syntax::{ObjectRef, Value, Xref, XrefEntry, malformed, parse_object, parse_xref};

pub const ENGINE_NAME: &str = "docsight-pdf-native";
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MAX_PAGES: u32 = 10_000;
pub const MAX_ANNOTATIONS: u32 = 4_096;
pub use content::MAX_OPERATIONS as PDF_MAX_CONTENT_OPERATIONS;
pub use font::MAX_FONT_BYTES as PDF_MAX_FONT_BYTES;
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

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfTracePoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PdfTracePathSegment {
    Move {
        point: PdfTracePoint,
    },
    Line {
        point: PdfTracePoint,
    },
    Cubic {
        control_1: PdfTracePoint,
        control_2: PdfTracePoint,
        end: PdfTracePoint,
    },
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfTraceFillRule {
    Nonzero,
    EvenOdd,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfTraceClip {
    pub polygons: Vec<Vec<PdfTracePoint>>,
    pub fill_rule: PdfTraceFillRule,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfTraceLineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfTraceLineJoin {
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PdfTraceResourceKind {
    Font,
    GraphicsState,
    XObject,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PdfTraceResource {
    pub name: String,
    pub kind: PdfTraceResourceKind,
    pub target: String,
    pub content_sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PdfTraceDisplayOperation {
    Text {
        text: String,
        bbox: Rect,
        font_size_pt: f32,
        font_name: String,
        bold: bool,
        color_argb: u32,
        stroke_color_argb: u32,
        stroke_width_pt: f32,
        render_mode: u8,
        mirrored_x: bool,
        clips: Vec<PdfTraceClip>,
    },
    Fill {
        path: Vec<PdfTracePathSegment>,
        color_argb: u32,
        fill_rule: PdfTraceFillRule,
        clips: Vec<PdfTraceClip>,
    },
    Stroke {
        path: Vec<PdfTracePathSegment>,
        color_argb: u32,
        width_pt: f32,
        line_cap: PdfTraceLineCap,
        line_join: PdfTraceLineJoin,
        miter_limit: f32,
        dash_pattern_pt: Vec<f32>,
        dash_phase_pt: f32,
        clips: Vec<PdfTraceClip>,
    },
    Figure {
        bbox: Rect,
        resource_name: String,
        clips: Vec<PdfTraceClip>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PdfTracePage {
    pub number: u32,
    pub width_pt: f32,
    pub height_pt: f32,
    pub spans: Vec<PdfTextSpan>,
    pub resources: Vec<PdfTraceResource>,
    pub operations: Vec<PdfTraceDisplayOperation>,
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
            object_streams: RefCell::new(BTreeMap::new()),
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
        let trace = self.trace_page(number)?;
        Ok(PdfPage {
            number: trace.number,
            width_pt: trace.width_pt,
            height_pt: trace.height_pt,
            spans: trace.spans,
            warnings: trace.warnings,
        })
    }

    pub fn trace_page(&self, number: u32) -> Result<PdfTracePage, DocsightError> {
        let parsed = self.parse_page(number)?;
        let spans = parsed
            .text_runs
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, run)| self.make_span(number, index, run, parsed.approximated_font))
            .collect::<Result<Vec<_>, DocsightError>>()?;
        let mut warnings = Vec::new();
        if parsed.approximated_font {
            warnings.push(font_approximation_warning(number));
        }
        if parsed.omitted_xobjects {
            warnings.push(xobject_placeholder_warning(number));
        }
        if parsed.unmapped_text_codes {
            warnings.push(unmapped_text_warning(number));
        }
        let warning_objects = spans
            .iter()
            .map(|span| (span.id.clone(), span.bbox))
            .collect::<Vec<_>>();
        warnings.extend(graphics_state_visual_warnings(
            number,
            &parsed,
            &warning_objects,
        ));
        let page = self.page_record(number)?;
        Ok(PdfTracePage {
            number,
            width_pt: page.media_box.width(),
            height_pt: page.media_box.height(),
            spans,
            resources: parsed.trace_resources,
            operations: parsed
                .commands
                .iter()
                .map(trace_display_operation)
                .collect(),
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

    fn page_annotations(
        &self,
        number: u32,
        page: &PageRecord,
    ) -> Result<(Vec<Hyperlink>, Vec<Overlay>), DocsightError> {
        let mut links = Vec::new();
        let mut overlays = Vec::new();
        for (index, value) in page.annotations.iter().enumerate() {
            let dict = match self.store.resolve(value)? {
                Value::Dict(dict) => dict,
                Value::Null => continue,
                _ => return Err(malformed("page annotation must be a dictionary")),
            };
            let subtype = match dict.get("Subtype") {
                Some(Value::Name(name)) => name.clone(),
                _ => return Err(malformed("page annotation has no Subtype name")),
            };
            let bbox = match dict.get("Rect") {
                Some(value) => Some(self.annotation_rect(value, page)?),
                None => None,
            };
            let source_path = format!("pdf::page[{number}]::annot[{index}]");
            if subtype == "Link" {
                let Some(target) = self.link_target(&dict)? else {
                    continue;
                };
                links.push(Hyperlink {
                    id: self.source.object_id("lnk", &source_path),
                    text: String::new(),
                    target: target.target,
                    is_external: target.is_external,
                    page: Some(number),
                    anchor_path: Some(source_path.clone()),
                    source: SourceSpan::new(source_path),
                });
                continue;
            }
            let text = match dict.get("Contents") {
                Some(value) => match self.store.resolve(value)? {
                    Value::String(bytes) => decode_pdf_text_string(&bytes).unwrap_or_default(),
                    _ => String::new(),
                },
                None => String::new(),
            };
            overlays.push(Overlay {
                id: self.source.object_id("anno", &source_path),
                kind: OverlayKind::Annotation,
                page: number,
                bbox,
                text,
                source: SourceSpan::new(source_path),
            });
        }
        Ok((links, overlays))
    }

    fn annotation_rect(&self, value: &Value, page: &PageRecord) -> Result<Rect, DocsightError> {
        let native = parse_box(&self.store, value)?;
        let left = page.media_box.x0;
        let top = page.media_box.y1;
        Rect::new(
            native.x0 - left,
            top - native.y1,
            native.x1 - left,
            top - native.y0,
        )
    }

    fn link_target(
        &self,
        annotation: &BTreeMap<String, Value>,
    ) -> Result<Option<LinkTarget>, DocsightError> {
        if let Some(value) = annotation.get("A") {
            let action = match self.store.resolve(value)? {
                Value::Dict(dict) => dict,
                Value::Null => return Ok(None),
                _ => return Err(malformed("link annotation action must be a dictionary")),
            };
            let kind = match action.get("S") {
                Some(Value::Name(name)) => name.clone(),
                _ => return Err(malformed("link action has no S name")),
            };
            if kind != "URI" {
                return Ok(None);
            }
            let Some(uri) = action.get("URI") else {
                return Ok(None);
            };
            let Value::String(bytes) = self.store.resolve(uri)? else {
                return Err(malformed("link action URI must be a string"));
            };
            return Ok(decode_pdf_text_string(&bytes).map(|target| LinkTarget {
                target,
                is_external: true,
            }));
        }
        let Some(destination) = annotation.get("Dest") else {
            return Ok(None);
        };
        Ok(
            destination_name(&self.store.resolve(destination)?).map(|target| LinkTarget {
                target,
                is_external: false,
            }),
        )
    }

    fn xobject_entries(
        &self,
        resources: &BTreeMap<String, Value>,
        depth: usize,
        visited: &mut BTreeSet<ObjectRef>,
    ) -> Result<BTreeMap<String, XObjectEntry>, DocsightError> {
        let Some(value) = resources.get("XObject") else {
            return Ok(BTreeMap::new());
        };
        if depth > MAX_FORM_XOBJECT_DEPTH {
            return Err(DocsightError::ResourceLimit {
                resource: "PDF form XObject nesting".to_owned(),
                limit: MAX_FORM_XOBJECT_DEPTH as u64,
            });
        }
        let dictionary = self.store.resolve_dict(value)?;
        let mut entries = BTreeMap::new();
        for (name, entry) in dictionary {
            let reference = match entry {
                Value::Ref(reference) => Some(reference),
                _ => None,
            };
            let resolved = self.store.resolve(&entry)?;
            let Value::Stream(stream) = resolved else {
                entries.insert(name, XObjectEntry::Image);
                continue;
            };
            let subtype = match stream.dict.get("Subtype") {
                Some(Value::Name(subtype)) => subtype.as_str(),
                _ => "",
            };
            if subtype != "Form" {
                entries.insert(name, XObjectEntry::Image);
                continue;
            }
            if let Some(reference) = reference
                && !visited.insert(reference)
            {
                return Err(malformed("cycle detected between PDF form XObjects"));
            }
            let content = decode_stream(&stream)?;
            let matrix = match stream.dict.get("Matrix") {
                Some(value) => match self.store.resolve(value)? {
                    Value::Array(values) if values.len() == 6 => {
                        let mut matrix = [0.0_f32; 6];
                        for (slot, value) in matrix.iter_mut().zip(values.iter()) {
                            *slot = pdf_number(value)?;
                        }
                        matrix
                    }
                    _ => return Err(malformed("form XObject Matrix must hold six numbers")),
                },
                None => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            };
            if !matrix.iter().all(|value| value.is_finite()) {
                return Err(malformed("form XObject Matrix is not finite"));
            }
            let nested_resources = match stream.dict.get("Resources") {
                Some(value) => self.store.resolve_dict(value)?,
                None => resources.clone(),
            };
            let fonts = fonts_from_resources(
                &nested_resources,
                |value| self.store.resolve(value),
                |value| match self.store.resolve(value)? {
                    Value::Stream(stream) => decode_stream(&stream),
                    _ => Err(malformed("ToUnicode must resolve to a stream")),
                },
            )?;
            let graphics_states = ext_graphics_states_from_resources(
                &nested_resources,
                |value| self.store.resolve(value),
                |value| match self.store.resolve(value)? {
                    Value::Stream(stream) => decode_stream(&stream),
                    _ => Err(malformed("font program must resolve to a stream")),
                },
            )?;
            let xobjects = self.xobject_entries(&nested_resources, depth + 1, visited)?;
            if let Some(reference) = reference {
                visited.remove(&reference);
            }
            entries.insert(
                name,
                XObjectEntry::Form(Box::new(FormXObject {
                    content,
                    matrix,
                    resources: ContentResources {
                        fonts,
                        xobjects,
                        ext_graphics_states: graphics_states.states,
                    },
                })),
            );
        }
        Ok(entries)
    }

    fn document_info(&self) -> Result<DocumentMetadata, DocsightError> {
        let Some(info) = self.store.xref.trailer.get("Info") else {
            return Ok(DocumentMetadata::default());
        };
        let dict = match self.store.resolve(info)? {
            Value::Dict(dict) => dict,
            Value::Null => return Ok(DocumentMetadata::default()),
            _ => return Err(malformed("trailer Info is not a dictionary")),
        };
        let entry = |key: &str| -> Result<Option<String>, DocsightError> {
            let Some(value) = dict.get(key) else {
                return Ok(None);
            };
            match self.store.resolve(value)? {
                Value::String(bytes) => Ok(decode_pdf_text_string(&bytes)),
                Value::Null => Ok(None),
                _ => Ok(None),
            }
        };
        Ok(DocumentMetadata {
            title: entry("Title")?,
            author: entry("Author")?,
            subject: entry("Subject")?,
            producer: entry("Producer")?,
        })
    }

    pub fn to_document(&self) -> Result<Document, DocsightError> {
        let mut blocks = Vec::new();
        let mut pages = Vec::new();
        let mut links = Vec::new();
        let mut all_warnings = Vec::new();
        let mut global_reading_order = 0_u32;

        for page_num in 1..=self.page_count() {
            let page_record = self.page_record(page_num)?;
            let parsed = self.parse_page(page_num)?;
            if parsed.approximated_font {
                all_warnings.push(font_approximation_warning(page_num));
            }
            if parsed.omitted_xobjects {
                all_warnings.push(xobject_placeholder_warning(page_num));
            }
            if parsed.unmapped_text_codes {
                all_warnings.push(unmapped_text_warning(page_num));
            }
            if parsed.text_runs.is_empty() && parsed.omitted_xobjects {
                all_warnings.push(no_text_layer_warning(page_num));
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
            let warning_objects = reconstructed
                .blocks
                .iter()
                .filter_map(|block| block.bbox.map(|bbox| (block.id.clone(), bbox)))
                .collect::<Vec<_>>();
            all_warnings.extend(graphics_state_visual_warnings(
                page_num,
                &parsed,
                &warning_objects,
            ));
            blocks.extend(reconstructed.blocks);

            let (page_links, overlays) = self.page_annotations(page_num, page_record)?;
            links.extend(page_links);
            pages.push(Page {
                number: page_num,
                width_pt: page_record.media_box.width(),
                height_pt: page_record.media_box.height(),
                block_ids: reconstructed.block_ids,
                overlays,
            });
        }

        let document = Document {
            version: IrVersion::current(),
            id: self.source.id(),
            sha256: self.source.sha256().to_owned(),
            format: DocumentFormat::Pdf,
            size_bytes: self.source.size_bytes(),
            metadata: self.document_info()?,
            styles: Vec::new(),
            sections: Vec::new(),
            pages,
            blocks,
            resources: Vec::new(),
            links,
            comments: Vec::new(),
            tracked_changes: docsight_core::TrackedChanges::default(),
            warnings: all_warnings,
        };
        validate_canonical(&document)?;
        Ok(document)
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
        let mut warnings = Vec::new();
        if parsed.approximated_font {
            warnings.push(font_approximation_warning(number));
        }
        if parsed.omitted_xobjects {
            warnings.push(xobject_placeholder_warning(number));
        }
        if parsed.unmapped_text_codes {
            warnings.push(unmapped_text_warning(number));
        }
        warnings.extend(graphics_state_visual_warnings(number, &parsed, &[]));
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
        let graphics_states = ext_graphics_states_from_resources(
            &resources,
            |value| self.store.resolve(value),
            |value| {
                let value = self.store.resolve(value)?;
                match value {
                    Value::Stream(stream) => decode_stream(&stream),
                    _ => Err(malformed("font program must resolve to a stream")),
                }
            },
        )?;
        let mut visited = BTreeSet::new();
        let xobject_entries = self.xobject_entries(&resources, 0, &mut visited)?;
        let xobjects: BTreeSet<String> = xobject_entries.keys().cloned().collect();
        let content_resources = ContentResources {
            fonts: fonts.clone(),
            xobjects: xobject_entries,
            ext_graphics_states: graphics_states.states.clone(),
        };
        let trace_resources = trace_resources(&fonts, &xobjects, &graphics_states.states);
        let content = self.read_content_streams(&page.contents)?;
        let parsed = parse_content(
            &content.bytes,
            page.media_box.x0,
            page.media_box.y1,
            &content_resources,
        )
        .map_err(|error| {
            let mut location = ErrorLocation {
                page: Some(number),
                ..ErrorLocation::default()
            };
            if let Some(error_location) = error.error_location()
                && let Some(offset) = error_location.offset
            {
                let stream_location = content.location(offset);
                location.object = stream_location.object;
                location.offset = stream_location.offset;
            }
            error.with_error_location(location)
        })?;
        Ok(ParsedPage {
            commands: parsed.commands,
            text_runs: parsed.text_runs,
            approximated_font: parsed.approximated_font,
            omitted_xobjects: parsed.omitted_xobjects,
            page_visual_issues: parsed.page_visual_issues,
            unmapped_text_codes: parsed.unmapped_text_codes,
            trace_resources,
        })
    }

    fn read_content_streams(&self, contents: &[Value]) -> Result<ContentStreams, DocsightError> {
        let mut combined = Vec::new();
        let mut segments = Vec::new();
        for (index, value) in contents.iter().enumerate() {
            let object = match value {
                Value::Ref(reference) => {
                    format!("{} {} R", reference.number, reference.generation)
                }
                _ => format!("contents[{index}]"),
            };
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
            let start = combined.len() as u64;
            combined.extend_from_slice(&data);
            combined.push(b'\n');
            segments.push(ContentSegment {
                start,
                end: combined.len() as u64,
                object,
            });
        }
        Ok(ContentStreams {
            bytes: combined,
            segments,
        })
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
    omitted_xobjects: bool,
    unmapped_text_codes: bool,
    page_visual_issues: Vec<VisualIssue>,
    trace_resources: Vec<PdfTraceResource>,
}

struct ContentStreams {
    bytes: Vec<u8>,
    segments: Vec<ContentSegment>,
}

impl ContentStreams {
    fn location(&self, offset: u64) -> ErrorLocation {
        let segment = self
            .segments
            .iter()
            .find(|segment| offset >= segment.start && offset < segment.end)
            .or_else(|| self.segments.last());
        match segment {
            Some(segment) => ErrorLocation {
                object: Some(segment.object.clone()),
                offset: Some(offset.saturating_sub(segment.start)),
                ..ErrorLocation::default()
            },
            None => ErrorLocation {
                offset: Some(offset),
                ..ErrorLocation::default()
            },
        }
    }
}

struct ContentSegment {
    start: u64,
    end: u64,
    object: String,
}

fn trace_resources(
    fonts: &BTreeMap<String, FontInfo>,
    xobjects: &BTreeSet<String>,
    graphics_states: &BTreeMap<String, ExtGraphicsState>,
) -> Vec<PdfTraceResource> {
    let mut resources = Vec::new();
    resources.extend(fonts.iter().map(|(name, font)| {
        PdfTraceResource {
            name: name.clone(),
            kind: PdfTraceResourceKind::Font,
            target: font.base_font.clone(),
            content_sha256: font
                .outline
                .as_ref()
                .map(|program| program.sha256().to_owned()),
        }
    }));
    resources.extend(graphics_states.keys().map(|name| PdfTraceResource {
        name: name.clone(),
        kind: PdfTraceResourceKind::GraphicsState,
        target: "ExtGState".to_owned(),
        content_sha256: None,
    }));
    resources.extend(xobjects.iter().map(|name| PdfTraceResource {
        name: name.clone(),
        kind: PdfTraceResourceKind::XObject,
        target: "XObject".to_owned(),
        content_sha256: None,
    }));
    resources
}

fn trace_display_operation(command: &DisplayCommand) -> PdfTraceDisplayOperation {
    match command {
        DisplayCommand::Text(run) => PdfTraceDisplayOperation::Text {
            text: run.text.clone(),
            bbox: run.bbox,
            font_size_pt: run.font_size,
            font_name: run.font_name.clone(),
            bold: run.bold,
            color_argb: run.argb,
            stroke_color_argb: run.stroke_argb,
            stroke_width_pt: run.stroke_style.width,
            render_mode: run.render_mode,
            mirrored_x: run.mirrored_x,
            clips: trace_clips(&run.clips),
        },
        DisplayCommand::Fill {
            path,
            paint,
            even_odd,
            clips,
            ..
        } => PdfTraceDisplayOperation::Fill {
            path: path.iter().map(trace_path_segment).collect(),
            color_argb: paint_argb(*paint),
            fill_rule: trace_fill_rule(*even_odd),
            clips: trace_clips(clips),
        },
        DisplayCommand::Stroke {
            path,
            paint,
            style,
            clips,
            ..
        } => PdfTraceDisplayOperation::Stroke {
            path: path.iter().map(trace_path_segment).collect(),
            color_argb: paint_argb(*paint),
            width_pt: style.width,
            line_cap: match style.cap {
                LineCap::Butt => PdfTraceLineCap::Butt,
                LineCap::Round => PdfTraceLineCap::Round,
                LineCap::Square => PdfTraceLineCap::Square,
            },
            line_join: match style.join {
                LineJoin::Miter => PdfTraceLineJoin::Miter,
                LineJoin::Round => PdfTraceLineJoin::Round,
                LineJoin::Bevel => PdfTraceLineJoin::Bevel,
            },
            miter_limit: style.miter_limit,
            dash_pattern_pt: style.dash.clone(),
            dash_phase_pt: style.dash_phase,
            clips: trace_clips(clips),
        },
        DisplayCommand::Figure {
            bbox,
            resource_name,
            clips,
            ..
        } => PdfTraceDisplayOperation::Figure {
            bbox: *bbox,
            resource_name: resource_name.clone(),
            clips: trace_clips(clips),
        },
    }
}

fn trace_path_segment(segment: &PathSegment) -> PdfTracePathSegment {
    match segment {
        PathSegment::Move(point) => PdfTracePathSegment::Move {
            point: trace_point(*point),
        },
        PathSegment::Line(point) => PdfTracePathSegment::Line {
            point: trace_point(*point),
        },
        PathSegment::Cubic(control_1, control_2, end) => PdfTracePathSegment::Cubic {
            control_1: trace_point(*control_1),
            control_2: trace_point(*control_2),
            end: trace_point(*end),
        },
        PathSegment::Close => PdfTracePathSegment::Close,
    }
}

fn trace_clips(clips: &[ClipRegion]) -> Vec<PdfTraceClip> {
    clips
        .iter()
        .map(|clip| PdfTraceClip {
            polygons: clip
                .polygons
                .iter()
                .map(|polygon| polygon.iter().copied().map(trace_point).collect())
                .collect(),
            fill_rule: trace_fill_rule(clip.even_odd),
        })
        .collect()
}

fn trace_point(point: Point) -> PdfTracePoint {
    PdfTracePoint {
        x: point.x,
        y: point.y,
    }
}

fn trace_fill_rule(even_odd: bool) -> PdfTraceFillRule {
    if even_odd {
        PdfTraceFillRule::EvenOdd
    } else {
        PdfTraceFillRule::Nonzero
    }
}

fn paint_argb(paint: Paint) -> u32 {
    let alpha = (paint.alpha * 255.0).round() as u32;
    alpha << 24
        | u32::from(paint.color.red) << 16
        | u32::from(paint.color.green) << 8
        | u32::from(paint.color.blue)
}

struct ObjectStreamIndex {
    data: Vec<u8>,
    entries: Vec<(u32, usize)>,
}

struct ObjectStore<'a> {
    bytes: &'a [u8],
    xref: Xref,
    object_streams: RefCell<BTreeMap<u32, Rc<ObjectStreamIndex>>>,
}

impl ObjectStore<'_> {
    fn object(&self, reference: ObjectRef) -> Result<Value, DocsightError> {
        let entry = self.xref.entries.get(&reference).copied().ok_or_else(|| {
            malformed(format!(
                "missing xref entry for object {}",
                reference.number
            ))
        })?;
        match entry {
            XrefEntry::Offset(offset) => {
                parse_object(self.bytes, offset, reference, &self.xref.entries)
            }
            XrefEntry::Compressed { stream, index } => {
                self.compressed_object(reference, stream, index)
            }
        }
    }

    fn compressed_object(
        &self,
        reference: ObjectRef,
        container: u32,
        index: u32,
    ) -> Result<Value, DocsightError> {
        let stream = self.object_stream(container)?;
        let index =
            usize::try_from(index).map_err(|_| malformed("object stream index is out of range"))?;
        let (number, offset) =
            stream.entries.get(index).copied().ok_or_else(|| {
                malformed(format!("object stream {container} has no entry {index}"))
            })?;
        if number != reference.number {
            return Err(malformed(format!(
                "object stream {container} entry {index} holds object {number}, not {}",
                reference.number
            )));
        }
        syntax::parse_object_stream_value(&stream.data, offset)
    }

    fn object_stream(&self, container: u32) -> Result<Rc<ObjectStreamIndex>, DocsightError> {
        if let Some(cached) = self.object_streams.borrow().get(&container) {
            return Ok(Rc::clone(cached));
        }
        let reference = ObjectRef {
            number: container,
            generation: 0,
        };
        let offset = match self.xref.entries.get(&reference).copied() {
            Some(XrefEntry::Offset(offset)) => offset,
            Some(XrefEntry::Compressed { .. }) => {
                return Err(malformed(
                    "an object stream must not live inside another object stream",
                ));
            }
            None => {
                return Err(malformed(format!(
                    "missing xref entry for object stream {container}"
                )));
            }
        };
        let value = parse_object(self.bytes, offset, reference, &self.xref.entries)?;
        let Value::Stream(stream) = value else {
            return Err(malformed(format!("object {container} is not a stream")));
        };
        if !matches!(stream.dict.get("Type"), Some(Value::Name(name)) if name == "ObjStm") {
            return Err(malformed(format!(
                "object {container} is not an object stream"
            )));
        }
        let count = match stream.dict.get("N") {
            Some(Value::Int(count)) if *count >= 0 => {
                usize::try_from(*count).map_err(|_| malformed("object stream N is out of range"))?
            }
            _ => return Err(malformed("object stream has no valid N")),
        };
        let first = match stream.dict.get("First") {
            Some(Value::Int(first)) if *first >= 0 => usize::try_from(*first)
                .map_err(|_| malformed("object stream First is out of range"))?,
            _ => return Err(malformed("object stream has no valid First")),
        };
        let data = decode_stream(&stream)?;
        if first > data.len() {
            return Err(malformed("object stream First is past the decoded stream"));
        }
        let entries = syntax::parse_object_stream_index(&data, count, first)?;
        let indexed = Rc::new(ObjectStreamIndex { data, entries });
        self.object_streams
            .borrow_mut()
            .insert(container, Rc::clone(&indexed));
        Ok(indexed)
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
    annotations: Vec<Value>,
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
                Some(value) => match store.resolve(value)? {
                    Value::Array(values) => values,
                    Value::Stream(_) => vec![value.clone()],
                    _ => return Err(malformed("Contents must be a stream or array of streams")),
                },
            };
            let annotations = match dict.get("Annots") {
                None => Vec::new(),
                Some(value) => match store.resolve(value)? {
                    Value::Array(values) => values,
                    Value::Null => Vec::new(),
                    _ => return Err(malformed("Annots must be an array")),
                },
            };
            if annotations.len() > MAX_ANNOTATIONS as usize {
                return Err(DocsightError::ResourceLimit {
                    resource: "PDF page annotations".to_owned(),
                    limit: u64::from(MAX_ANNOTATIONS),
                });
            }
            pages.push(PageRecord {
                media_box: effective_box,
                resources: current.resources,
                contents,
                annotations,
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
    if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
        return Err(malformed("page box has non-finite geometry"));
    }
    let normalized = NativeBox {
        x0: x0.min(x1),
        y0: y0.min(y1),
        x1: x0.max(x1),
        y1: y0.max(y1),
    };
    if normalized.width() <= 0.0 || normalized.height() <= 0.0 {
        return Err(malformed("page box has zero width or height"));
    }
    Ok(normalized)
}

fn pdf_number(value: &Value) -> Result<f32, DocsightError> {
    match value {
        Value::Int(value) => Ok(*value as f32),
        Value::Real(value) => Ok(*value),
        _ => Err(malformed("page box coordinate must be numeric")),
    }
}

fn no_text_layer_warning(page: u32) -> Diagnostic {
    Diagnostic {
        code: "PDF_PAGE_HAS_NO_TEXT_LAYER".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!("page {page} paints only images and carries no text operators"),
        effect: "the page is a raster scan; recovering its text would require OCR, which this engine does not perform"
            .to_owned(),
        object: None,
        page: Some(page),
    }
}

fn unmapped_text_warning(page: u32) -> Diagnostic {
    Diagnostic {
        code: "PDF_TEXT_CODE_UNMAPPED".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!("page {page} shows text codes the font ToUnicode CMap does not map"),
        effect: "those codes are extracted as the Unicode replacement character".to_owned(),
        object: None,
        page: Some(page),
    }
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

fn xobject_placeholder_warning(page: u32) -> Diagnostic {
    Diagnostic {
        code: "PDF_XOBJECT_PLACEHOLDER".to_owned(),
        severity: DiagnosticSeverity::Warning,
        message: format!("page {page} places an image XObject as a figure placeholder"),
        effect: "the image pixels are not decoded; form XObjects are traversed and their text is extracted"
            .to_owned(),
        object: None,
        page: Some(page),
    }
}

fn graphics_state_visual_warnings(
    page: u32,
    parsed: &ParsedPage,
    objects: &[(ObjectId, Rect)],
) -> Vec<Diagnostic> {
    let mut warning_targets = BTreeSet::new();
    for issue in &parsed.page_visual_issues {
        warning_targets.insert((issue.clone(), None));
    }
    for command in &parsed.commands {
        let (issues, bbox) = command_visual_evidence(command);
        for issue in issues {
            let affected = bbox
                .map(|bbox| {
                    objects
                        .iter()
                        .filter(|(_, object_bbox)| bbox.intersects(*object_bbox))
                        .map(|(id, _)| id.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if affected.is_empty() {
                warning_targets.insert((issue.clone(), None));
            } else {
                warning_targets.extend(
                    affected
                        .into_iter()
                        .map(|object| (issue.clone(), Some(object))),
                );
            }
        }
    }
    warning_targets
        .into_iter()
        .map(|(issue, object)| visual_warning(page, issue, object))
        .collect()
}

fn command_visual_evidence(command: &DisplayCommand) -> (&[VisualIssue], Option<Rect>) {
    match command {
        DisplayCommand::Text(run) => (&run.visual_issues, Some(run.bbox)),
        DisplayCommand::Figure {
            bbox,
            visual_issues,
            ..
        } => (visual_issues, Some(*bbox)),
        DisplayCommand::Fill {
            path,
            visual_issues,
            ..
        }
        | DisplayCommand::Stroke {
            path,
            visual_issues,
            ..
        } => (visual_issues, path_bbox(path)),
    }
}

fn path_bbox(path: &[PathSegment]) -> Option<Rect> {
    let points = path.iter().flat_map(|segment| match segment {
        PathSegment::Move(point) | PathSegment::Line(point) => vec![*point],
        PathSegment::Cubic(control_1, control_2, end) => vec![*control_1, *control_2, *end],
        PathSegment::Close => Vec::new(),
    });
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for point in points {
        min_x = min_x.min(point.x);
        min_y = min_y.min(point.y);
        max_x = max_x.max(point.x);
        max_y = max_y.max(point.y);
    }
    if !min_x.is_finite() {
        return None;
    }
    if min_x == max_x {
        max_x += 0.001;
    }
    if min_y == max_y {
        max_y += 0.001;
    }
    Rect::new(min_x, min_y, max_x, max_y).ok()
}

fn visual_warning(page: u32, issue: VisualIssue, object: Option<ObjectId>) -> Diagnostic {
    let (code, message, effect) = match issue {
        VisualIssue::ExtGraphicsState(key) => (
            "PDF_EXTGSTATE_IGNORED",
            format!("page {page} paints content with unsupported ExtGState entry {key}"),
            "the affected object's pixels omit this graphics-state behavior",
        ),
        VisualIssue::SoftMask => (
            "PDF_SOFT_MASK_IGNORED",
            format!("page {page} paints content under a PDF soft mask"),
            "the affected object's pixels are painted without their transparency mask",
        ),
        VisualIssue::BlendMode(name) => (
            "PDF_BLEND_MODE_UNSUPPORTED",
            format!("page {page} paints content with unsupported blend mode {name}"),
            "the affected object's pixels are composited without the declared blend mode",
        ),
        VisualIssue::ColorSpace(name) => (
            "PDF_COLOR_SPACE_UNSUPPORTED",
            format!("page {page} paints content in unsupported color space {name}"),
            "the affected object's pixels use black instead of a calibrated color conversion",
        ),
        VisualIssue::Pattern(name) => (
            "PDF_PATTERN_PAINT_UNSUPPORTED",
            format!("page {page} paints content with unsupported pattern {name}"),
            "the affected object's pixels use the previous flat color instead of the pattern",
        ),
        VisualIssue::Shading(name) => (
            "PDF_SHADING_UNSUPPORTED",
            format!("page {page} paints an unsupported shading resource {name}"),
            "the shaded area is left unpainted; text and structure on the page are unaffected",
        ),
        VisualIssue::TextClip => (
            "PDF_CLIP_TEXT_VISUAL",
            format!("page {page} uses a clipping text rendering mode"),
            "the affected text was extracted, but its outline was not added to the clipping path",
        ),
        VisualIssue::NonUniformStroke => (
            "PDF_NON_UNIFORM_STROKE_VISUAL",
            format!("page {page} strokes a path under a non-uniform transform"),
            "stroke width is approximated by the area-preserving mean of both axis scales",
        ),
        VisualIssue::NegativeFontSize => (
            "PDF_NEGATIVE_FONT_SIZE_VISUAL",
            format!("page {page} uses a negative PDF font size"),
            "the affected text was extracted, but the complete signed font transform is not reproduced by the axis-aligned renderer",
        ),
    };
    Diagnostic {
        code: code.to_owned(),
        severity: DiagnosticSeverity::Warning,
        message,
        effect: effect.to_owned(),
        object,
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

const PDF_DOC_ENCODING_HIGH: [char; 33] = [
    '\u{2022}',
    '\u{2020}',
    '\u{2021}',
    '\u{2026}',
    '\u{2014}',
    '\u{2013}',
    '\u{0192}',
    '\u{2044}',
    '\u{2039}',
    '\u{203A}',
    '\u{2212}',
    '\u{2030}',
    '\u{201E}',
    '\u{201C}',
    '\u{201D}',
    '\u{2018}',
    '\u{2019}',
    '\u{201A}',
    '\u{2122}',
    '\u{FB01}',
    '\u{FB02}',
    '\u{0141}',
    '\u{0152}',
    '\u{0160}',
    '\u{0178}',
    '\u{017D}',
    '\u{0131}',
    '\u{0142}',
    '\u{0153}',
    '\u{0161}',
    '\u{017E}',
    char::REPLACEMENT_CHARACTER,
    '\u{20AC}',
];

const PDF_DOC_ENCODING_ACCENTS: [char; 8] = [
    '\u{02D8}', '\u{02C7}', '\u{02C6}', '\u{02D9}', '\u{02DD}', '\u{02DB}', '\u{02DA}', '\u{02DC}',
];

fn decode_pdf_text_string(bytes: &[u8]) -> Option<String> {
    let text = if bytes.starts_with(&[0xFE, 0xFF]) {
        decode_utf16_be(&bytes[2..])
    } else {
        decode_pdf_doc_encoding(bytes)
    };
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn decode_utf16_be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect();
    char::decode_utf16(units)
        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

fn decode_pdf_doc_encoding(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| match byte {
            0x18..=0x1F => PDF_DOC_ENCODING_ACCENTS[usize::from(byte - 0x18)],
            0x20..=0x7E => char::from(*byte),
            0x80..=0xA0 => PDF_DOC_ENCODING_HIGH[usize::from(byte - 0x80)],
            0xA1..=0xFF => char::from(*byte),
            _ => char::REPLACEMENT_CHARACTER,
        })
        .collect()
}

struct LinkTarget {
    target: String,
    is_external: bool,
}

fn destination_name(value: &Value) -> Option<String> {
    match value {
        Value::Name(name) => Some(name.clone()),
        Value::String(bytes) => decode_pdf_text_string(bytes),
        Value::Array(values) => match values.first() {
            Some(Value::Ref(reference)) => Some(format!("page object {}", reference.number)),
            Some(Value::Int(index)) => Some(format!("page index {index}")),
            _ => None,
        },
        _ => None,
    }
}
