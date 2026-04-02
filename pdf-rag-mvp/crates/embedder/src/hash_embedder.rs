use anyhow::Result;
use nabla_pdf_rag_contracts::ChunkRecord;
use nabla_pdf_rag_core::{
    Embedder, EmbeddingBatchResult, EmbeddingRecord, PipelineStage, ProgressSink, ProgressUpdate,
};

/// Deterministic hash-based embedder for development and testing.
///
/// Produces normalized vectors from text content using a simple hash function.
/// NOT suitable for production — use an ONNX model (bge-small-zh-v1.5)
/// via the `ort` crate for real semantic embeddings.
pub struct HashEmbedder {
    pub dimensions: usize,
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self { dimensions: 384 }
    }
}

impl HashEmbedder {
    fn embed_text(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; self.dimensions];
        for (i, b) in text.bytes().enumerate() {
            v[i % self.dimensions] += b as f32 / 255.0;
        }
        // L2 normalize
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        v.iter_mut().for_each(|x| *x /= norm);
        v
    }
}

impl Embedder for HashEmbedder {
    fn embed_chunks(
        &self,
        chunks: &[ChunkRecord],
        progress: &dyn ProgressSink,
    ) -> Result<EmbeddingBatchResult> {
        let total = chunks.len() as u64;
        let mut indexed = Vec::with_capacity(chunks.len());

        for (i, chunk) in chunks.iter().enumerate() {
            let vector = self.embed_text(&chunk.text);
            indexed.push(EmbeddingRecord {
                chunk_id: chunk.id.clone(),
                vector,
            });

            if (i + 1) % 50 == 0 || i + 1 == chunks.len() {
                progress.on_progress(&ProgressUpdate {
                    stage: PipelineStage::Embed,
                    current: (i + 1) as u64,
                    total,
                    message: None,
                });
            }
        }

        Ok(EmbeddingBatchResult {
            indexed,
            failed: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nabla_pdf_rag_contracts::{ChunkId, DocumentId, EmbeddingState};
    use nabla_pdf_rag_core::NullProgress;

    fn make_chunk(text: &str) -> ChunkRecord {
        ChunkRecord {
            id: ChunkId::new(format!("c-{}", text.len())),
            document_id: DocumentId::new("doc-1"),
            summary_node_id: None,
            ordinal: 0,
            heading_path: vec![],
            page_span: None,
            text: text.into(),
            token_count: text.split_whitespace().count() as u32,
            embedding_state: EmbeddingState::Pending,
        }
    }

    #[test]
    fn produces_normalized_vectors() {
        let embedder = HashEmbedder::default();
        let chunks = vec![make_chunk("hello world")];
        let result = embedder.embed_chunks(&chunks, &NullProgress).unwrap();

        assert_eq!(result.indexed.len(), 1);
        assert_eq!(result.failed.len(), 0);

        let v = &result.indexed[0].vector;
        assert_eq!(v.len(), 384);

        // Check L2 norm ≈ 1.0
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 0.001);
    }

    #[test]
    fn different_texts_produce_different_vectors() {
        let embedder = HashEmbedder::default();
        let chunks = vec![make_chunk("hello"), make_chunk("world")];
        let result = embedder.embed_chunks(&chunks, &NullProgress).unwrap();

        assert_ne!(result.indexed[0].vector, result.indexed[1].vector);
    }

    #[test]
    fn deterministic() {
        let embedder = HashEmbedder::default();
        let chunks = vec![make_chunk("consistent output")];
        let r1 = embedder.embed_chunks(&chunks, &NullProgress).unwrap();
        let r2 = embedder.embed_chunks(&chunks, &NullProgress).unwrap();
        assert_eq!(r1.indexed[0].vector, r2.indexed[0].vector);
    }
}
