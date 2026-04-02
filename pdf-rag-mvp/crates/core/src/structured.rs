use nabla_pdf_rag_contracts::DocumentId;
use serde::{Deserialize, Serialize};

// ─── Element Types ───────────────────────────────────────────────────────

/// The kind of a document element as recognized by a structure-aware parser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ElementKind {
    /// Document title (typically the first prominent heading).
    Title,
    /// Section heading with a nesting level (1 = top-level, 2 = subsection, …).
    SectionHeader { level: u8 },
    /// Body paragraph.
    Paragraph,
    /// Table — `text` holds the markdown representation.
    Table,
    /// Single list item (may appear consecutively for a full list).
    ListItem,
    /// Figure or image — `text` holds the caption if available.
    Figure,
    /// Code block.
    Code,
    /// Mathematical equation (LaTeX or Unicode).
    Equation,
    /// Running page header (non-body content).
    PageHeader,
    /// Running page footer (non-body content).
    PageFooter,
}

/// A single structural element extracted from the document.
///
/// Elements are stored in reading order. Heading hierarchy is implicit:
/// a `SectionHeader { level: N }` applies to all subsequent elements
/// until the next `SectionHeader` of equal or lesser level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocElement {
    pub kind: ElementKind,
    pub text: String,
    pub page_number: u32,
}

// ─── Structured Document ─────────────────────────────────────────────────

/// Structure-aware document representation.
///
/// This is the canonical hand-off type between the parser and hierarchy builder.
/// A structure-aware parser (Docling) populates full element types;
/// a fallback parser (pdf-extract) emits all-`Paragraph` elements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredDocument {
    pub document_id: DocumentId,
    pub title: Option<String>,
    pub page_count: u32,
    /// Body elements in reading order (excludes page headers/footers).
    pub elements: Vec<DocElement>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_document_roundtrips_json() {
        let doc = StructuredDocument {
            document_id: DocumentId::new("test"),
            title: Some("My Paper".into()),
            page_count: 3,
            elements: vec![
                DocElement {
                    kind: ElementKind::Title,
                    text: "My Paper".into(),
                    page_number: 1,
                },
                DocElement {
                    kind: ElementKind::SectionHeader { level: 1 },
                    text: "Introduction".into(),
                    page_number: 1,
                },
                DocElement {
                    kind: ElementKind::Paragraph,
                    text: "This paper explores…".into(),
                    page_number: 1,
                },
                DocElement {
                    kind: ElementKind::Table,
                    text: "| A | B |\n|---|---|\n| 1 | 2 |".into(),
                    page_number: 2,
                },
            ],
        };

        let json = serde_json::to_string(&doc).unwrap();
        let back: StructuredDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, back);
    }
}
