use docsight_core::{Diagnostic, ObjectId};
use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParagraphKind {
    Heading,
    ListItem,
    Paragraph,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ListMarker {
    pub num_id: String,
    pub level: u8,
    pub format: Option<String>,
    pub pattern: Option<String>,
    pub ordered: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Paragraph {
    pub id: ObjectId,
    pub text: String,
    pub kind: ParagraphKind,
    pub style_id: Option<String>,
    pub heading_level: Option<u8>,
    pub list: Option<ListMarker>,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Heading {
    pub id: ObjectId,
    pub level: u8,
    pub text: String,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TableCell {
    pub id: ObjectId,
    pub row: u32,
    pub column: u32,
    pub row_span: u32,
    pub column_span: u32,
    pub text: String,
    pub nested_tables: Vec<Table>,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Table {
    pub id: ObjectId,
    pub rows: u32,
    pub columns: u32,
    pub cells: Vec<TableCell>,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum DocxBlock {
    Paragraph(Paragraph),
    Table(Table),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DocxDocument {
    pub blocks: Vec<DocxBlock>,
    pub warnings: Vec<Diagnostic>,
}

impl DocxDocument {
    pub fn paragraphs(&self) -> impl Iterator<Item = &Paragraph> {
        self.blocks.iter().filter_map(|block| match block {
            DocxBlock::Paragraph(paragraph) => Some(paragraph),
            DocxBlock::Table(_) => None,
        })
    }

    pub fn headings(&self) -> impl Iterator<Item = Heading> + '_ {
        self.paragraphs().filter_map(|paragraph| {
            paragraph.heading_level.map(|level| Heading {
                id: paragraph.id.clone(),
                level,
                text: paragraph.text.clone(),
                source: paragraph.source.clone(),
            })
        })
    }

    pub fn tables(&self) -> impl Iterator<Item = &Table> {
        self.blocks.iter().filter_map(|block| match block {
            DocxBlock::Paragraph(_) => None,
            DocxBlock::Table(table) => Some(table),
        })
    }
}
