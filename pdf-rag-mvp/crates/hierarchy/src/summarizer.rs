use anyhow::Result;
use nabla_pdf_rag_contracts::*;
use nabla_pdf_rag_core::*;
use uuid::Uuid;

use crate::chunker;

/// Truncate a string to at most `max_bytes`, respecting UTF-8 char boundaries.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// RAPTOR-lite hierarchy builder.
///
/// 1. Split extracted document into chunks (rule-based, by token budget)
/// 2. Generate section summaries via LLM
/// 3. Generate a single document summary via LLM
///
/// Cluster summaries are deferred to a later milestone — for MVP,
/// section + document summaries are sufficient.
pub struct RaptorLiteBuilder {
    pub chunk_max_tokens: u32,
}

impl Default for RaptorLiteBuilder {
    fn default() -> Self {
        Self {
            chunk_max_tokens: 256,
        }
    }
}

impl HierarchyBuilder for RaptorLiteBuilder {
    fn build(
        &self,
        document: &ExtractedDocument,
        llm: &dyn LlmClient,
        progress: &dyn ProgressSink,
    ) -> Result<HierarchyBuildOutput> {
        progress.on_progress(&ProgressUpdate {
            stage: PipelineStage::Structure,
            current: 0,
            total: 3,
            message: Some("Chunking document".into()),
        });

        // Step 1: Chunk all pages
        let chunks = chunker::chunk_pages(
            &document.document_id,
            &document.pages,
            &[],
            self.chunk_max_tokens,
        );

        if chunks.is_empty() {
            return Ok(HierarchyBuildOutput {
                summary_nodes: vec![],
                chunks: vec![],
            });
        }

        progress.on_progress(&ProgressUpdate {
            stage: PipelineStage::Summarize,
            current: 1,
            total: 3,
            message: Some(format!("Summarizing {} chunks", chunks.len())),
        });

        // Step 2: Build section summaries (one per ~5 chunks)
        let section_size = 5;
        let mut section_nodes = Vec::new();
        let mut chunk_ids_by_section: Vec<Vec<ChunkId>> = Vec::new();

        for section_chunks in chunks.chunks(section_size) {
            let combined_text: String = section_chunks
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");

            let prompt = format!(
                "Summarize the following text in 2-3 sentences. Be concise and factual.\n\n{}",
                truncate_str(&combined_text, 4000)
            );

            let summary = llm.complete(&prompt, 200)?;

            let page_start = section_chunks
                .first()
                .and_then(|c| c.page_span.as_ref().map(|p| p.start));
            let page_end = section_chunks
                .last()
                .and_then(|c| c.page_span.as_ref().map(|p| p.end));

            let node_id = SummaryNodeId::new(Uuid::new_v4().to_string());
            let chunk_ids: Vec<ChunkId> = section_chunks.iter().map(|c| c.id.clone()).collect();

            section_nodes.push(SummaryNode {
                id: node_id,
                document_id: document.document_id.clone(),
                parent_id: None, // will be set after document node is created
                kind: SummaryNodeKind::Section,
                depth: 1,
                ordinal: section_nodes.len() as u32,
                title: format!("Section {}", section_nodes.len() + 1),
                page_span: page_start
                    .zip(page_end)
                    .map(|(s, e)| PageSpan { start: s, end: e }),
                summary,
                child_ids: vec![],
                source_chunk_ids: chunk_ids.clone(),
            });
            chunk_ids_by_section.push(chunk_ids);
        }

        progress.on_progress(&ProgressUpdate {
            stage: PipelineStage::Summarize,
            current: 2,
            total: 3,
            message: Some("Generating document summary".into()),
        });

        // Step 3: Build document summary from section summaries
        let sections_text: String = section_nodes
            .iter()
            .map(|n| n.summary.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");

        let doc_summary_prompt = format!(
            "Synthesize the following section summaries into a single cohesive document summary (3-5 sentences).\n\n{}",
            truncate_str(&sections_text, 4000)
        );

        let doc_summary = llm.complete(&doc_summary_prompt, 300)?;

        let doc_node_id = SummaryNodeId::new(Uuid::new_v4().to_string());
        let section_ids: Vec<SummaryNodeId> = section_nodes.iter().map(|n| n.id.clone()).collect();

        // Set parent_id on section nodes
        for node in &mut section_nodes {
            node.parent_id = Some(doc_node_id.clone());
        }

        let doc_node = SummaryNode {
            id: doc_node_id,
            document_id: document.document_id.clone(),
            parent_id: None,
            kind: SummaryNodeKind::Document,
            depth: 0,
            ordinal: 0,
            title: document
                .inferred_title
                .clone()
                .unwrap_or_else(|| "Untitled".into()),
            page_span: Some(PageSpan {
                start: 1,
                end: document.pages.len() as u32,
            }),
            summary: doc_summary,
            child_ids: section_ids,
            source_chunk_ids: vec![],
        };

        let mut summary_nodes = vec![doc_node];
        summary_nodes.extend(section_nodes);

        progress.on_progress(&ProgressUpdate {
            stage: PipelineStage::Summarize,
            current: 3,
            total: 3,
            message: Some("Hierarchy complete".into()),
        });

        Ok(HierarchyBuildOutput {
            summary_nodes,
            chunks,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockLlm;

    impl LlmClient for MockLlm {
        fn complete(&self, _prompt: &str, _max_tokens: u32) -> Result<String> {
            Ok("This is a mock summary.".into())
        }

        fn complete_json(
            &self,
            _prompt: &str,
            _max_tokens: u32,
        ) -> Result<serde_json::Value> {
            Ok(serde_json::json!({}))
        }

        fn max_context_tokens(&self) -> u32 {
            4096
        }
    }

    fn make_document(num_pages: usize) -> ExtractedDocument {
        let pages: Vec<ExtractedPage> = (0..num_pages)
            .map(|i| ExtractedPage {
                page_number: (i + 1) as u32,
                text: format!(
                    "This is page {}. It contains some text about topic {} with details.",
                    i + 1,
                    i + 1
                ),
            })
            .collect();
        ExtractedDocument {
            document_id: DocumentId::new("test-doc"),
            inferred_title: Some("Test Paper".into()),
            pages,
        }
    }

    #[test]
    fn builds_hierarchy_for_small_document() {
        let builder = RaptorLiteBuilder {
            chunk_max_tokens: 10,
        };
        let doc = make_document(3);
        let result = builder.build(&doc, &MockLlm, &NullProgress).unwrap();

        assert!(!result.chunks.is_empty());
        assert!(!result.summary_nodes.is_empty());

        // Should have at least one document-level summary
        let doc_nodes: Vec<_> = result
            .summary_nodes
            .iter()
            .filter(|n| n.kind == SummaryNodeKind::Document)
            .collect();
        assert_eq!(doc_nodes.len(), 1);
        assert_eq!(doc_nodes[0].title, "Test Paper");
        assert_eq!(doc_nodes[0].depth, 0);

        // Section nodes should point to document as parent
        let section_nodes: Vec<_> = result
            .summary_nodes
            .iter()
            .filter(|n| n.kind == SummaryNodeKind::Section)
            .collect();
        for sn in &section_nodes {
            assert_eq!(sn.parent_id.as_ref(), Some(&doc_nodes[0].id));
            assert_eq!(sn.depth, 1);
        }
    }

    #[test]
    fn empty_document_returns_empty() {
        let builder = RaptorLiteBuilder::default();
        let doc = ExtractedDocument {
            document_id: DocumentId::new("empty"),
            inferred_title: None,
            pages: vec![],
        };
        let result = builder.build(&doc, &MockLlm, &NullProgress).unwrap();
        assert!(result.chunks.is_empty());
        assert!(result.summary_nodes.is_empty());
    }
}
