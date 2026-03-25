use std::time::Duration;

use anyhow::{bail, Context, Result};
use nabla_pdf_rag_contracts::ChunkRecord;
use nabla_pdf_rag_core::{
    Embedder, EmbeddingBatchResult, EmbeddingRecord, PipelineStage, ProgressSink, ProgressUpdate,
};
use serde::Deserialize;

/// OpenAI-compatible embedding API client.
///
/// Works with any provider that implements the `/embeddings` endpoint:
/// OpenAI, Azure OpenAI, Jina, Voyage, local vLLM, etc.
pub struct ApiEmbedder {
    client: reqwest::blocking::Client,
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
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client"),
            api_key: api_key.into(),
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com/v1".into()),
            model: model.unwrap_or_else(|| "text-embedding-3-small".into()),
            dimensions: dimensions.unwrap_or(1536),
            batch_size: 64, // OpenAI allows up to 2048 inputs, but 64 is safe for most providers
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

        // Only include dimensions if model supports it (OpenAI text-embedding-3-*)
        if self.model.contains("text-embedding-3") {
            body["dimensions"] = serde_json::json!(self.dimensions);
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .context("Embedding API request failed")?;

        let status = resp.status();
        let resp_body: serde_json::Value = resp.json().context("Failed to parse embedding response")?;

        if !status.is_success() {
            let msg = resp_body["error"]["message"]
                .as_str()
                .unwrap_or("unknown error");
            bail!("Embedding API {status}: {msg}");
        }

        let data = resp_body["data"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("No 'data' array in embedding response"))?;

        let mut vectors: Vec<EmbeddingItem> = data
            .iter()
            .map(|item| serde_json::from_value(item.clone()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to parse embedding items")?;

        // Sort by index to match input order
        vectors.sort_by_key(|v| v.index);

        Ok(vectors.into_iter().map(|v| v.embedding).collect())
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
