mod model;
mod package;
mod parser;

pub use model::{
    DocxBlock, DocxDocument, Heading, ListMarker, Paragraph, ParagraphKind, Table, TableCell,
};
pub use parser::parse_docx;
