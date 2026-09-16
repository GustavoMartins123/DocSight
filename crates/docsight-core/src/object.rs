use crate::{
    Block, BlockContent, BlockKind, Document, Hyperlink, ObjectId, Overlay, OverlayKind, Rect,
    SourceSpan, TableCell,
};
use serde::{Deserialize, Serialize};

const MAX_OBJECT_NESTING: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentObjectKind {
    Block,
    TableCell,
    Overlay,
    Hyperlink,
}

#[derive(Clone, Copy, Debug)]
pub enum DocumentObject<'a> {
    Block {
        block: &'a Block,
        container: Option<&'a Block>,
    },
    TableCell {
        table: &'a Block,
        cell: &'a TableCell,
    },
    Overlay(&'a Overlay),
    Hyperlink(&'a Hyperlink),
}

impl<'a> DocumentObject<'a> {
    pub fn kind(&self) -> DocumentObjectKind {
        match self {
            Self::Block { .. } => DocumentObjectKind::Block,
            Self::TableCell { .. } => DocumentObjectKind::TableCell,
            Self::Overlay(_) => DocumentObjectKind::Overlay,
            Self::Hyperlink(_) => DocumentObjectKind::Hyperlink,
        }
    }

    pub fn id(&self) -> &'a ObjectId {
        match self {
            Self::Block { block, .. } => &block.id,
            Self::TableCell { cell, .. } => &cell.id,
            Self::Overlay(overlay) => &overlay.id,
            Self::Hyperlink(link) => &link.id,
        }
    }

    pub fn block_kind(&self) -> Option<BlockKind> {
        match self {
            Self::Block { block, .. } => Some(block.kind),
            _ => None,
        }
    }

    pub fn overlay_kind(&self) -> Option<OverlayKind> {
        match self {
            Self::Overlay(overlay) => Some(overlay.kind),
            _ => None,
        }
    }

    pub fn container(&self) -> Option<&'a Block> {
        match self {
            Self::Block { container, .. } => *container,
            Self::TableCell { table, .. } => Some(table),
            Self::Overlay(_) | Self::Hyperlink(_) => None,
        }
    }

    pub fn page(&self) -> Option<u32> {
        match self {
            Self::Block { block, container } => block
                .page
                .or_else(|| container.and_then(|container| container.page)),
            Self::TableCell { table, .. } => table.page,
            Self::Overlay(overlay) => Some(overlay.page),
            Self::Hyperlink(link) => link.page,
        }
    }

    pub fn bbox(&self) -> Option<Rect> {
        match self {
            Self::Block { block, .. } => block.bbox,
            Self::TableCell { cell, .. } => cell.bbox,
            Self::Overlay(overlay) => overlay.bbox,
            Self::Hyperlink(link) => link.bbox,
        }
    }

    pub fn text(&self) -> String {
        match self {
            Self::Block { block, .. } => block.text(),
            Self::TableCell { cell, .. } => cell.text.clone(),
            Self::Overlay(overlay) => overlay.text.clone(),
            Self::Hyperlink(link) => link.text.clone(),
        }
    }

    pub fn source(&self) -> &'a SourceSpan {
        match self {
            Self::Block { block, .. } => &block.source,
            Self::TableCell { cell, .. } => &cell.source,
            Self::Overlay(overlay) => &overlay.source,
            Self::Hyperlink(link) => &link.source,
        }
    }

    pub fn confidence(&self) -> f32 {
        match self {
            Self::Block { block, .. } => block.confidence,
            Self::TableCell { table, .. } => table.confidence,
            Self::Overlay(_) | Self::Hyperlink(_) => 1.0,
        }
    }

    pub fn reading_order(&self) -> Option<u32> {
        match self {
            Self::Block { block, container } => Some(
                container
                    .map(|container| container.reading_order)
                    .unwrap_or(block.reading_order),
            ),
            Self::TableCell { table, .. } => Some(table.reading_order),
            Self::Overlay(_) | Self::Hyperlink(_) => None,
        }
    }

    pub fn anchor_block(&self) -> Option<&'a Block> {
        match self {
            Self::Block { block, container } => Some(container.unwrap_or(block)),
            Self::TableCell { table, .. } => Some(table),
            Self::Overlay(_) | Self::Hyperlink(_) => None,
        }
    }
}

impl Document {
    pub fn resolve_object(&self, id: &str) -> Option<DocumentObject<'_>> {
        for block in &self.blocks {
            if let Some(found) = find_in_block(block, None, id, 0) {
                return Some(found);
            }
        }
        for page in &self.pages {
            if let Some(overlay) = page.overlays.iter().find(|overlay| overlay.id.as_str() == id) {
                return Some(DocumentObject::Overlay(overlay));
            }
        }
        self.links
            .iter()
            .find(|link| link.id.as_str() == id)
            .map(DocumentObject::Hyperlink)
    }

    pub fn object_ids(&self) -> Vec<&ObjectId> {
        let mut ids = Vec::new();
        for block in &self.blocks {
            collect_block_ids(block, &mut ids, 0);
        }
        for page in &self.pages {
            ids.extend(page.overlays.iter().map(|overlay| &overlay.id));
        }
        ids.extend(self.links.iter().map(|link| &link.id));
        ids
    }
}

fn find_in_block<'a>(
    block: &'a Block,
    top_level: Option<&'a Block>,
    id: &str,
    depth: usize,
) -> Option<DocumentObject<'a>> {
    if depth > MAX_OBJECT_NESTING {
        return None;
    }
    if block.id.as_str() == id {
        return Some(DocumentObject::Block {
            block,
            container: top_level,
        });
    }
    let BlockContent::Table(table) = &block.content else {
        return None;
    };
    let anchor = top_level.unwrap_or(block);
    for cell in &table.cells {
        if cell.id.as_str() == id {
            return Some(DocumentObject::TableCell { table: anchor, cell });
        }
        for nested in &cell.blocks {
            if let Some(found) = find_in_block(nested, Some(anchor), id, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

fn collect_block_ids<'a>(block: &'a Block, ids: &mut Vec<&'a ObjectId>, depth: usize) {
    if depth > MAX_OBJECT_NESTING {
        return;
    }
    ids.push(&block.id);
    if let BlockContent::Table(table) = &block.content {
        for cell in &table.cells {
            ids.push(&cell.id);
            for nested in &cell.blocks {
                collect_block_ids(nested, ids, depth + 1);
            }
        }
    }
}
