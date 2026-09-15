use crate::{Block, BlockContent, DocsightError, Document, ObjectId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "rule", rename_all = "snake_case")]
pub enum CanonicalViolation {
    DuplicateObjectId {
        id: ObjectId,
    },
    PageOutOfOrder {
        previous: u32,
        current: u32,
    },
    BlockOutOfOrder {
        previous: ObjectId,
        current: ObjectId,
    },
    PageIndexUnknownBlock {
        page: u32,
        id: ObjectId,
    },
    PageIndexForeignBlock {
        page: u32,
        id: ObjectId,
        block_page: Option<u32>,
    },
    PageIndexMissingBlock {
        page: u32,
        id: ObjectId,
    },
    PageIndexOutOfOrder {
        page: u32,
        previous: ObjectId,
        current: ObjectId,
    },
    OverlayPageUnknown {
        page: u32,
        id: ObjectId,
    },
}

impl Display for CanonicalViolation {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateObjectId { id } => {
                write!(
                    formatter,
                    "object id {id} is assigned to more than one object"
                )
            }
            Self::PageOutOfOrder { previous, current } => write!(
                formatter,
                "page {current} follows page {previous} instead of ascending page order"
            ),
            Self::BlockOutOfOrder { previous, current } => write!(
                formatter,
                "block {current} follows block {previous} instead of ascending page and reading order"
            ),
            Self::PageIndexUnknownBlock { page, id } => write!(
                formatter,
                "page {page} indexes block {id} which does not exist"
            ),
            Self::PageIndexForeignBlock {
                page,
                id,
                block_page,
            } => match block_page {
                Some(block_page) => write!(
                    formatter,
                    "page {page} indexes block {id} which is placed on page {block_page}"
                ),
                None => write!(
                    formatter,
                    "page {page} indexes block {id} which has no page placement"
                ),
            },
            Self::PageIndexMissingBlock { page, id } => write!(
                formatter,
                "block {id} is placed on page {page} but is missing from the page index"
            ),
            Self::PageIndexOutOfOrder {
                page,
                previous,
                current,
            } => write!(
                formatter,
                "page {page} indexes block {current} after block {previous} instead of reading order"
            ),
            Self::OverlayPageUnknown { page, id } => write!(
                formatter,
                "overlay {id} references page {page} which the document does not declare"
            ),
        }
    }
}

pub fn canonical_violations(document: &Document) -> Vec<CanonicalViolation> {
    let mut violations = Vec::new();
    collect_duplicate_ids(document, &mut violations);
    collect_page_order(document, &mut violations);
    collect_block_order(document, &mut violations);
    collect_page_index(document, &mut violations);
    collect_overlay_pages(document, &mut violations);
    violations
}

pub fn validate_canonical(document: &Document) -> Result<(), DocsightError> {
    let violations = canonical_violations(document);
    if violations.is_empty() {
        return Ok(());
    }
    let message = violations
        .iter()
        .map(CanonicalViolation::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    Err(DocsightError::BackendFailure {
        backend: "document-ir".to_owned(),
        message: format!("canonical ordering contract violated: {message}"),
    })
}

fn visit_block_ids(block: &Block, visit: &mut impl FnMut(&ObjectId)) {
    visit(&block.id);
    if let BlockContent::Table(table) = &block.content {
        for cell in &table.cells {
            visit(&cell.id);
            for nested in &cell.blocks {
                visit_block_ids(nested, visit);
            }
        }
    }
}

fn collect_duplicate_ids(document: &Document, violations: &mut Vec<CanonicalViolation>) {
    let mut seen: BTreeSet<ObjectId> = BTreeSet::new();
    let mut duplicates: BTreeSet<ObjectId> = BTreeSet::new();
    let mut record = |id: &ObjectId| {
        if !seen.insert(id.clone()) {
            duplicates.insert(id.clone());
        }
    };
    for block in &document.blocks {
        visit_block_ids(block, &mut record);
    }
    for page in &document.pages {
        for overlay in &page.overlays {
            record(&overlay.id);
        }
    }
    for link in &document.links {
        record(&link.id);
    }
    for comment in &document.comments {
        record(&comment.id);
    }
    for section in &document.sections {
        record(&section.id);
    }
    for resource in &document.resources {
        record(&resource.id);
    }
    violations.extend(
        duplicates
            .into_iter()
            .map(|id| CanonicalViolation::DuplicateObjectId { id }),
    );
}

fn collect_page_order(document: &Document, violations: &mut Vec<CanonicalViolation>) {
    let mut previous: Option<u32> = None;
    for page in &document.pages {
        if let Some(previous_number) = previous
            && page.number <= previous_number
        {
            violations.push(CanonicalViolation::PageOutOfOrder {
                previous: previous_number,
                current: page.number,
            });
        }
        previous = Some(page.number);
    }
}

fn block_sort_key(block: &Block) -> (u32, u32) {
    (block.page.unwrap_or(0), block.reading_order)
}

fn collect_block_order(document: &Document, violations: &mut Vec<CanonicalViolation>) {
    let mut previous: Option<&Block> = None;
    for block in &document.blocks {
        if let Some(previous_block) = previous
            && block_sort_key(block) <= block_sort_key(previous_block)
        {
            violations.push(CanonicalViolation::BlockOutOfOrder {
                previous: previous_block.id.clone(),
                current: block.id.clone(),
            });
        }
        previous = Some(block);
    }
}

fn collect_page_index(document: &Document, violations: &mut Vec<CanonicalViolation>) {
    let blocks_by_id: BTreeMap<&ObjectId, &Block> = document
        .blocks
        .iter()
        .map(|block| (&block.id, block))
        .collect();
    for page in &document.pages {
        let mut indexed: BTreeSet<&ObjectId> = BTreeSet::new();
        let mut previous: Option<&Block> = None;
        for id in &page.block_ids {
            indexed.insert(id);
            let Some(block) = blocks_by_id.get(id) else {
                violations.push(CanonicalViolation::PageIndexUnknownBlock {
                    page: page.number,
                    id: id.clone(),
                });
                continue;
            };
            if block.page != Some(page.number) {
                violations.push(CanonicalViolation::PageIndexForeignBlock {
                    page: page.number,
                    id: id.clone(),
                    block_page: block.page,
                });
            }
            if let Some(previous_block) = previous
                && block.reading_order <= previous_block.reading_order
            {
                violations.push(CanonicalViolation::PageIndexOutOfOrder {
                    page: page.number,
                    previous: previous_block.id.clone(),
                    current: block.id.clone(),
                });
            }
            previous = Some(block);
        }
        for block in document
            .blocks
            .iter()
            .filter(|block| block.page == Some(page.number))
        {
            if !indexed.contains(&block.id) {
                violations.push(CanonicalViolation::PageIndexMissingBlock {
                    page: page.number,
                    id: block.id.clone(),
                });
            }
        }
    }
}

fn collect_overlay_pages(document: &Document, violations: &mut Vec<CanonicalViolation>) {
    let page_numbers: BTreeSet<u32> = document.pages.iter().map(|page| page.number).collect();
    for page in &document.pages {
        for overlay in &page.overlays {
            if !page_numbers.contains(&overlay.page) {
                violations.push(CanonicalViolation::OverlayPageUnknown {
                    page: overlay.page,
                    id: overlay.id.clone(),
                });
            }
        }
    }
}
