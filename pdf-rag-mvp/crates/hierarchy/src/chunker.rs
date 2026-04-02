use nabla_pdf_rag_contracts::{ChunkId, ChunkRecord, DocumentId, EmbeddingState, PageSpan};
use nabla_pdf_rag_core::{DocElement, ElementKind};
use uuid::Uuid;

/// Structure-aware chunking: respects heading boundaries, keeps tables intact,
/// and populates `heading_path` from actual document headings.
///
/// Algorithm:
/// 1. Walk elements in reading order, maintaining a heading stack.
/// 2. SectionHeader → flush current buffer, update heading stack.
/// 3. Table → emit as its own chunk (never split).
/// 4. Paragraph/ListItem/Code/Equation → accumulate until `max_tokens`.
/// 5. Each chunk gets `heading_path` from the current heading stack.
/// 6. Trailing undersized chunks merge with previous if under `min_tokens`.
pub fn chunk_structured(
    document_id: &DocumentId,
    elements: &[DocElement],
    max_tokens: u32,
    min_tokens: u32,
) -> Vec<ChunkRecord> {
    let mut chunks = Vec::new();
    let mut heading_stack: Vec<String> = Vec::new();
    let mut acc = Accumulator::new();

    for elem in elements {
        match &elem.kind {
            // Skip page furniture
            ElementKind::PageHeader | ElementKind::PageFooter => continue,

            // Title treated as top-level heading
            ElementKind::Title => {
                flush_acc(document_id, &heading_stack, &mut acc, &mut chunks, max_tokens);
                heading_stack.clear();
                heading_stack.push(elem.text.clone());
            }

            // Section headers: flush and update heading stack
            ElementKind::SectionHeader { level } => {
                flush_acc(document_id, &heading_stack, &mut acc, &mut chunks, max_tokens);
                let depth = (*level as usize).saturating_sub(1);
                // Pop back to parent level
                heading_stack.truncate(depth);
                heading_stack.push(elem.text.clone());
            }

            // Tables: always their own chunk (preserve structural integrity)
            ElementKind::Table => {
                flush_acc(document_id, &heading_stack, &mut acc, &mut chunks, max_tokens);
                let token_count = word_count(&elem.text);
                chunks.push(make_chunk(
                    document_id,
                    chunks.len() as u32,
                    &heading_stack,
                    elem.page_number,
                    elem.page_number,
                    elem.text.clone(),
                    token_count,
                ));
            }

            // Figures: emit caption as a small chunk
            ElementKind::Figure => {
                if !elem.text.is_empty() {
                    flush_acc(document_id, &heading_stack, &mut acc, &mut chunks, max_tokens);
                    let token_count = word_count(&elem.text);
                    chunks.push(make_chunk(
                        document_id,
                        chunks.len() as u32,
                        &heading_stack,
                        elem.page_number,
                        elem.page_number,
                        elem.text.clone(),
                        token_count,
                    ));
                }
            }

            // Accumulating elements: Paragraph, ListItem, Code, Equation
            _ => {
                let elem_tokens = word_count(&elem.text);

                // If adding this element would exceed max_tokens, flush first
                if acc.token_count > 0 && acc.token_count + elem_tokens > max_tokens {
                    flush_acc(document_id, &heading_stack, &mut acc, &mut chunks, max_tokens);
                }

                acc.append(&elem.text, elem_tokens, elem.page_number);
            }
        }
    }

    // Flush remaining accumulator
    flush_acc(document_id, &heading_stack, &mut acc, &mut chunks, max_tokens);

    // Merge trailing undersized chunk with previous if they share heading_path
    merge_undersized_tail(&mut chunks, min_tokens, max_tokens);

    // Re-number ordinals
    for (i, chunk) in chunks.iter_mut().enumerate() {
        chunk.ordinal = i as u32;
    }

    chunks
}

// ─── Internal helpers ────────────────────────────────────────────────────

struct Accumulator {
    text: String,
    token_count: u32,
    page_start: Option<u32>,
    page_end: u32,
}

