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

/// RAPTOR-lite hierarchy builder — now structure-aware.
///
/// 1. Split structured document into chunks via structure-aware chunking
/// 2. Group chunks by top-level heading (not arbitrary count)
/// 3. Generate section summaries via LLM with real heading titles
/// 4. Generate a single document summary from section summaries
pub struct RaptorLiteBuilder {
    pub chunk_max_tokens: u32,
    pub chunk_min_tokens: u32,
}

impl Default for RaptorLiteBuilder {
    fn default() -> Self {
        Self {
            chunk_max_tokens: 256,
            chunk_min_tokens: 32,
        }
    }
}

impl HierarchyBuilder for RaptorLiteBuilder {
    fn build(
        &self,
        document: &StructuredDocument,
        llm: &dyn LlmClient,
        progress: &dyn ProgressSink,
    ) -> Result<HierarchyBuildOutput> {
        progress.on_progress(&ProgressUpdate {
            stage: PipelineStage::Structure,
            current: 0,
            total: 3,
            message: Some("Chunking document".into()),
        });

        // Step 1: Structure-aware chunking
        let chunks = chunker::chunk_structured(
            &document.document_id,
            &document.elements,
            self.chunk_max_tokens,
            self.chunk_min_tokens,
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

        // Step 2: Group chunks by top-level heading for section summaries
        let section_groups = group_chunks_by_heading(&chunks);

        let mut section_nodes = Vec::new();

        for group in &section_groups {
            let combined_text: String = group
                .chunks
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");

            let prompt = format!(
                "Summarize the following text in 2-3 sentences. Be concise and factual.\n\n{}",
                truncate_str(&combined_text, 4000)
            );

            let summary = llm.complete(&prompt, 200)?;

            let page_start = group
                .chunks
                .first()
                .and_then(|c| c.page_span.as_ref().map(|p| p.start));
            let page_end = group
                .chunks
                .last()
                .and_then(|c| c.page_span.as_ref().map(|p| p.end));

            let node_id = SummaryNodeId::new(Uuid::new_v4().to_string());
            let chunk_ids: Vec<ChunkId> = group.chunks.iter().map(|c| c.id.clone()).collect();

            section_nodes.push(SummaryNode {
                id: node_id,
                document_id: document.document_id.clone(),
                parent_id: None, // set after document node is created
                kind: SummaryNodeKind::Section,
                depth: 1,
                ordinal: section_nodes.len() as u32,
                title: group.title.clone(),
                page_span: page_start
                    .zip(page_end)
                    .map(|(s, e)| PageSpan { start: s, end: e }),
                summary,
                child_ids: vec![],
                source_chunk_ids: chunk_ids,
            });
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
            .map(|n| format!("[{}] {}", n.title, n.summary))
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
                .title
                .clone()
                .unwrap_or_else(|| "Untitled".into()),
            page_span: Some(PageSpan {
                start: 1,
                end: document.page_count,
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

// ─── Section Grouping ────────────────────────────────────────────────────

struct SectionGroup<'a> {
    title: String,
    chunks: Vec<&'a ChunkRecord>,
}

/// Group chunks by their top-level heading.
///
/// If chunks have real heading paths (from Docling), groups are formed by
/// the first element of heading_path. If no headings exist (PdfExtract fallback),
/// falls back to positional grouping (every ~5 chunks).
fn group_chunks_by_heading<'a>(chunks: &'a [ChunkRecord]) -> Vec<SectionGroup<'a>> {
    let has_headings = chunks.iter().any(|c| !c.heading_path.is_empty());

    if has_headings {
        group_by_top_heading(chunks)
    } else {
        group_by_position(chunks, 5)
    }
}

/// Group chunks that share the same top-level heading.
fn group_by_top_heading<'a>(chunks: &'a [ChunkRecord]) -> Vec<SectionGroup<'a>> {
    let mut groups: Vec<SectionGroup<'a>> = Vec::new();

    for chunk in chunks {
        let top_heading = chunk
            .heading_path
            .first()
            .cloned()
            .unwrap_or_else(|| "(no heading)".into());

        // If the last group has the same top heading, append to it
        if let Some(last) = groups.last_mut() {
            if last.title == top_heading {
                last.chunks.push(chunk);
                continue;
            }
        }

        groups.push(SectionGroup {
            title: top_heading,
            chunks: vec![chunk],
        });
    }

    groups
}

/// Fallback: group every N chunks together with a positional title.
fn group_by_position<'a>(chunks: &'a [ChunkRecord], group_size: usize) -> Vec<SectionGroup<'a>> {
    chunks
        .chunks(group_size)
        .enumerate()
        .map(|(i, group)| {
            // Use first sentence of first chunk as title (better than "Section N")
            let title = group
                .first()
                .map(|c| {
                    c.text
                        .split_terminator(|ch: char| ch == '.' || ch == '\n')
                        .next()
                        .unwrap_or(&c.text)
                        .chars()
                        .take(80)
                        .collect::<String>()
                })
                .unwrap_or_else(|| format!("Section {}", i + 1));

            SectionGroup {
                title,
                chunks: group.iter().collect(),
            }
        })
        .collect()
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

    fn make_structured(elements: Vec<DocElement>) -> StructuredDocument {
        StructuredDocument {
            document_id: DocumentId::new("test-doc"),
            title: Some("Test Paper".into()),
            page_count: 3,
            elements,
        }
    }

    #[test]
    fn builds_hierarchy_with_headings() {
        let doc = make_structured(vec![
            DocElement {
                kind: ElementKind::SectionHeader { level: 1 },
                text: "Introduction".into(),
                page_number: 1,
            },
            DocElement {
                kind: ElementKind::Paragraph,
                text: "This paper explores something important about the topic at hand with details.".into(),
                page_number: 1,
            },
            DocElement {
                kind: ElementKind::SectionHeader { level: 1 },
                text: "Methods".into(),
                page_number: 2,
            },
            DocElement {
                kind: ElementKind::Paragraph,
                text: "We used the following methodology to investigate the research question here.".into(),
                page_number: 2,
            },
        ]);

        let builder = RaptorLiteBuilder {
            chunk_max_tokens: 100,
            chunk_min_tokens: 5,
        };
        let result = builder.build(&doc, &MockLlm, &NullProgress).unwrap();

        assert!(!result.chunks.is_empty());

        // Should have section nodes with real titles
        let section_nodes: Vec<_> = result
            .summary_nodes
            .iter()
            .filter(|n| n.kind == SummaryNodeKind::Section)
            .collect();

        let titles: Vec<&str> = section_nodes.iter().map(|n| n.title.as_str()).collect();
        assert!(titles.contains(&"Introduction"));
        assert!(titles.contains(&"Methods"));

        // Document node
        let doc_nodes: Vec<_> = result
            .summary_nodes
            .iter()
            .filter(|n| n.kind == SummaryNodeKind::Document)
            .collect();
        assert_eq!(doc_nodes.len(), 1);
        assert_eq!(doc_nodes[0].title, "Test Paper");
    }

    #[test]
    fn fallback_positional_grouping_without_headings() {
        // Simulate PdfExtract path: all paragraphs, no headings
        let elements: Vec<DocElement> = (0..12)
            .map(|i| DocElement {
                kind: ElementKind::Paragraph,
                text: format!("Paragraph {} with some words about topic {i}.", i + 1),
                page_number: (i / 4 + 1) as u32,
            })
            .collect();

        let doc = make_structured(elements);
        let builder = RaptorLiteBuilder {
            chunk_max_tokens: 20,
            chunk_min_tokens: 3,
        };
        let result = builder.build(&doc, &MockLlm, &NullProgress).unwrap();

        assert!(!result.chunks.is_empty());
        assert!(!result.summary_nodes.is_empty());

        // Section titles should NOT be "Section N" — they use first sentence
        let section_nodes: Vec<_> = result
            .summary_nodes
            .iter()
            .filter(|n| n.kind == SummaryNodeKind::Section)
            .collect();
        for sn in &section_nodes {
            assert!(!sn.title.starts_with("Section "), "Got generic title: {}", sn.title);
        }
    }

    #[test]
    fn empty_document_returns_empty() {
        let doc = StructuredDocument {
            document_id: DocumentId::new("empty"),
            title: None,
            page_count: 0,
            elements: vec![],
        };
        let builder = RaptorLiteBuilder::default();
        let result = builder.build(&doc, &MockLlm, &NullProgress).unwrap();
        assert!(result.chunks.is_empty());
        assert!(result.summary_nodes.is_empty());
    }
}
