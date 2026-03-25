use nabla_pdf_rag_contracts::{ChunkId, ChunkRecord, EmbeddingState, PageSpan};
use nabla_pdf_rag_core::ExtractedPage;
use uuid::Uuid;

/// Split pages into chunks of approximately `max_tokens` tokens.
/// Uses whitespace tokenization (word count) as a simple proxy for token count.
/// Each chunk preserves its page span and heading path.
pub fn chunk_pages(
    document_id: &nabla_pdf_rag_contracts::DocumentId,
    pages: &[ExtractedPage],
    heading_path: &[String],
    max_tokens: u32,
) -> Vec<ChunkRecord> {
    let mut chunks = Vec::new();
    let mut buf = String::new();
    let mut buf_tokens: u32 = 0;
    let mut span_start: Option<u32> = None;
    let mut span_end: u32 = 0;

    for page in pages {
        if span_start.is_none() {
            span_start = Some(page.page_number);
        }
        span_end = page.page_number;

        for word in page.text.split_whitespace() {
            if buf_tokens >= max_tokens && !buf.is_empty() {
                chunks.push(make_chunk(
                    document_id,
                    chunks.len() as u32,
                    heading_path,
                    span_start.unwrap(),
                    span_end,
                    std::mem::take(&mut buf),
                    buf_tokens,
                ));
                buf_tokens = 0;
                span_start = Some(page.page_number);
            }

            if !buf.is_empty() {
                buf.push(' ');
            }
            buf.push_str(word);
            buf_tokens += 1;
        }
    }

    // Flush remaining buffer
    if !buf.is_empty() {
        chunks.push(make_chunk(
            document_id,
            chunks.len() as u32,
            heading_path,
            span_start.unwrap_or(1),
            span_end,
            buf,
            buf_tokens,
        ));
    }

    chunks
}

fn make_chunk(
    document_id: &nabla_pdf_rag_contracts::DocumentId,
    ordinal: u32,
    heading_path: &[String],
    start: u32,
    end: u32,
    text: String,
    token_count: u32,
) -> ChunkRecord {
    ChunkRecord {
        id: ChunkId::new(Uuid::new_v4().to_string()),
        document_id: document_id.clone(),
        summary_node_id: None,
        ordinal,
        heading_path: heading_path.to_vec(),
        page_span: Some(PageSpan { start, end }),
        text,
        token_count,
        embedding_state: EmbeddingState::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nabla_pdf_rag_contracts::DocumentId;

    fn pages(texts: &[&str]) -> Vec<ExtractedPage> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| ExtractedPage {
                page_number: (i + 1) as u32,
                text: t.to_string(),
            })
            .collect()
    }

    #[test]
    fn single_page_single_chunk() {
        let doc_id = DocumentId::new("doc-1");
        let ps = pages(&["hello world foo bar"]);
        let chunks = chunk_pages(&doc_id, &ps, &[], 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].token_count, 4);
    }

    #[test]
    fn splits_at_max_tokens() {
        let doc_id = DocumentId::new("doc-1");
        let text = (0..20).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" ");
        let ps = pages(&[&text]);
        let chunks = chunk_pages(&doc_id, &ps, &["Intro".into()], 5);
        assert_eq!(chunks.len(), 4);
        for c in &chunks {
            assert!(c.token_count <= 5);
            assert_eq!(c.heading_path, vec!["Intro"]);
        }
    }

    #[test]
    fn preserves_page_spans() {
        let doc_id = DocumentId::new("doc-1");
        let ps = pages(&["a b c", "d e f", "g h i"]);
        let chunks = chunk_pages(&doc_id, &ps, &[], 4);
        assert!(chunks.len() >= 2);
        assert_eq!(chunks[0].page_span.as_ref().unwrap().start, 1);
    }
}