impl Accumulator {
    fn new() -> Self {
        Self {
            text: String::new(),
            token_count: 0,
            page_start: None,
            page_end: 0,
        }
    }

    fn append(&mut self, text: &str, tokens: u32, page: u32) {
        if self.page_start.is_none() {
            self.page_start = Some(page);
        }
        self.page_end = page;
        if !self.text.is_empty() {
            self.text.push_str("\n\n");
        }
        self.text.push_str(text);
        self.token_count += tokens;
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn take(&mut self) -> (String, u32, u32, u32) {
        let text = std::mem::take(&mut self.text);
        let tokens = self.token_count;
        let start = self.page_start.unwrap_or(1);
        let end = self.page_end;
        self.token_count = 0;
        self.page_start = None;
        self.page_end = 0;
        (text, tokens, start, end)
    }
}

fn flush_acc(
    document_id: &DocumentId,
    heading_stack: &[String],
    acc: &mut Accumulator,
    chunks: &mut Vec<ChunkRecord>,
    max_tokens: u32,
) {
    if acc.is_empty() {
        return;
    }

    let (text, token_count, page_start, page_end) = acc.take();

    // If the accumulated text exceeds max_tokens, split it
    if token_count > max_tokens {
        split_and_push(document_id, heading_stack, &text, page_start, page_end, chunks, max_tokens);
    } else {
        chunks.push(make_chunk(
            document_id,
            chunks.len() as u32,
            heading_stack,
            page_start,
            page_end,
            text,
            token_count,
        ));
    }
}

/// Split oversized text by token budget (word-boundary splitting).
fn split_and_push(
    document_id: &DocumentId,
    heading_stack: &[String],
    text: &str,
    page_start: u32,
    page_end: u32,
    chunks: &mut Vec<ChunkRecord>,
    max_tokens: u32,
) {
    let words: Vec<&str> = text.split_whitespace().collect();
    let max = max_tokens as usize;

    for word_chunk in words.chunks(max) {
        let chunk_text = word_chunk.join(" ");
        let token_count = word_chunk.len() as u32;
        chunks.push(make_chunk(
            document_id,
            chunks.len() as u32,
            heading_stack,
            page_start,
            page_end,
            chunk_text,
            token_count,
        ));
    }
}

/// If the last chunk is below min_tokens and shares heading_path with the
/// previous chunk, merge them (unless the merged result exceeds max_tokens,
/// or the previous chunk is a table/figure which should stay isolated).
fn merge_undersized_tail(chunks: &mut Vec<ChunkRecord>, min_tokens: u32, max_tokens: u32) {
    if chunks.len() < 2 {
        return;
    }

    let last_idx = chunks.len() - 1;
    let prev_idx = last_idx - 1;

    if chunks[last_idx].token_count >= min_tokens {
        return;
    }
    if chunks[last_idx].heading_path != chunks[prev_idx].heading_path {
        return;
    }
    if chunks[prev_idx].token_count + chunks[last_idx].token_count > max_tokens {
        return;
    }
    // Don't merge into a table chunk (tables start with "|" in markdown)
    if chunks[prev_idx].text.starts_with('|') {
        return;
    }

    let last = chunks.pop().unwrap();
    let prev = chunks.last_mut().unwrap();
    prev.text.push_str("\n\n");
    prev.text.push_str(&last.text);
    prev.token_count += last.token_count;
    if let (Some(ref mut prev_span), Some(last_span)) = (&mut prev.page_span, &last.page_span) {
        prev_span.end = last_span.end;
    }
}

fn word_count(text: &str) -> u32 {
    text.split_whitespace().count() as u32
}

fn make_chunk(
    document_id: &DocumentId,
    ordinal: u32,
    heading_path: &[String],
    page_start: u32,
    page_end: u32,
    text: String,
    token_count: u32,
) -> ChunkRecord {
    ChunkRecord {
        id: ChunkId::new(Uuid::new_v4().to_string()),
        document_id: document_id.clone(),
        summary_node_id: None,
        ordinal,
        heading_path: heading_path.to_vec(),
        page_span: Some(PageSpan {
            start: page_start,
            end: page_end,
        }),
        text,
        token_count,
        embedding_state: EmbeddingState::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elem(kind: ElementKind, text: &str, page: u32) -> DocElement {
        DocElement {
            kind,
            text: text.into(),
            page_number: page,
        }
    }

    #[test]
    fn heading_boundaries_create_separate_chunks() {
        let doc_id = DocumentId::new("doc-1");
        let elements = vec![
            elem(ElementKind::SectionHeader { level: 1 }, "Introduction", 1),
            elem(ElementKind::Paragraph, "First paragraph about intro.", 1),
            elem(ElementKind::SectionHeader { level: 1 }, "Methods", 2),
            elem(ElementKind::Paragraph, "We used the following method.", 2),
        ];

        let chunks = chunk_structured(&doc_id, &elements, 100, 5);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading_path, vec!["Introduction"]);
        assert_eq!(chunks[1].heading_path, vec!["Methods"]);
    }

    #[test]
    fn tables_get_own_chunk() {
        let doc_id = DocumentId::new("doc-1");
        let elements = vec![
            elem(ElementKind::Paragraph, "Some text before.", 1),
            elem(ElementKind::Table, "| A | B |\n|---|---|\n| 1 | 2 |", 1),
            elem(ElementKind::Paragraph, "Some text after.", 1),
        ];

        let chunks = chunk_structured(&doc_id, &elements, 100, 5);

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[1].text, "| A | B |\n|---|---|\n| 1 | 2 |");
    }

