use anyhow::{bail, Context, Result};
use nabla_pdf_rag_contracts::ChunkRecord;
use nabla_pdf_rag_core::{
    Embedder, EmbeddingBatchResult, EmbeddingRecord, PipelineStage, ProgressSink, ProgressUpdate,
};
use serde::Deserialize;

/// OpenAI-compatible embedding API client.
///
/// Uses `ureq` (pure sync HTTP) to avoid tokio runtime conflicts.
/// Works with any provider: OpenAI, GLM/ZhipuAI, Jina, Voyage, vLLM, etc.
pub struct ApiEmbedder {
    api_key: String,
    base_url: String,
    model: String,
    dimensions: usize,
    batch_size: usize,
}

impl ApiEmbedder {
    pub fn new(
        api_key: impl Into<String>,
        base_url: Option<String>,
        model: Option<String>,
        dimensions: Option<usize>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".into()),
            model: model.unwrap_or_else(|| "text-embedding-3-small".into()),
            dimensions: dimensions.unwrap_or(1536),
            batch_size: 64,
        }
    }

    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let url = format!("{}/embeddings", self.base_url);

        let mut body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        // Include dimensions for models that support it
        body["dimensions"] = serde_json::json!(self.dimensions);

        let resp: serde_json::Value = ureq::post(&url)
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .send_json(&body)
            .context("Embedding API request failed")?
            .body_mut()
            .read_json()
            .context("Failed to parse embedding response")?;

        if let Some(err) = resp.get("error") {
            let msg = err["message"].as_str().unwrap_or("unknown error");
            bail!("Embedding API error: {msg}");
        }

        let data = resp["data"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("No 'data' array in embedding response"))?;

        let mut items: Vec<EmbeddingItem> = data
            .iter()
            .map(|item| serde_json::from_value(item.clone()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to parse embedding items")?;

        items.sort_by_key(|v| v.index);
        Ok(items.into_iter().map(|v| v.embedding).collect())
    }
}

#[derive(Deserialize)]
struct EmbeddingItem {
    index: usize,
    embedding: Vec<f32>,
}

impl Embedder for ApiEmbedder {
    fn embed_chunks(
        &self,
        chunks: &[ChunkRecord],
        progress: &dyn ProgressSink,
    ) -> Result<EmbeddingBatchResult> {
        let total = chunks.len();
        let mut indexed = Vec::with_capacity(total);
        let mut failed = Vec::new();

        for batch_start in (0..total).step_by(self.batch_size) {
            let batch_end = (batch_start + self.batch_size).min(total);
            let batch = &chunks[batch_start..batch_end];
            let texts: Vec<&str> = batch.iter().map(|c| c.text.as_str()).collect();

            match self.embed_batch(&texts) {
                Ok(vectors) => {
                    for (chunk, vector) in batch.iter().zip(vectors.into_iter()) {
                        indexed.push(EmbeddingRecord {
                            chunk_id: chunk.id.clone(),
                            vector,
                        });
                    }
                }
                Err(e) => {
                    for chunk in batch {
                        failed.push((chunk.id.clone(), e.to_string()));
                    }
                }
            }

            progress.on_progress(&ProgressUpdate {
                stage: PipelineStage::Embed,
                current: batch_end as u64,
                total: total as u64,
                message: None,
            });
        }

        Ok(EmbeddingBatchResult { indexed, failed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_default() {
        let e = ApiEmbedder::new("fake-key", None, None, None);
        assert_eq!(e.dimensions(), 1536);
    }

    #[test]
    fn dimensions_custom() {
        let e = ApiEmbedder::new("fake-key", None, None, Some(384));
        assert_eq!(e.dimensions(), 384);
    }
}