    #[test]
    fn nested_headings_build_path() {
        let doc_id = DocumentId::new("doc-1");
        let elements = vec![
            elem(ElementKind::SectionHeader { level: 1 }, "Chapter 1", 1),
            elem(ElementKind::SectionHeader { level: 2 }, "Section 1.1", 1),
            elem(ElementKind::Paragraph, "Content under 1.1", 1),
            elem(ElementKind::SectionHeader { level: 2 }, "Section 1.2", 2),
            elem(ElementKind::Paragraph, "Content under 1.2", 2),
            elem(ElementKind::SectionHeader { level: 1 }, "Chapter 2", 3),
            elem(ElementKind::Paragraph, "Content under chapter 2", 3),
        ];

        let chunks = chunk_structured(&doc_id, &elements, 100, 3);

        assert_eq!(chunks[0].heading_path, vec!["Chapter 1", "Section 1.1"]);
        assert_eq!(chunks[1].heading_path, vec!["Chapter 1", "Section 1.2"]);
        assert_eq!(chunks[2].heading_path, vec!["Chapter 2"]);
    }

    #[test]
    fn respects_max_tokens() {
        let doc_id = DocumentId::new("doc-1");
        let long_text = (0..20).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" ");
        let elements = vec![elem(ElementKind::Paragraph, &long_text, 1)];

        let chunks = chunk_structured(&doc_id, &elements, 5, 2);

        assert_eq!(chunks.len(), 4);
        for c in &chunks {
            assert!(c.token_count <= 5);
        }
    }

    #[test]
    fn merges_undersized_tail() {
        let doc_id = DocumentId::new("doc-1");
        let elements = vec![
            elem(ElementKind::Paragraph, "word1 word2 word3 word4 word5", 1),
            elem(ElementKind::Paragraph, "word6", 1),
        ];

        let chunks = chunk_structured(&doc_id, &elements, 10, 3);

        // word6 is below min_tokens=3, should merge with previous
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("word6"));
    }

    #[test]
    fn empty_elements_produce_no_chunks() {
        let doc_id = DocumentId::new("doc-1");
        let chunks = chunk_structured(&doc_id, &[], 100, 5);
        assert!(chunks.is_empty());
    }

    #[test]
    fn page_headers_and_footers_are_skipped() {
        let doc_id = DocumentId::new("doc-1");
        let elements = vec![
            elem(ElementKind::PageHeader, "Page 1", 1),
            elem(ElementKind::Paragraph, "Real content.", 1),
            elem(ElementKind::PageFooter, "Footer", 1),
        ];

        let chunks = chunk_structured(&doc_id, &elements, 100, 1);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Real content.");
    }
}
