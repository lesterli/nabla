use anyhow::{Context, Result};
use arrow_array::{Array, Float32Array, StringArray};
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::query::{ExecutableQuery, QueryBase, QueryExecutionOptions};
use lancedb::DistanceType;

use crate::store::LanceStore;

/// A single search result from LanceDB.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub chunk_id: String,
    pub document_id: String,
    pub text: String,
    pub score: f32,
}

/// Performs hybrid search (vector + BM25 + RRF) over the LanceDB chunks table.
pub struct HybridSearcher<'a> {
    store: &'a LanceStore,
}

impl<'a> HybridSearcher<'a> {
    pub fn new(store: &'a LanceStore) -> Self {
        Self { store }
    }

    /// Vector-only search (ANN).
    pub async fn vector_search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchHit>> {
        let table = self.store.open_table().await?;
        let batches = table
            .vector_search(query_embedding)?
            .distance_type(DistanceType::Cosine)
            .limit(top_k)
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await
            .context("Vector search failed")?;

        Ok(extract_hits(&batches))
    }

    /// Full-text search (BM25).
    pub async fn fts_search(
        &self,
        query_text: &str,
        top_k: usize,
    ) -> Result<Vec<SearchHit>> {
        let table = self.store.open_table().await?;
        let batches = table
            .query()
            .full_text_search(FullTextSearchQuery::new(query_text.to_owned()))
            .limit(top_k)
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await
            .context("FTS search failed")?;

        Ok(extract_hits(&batches))
    }

    /// Hybrid search: vector + BM25 fused with RRF.
    pub async fn hybrid_search(
        &self,
        query_text: &str,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SearchHit>> {
        let table = self.store.open_table().await?;
        let batches = table
            .query()
            .full_text_search(FullTextSearchQuery::new(query_text.to_owned()))
            .nearest_to(query_embedding)?
            .limit(top_k)
            .execute_hybrid(QueryExecutionOptions::default())
            .await?
            .try_collect::<Vec<_>>()
            .await
            .context("Hybrid search failed")?;

        Ok(extract_hits(&batches))
    }
}

fn extract_hits(batches: &[arrow_array::RecordBatch]) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    for batch in batches {
        let chunk_ids = batch
            .column_by_name("chunk_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let doc_ids = batch
            .column_by_name("document_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let texts = batch
            .column_by_name("text")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());

        // Try to extract actual relevance score from LanceDB
        // hybrid → _relevance_score, vector → _distance, FTS → ordering
        let scores = batch
            .column_by_name("_relevance_score")
            .or_else(|| batch.column_by_name("_distance"))
            .and_then(|c| c.as_any().downcast_ref::<Float32Array>());

        let (Some(chunk_ids), Some(doc_ids), Some(texts)) = (chunk_ids, doc_ids, texts) else {
            continue;
        };

        for i in 0..batch.num_rows() {
            let score = scores.map(|s| s.value(i)).unwrap_or(0.0);
            hits.push(SearchHit {
                chunk_id: chunk_ids.value(i).to_string(),
                document_id: doc_ids.value(i).to_string(),
                text: texts.value(i).to_string(),
                score,
            });
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ChunkEmbedding;
    use crate::LanceStore;
    use nabla_pdf_rag_contracts::ChunkId;
    use nabla_pdf_rag_core::EmbeddingRecord;

    async fn setup_store(dir: &std::path::Path) -> LanceStore {
        let store = LanceStore::open(dir.to_str().unwrap(), 8).await.unwrap();

        let test_data: Vec<(&str, &str)> = vec![
            ("c-0", "RAPTOR builds hierarchical summaries for document retrieval"),
            ("c-1", "BM25 is a probabilistic ranking function for full-text search"),
            ("c-2", "Reciprocal Rank Fusion combines multiple retrieval channels"),
            ("c-3", "PDF parsing extracts text and reading order from pages"),
            ("c-4", "Vector embeddings capture semantic similarity between passages"),
            ("c-5", "LanceDB is an embedded vector database with hybrid search"),
            ("c-6", "Scientific papers often use two-column layouts"),
            ("c-7", "Support-oppose framing affects attitude sharing behavior"),
            ("c-8", "Machine learning models require large training datasets"),
            ("c-9", "Bitcoin vaults enable trustless collateral management"),
        ];

        let records: Vec<EmbeddingRecord> = test_data
            .iter()
            .enumerate()
            .map(|(i, (id, _))| {
                let mut v = vec![0.1f32; 8];
                v[i % 8] += 0.5;
                let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                v.iter_mut().for_each(|x| *x /= norm);
                EmbeddingRecord {
                    chunk_id: ChunkId::new(*id),
                    vector: v,
                }
            })
            .collect();

        let items: Vec<ChunkEmbedding> = records
            .iter()
            .zip(test_data.iter())
            .map(|(r, (_, text))| ChunkEmbedding {
                record: r,
                document_id: "doc-1",
                text,
            })
            .collect();

        store.upsert_embeddings(&items).await.unwrap();
        store
    }

    #[tokio::test]
    async fn fts_search_finds_relevant_chunks() {
        let dir = std::env::temp_dir().join("nabla-lance-search-fts");
        let _ = std::fs::remove_dir_all(&dir);

        let store = setup_store(&dir).await;
        let searcher = HybridSearcher::new(&store);

        let results = searcher.fts_search("vector database hybrid", 3).await.unwrap();
        assert!(!results.is_empty());
        // Should find the LanceDB chunk
        assert!(results.iter().any(|h| h.text.contains("LanceDB")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn vector_search_returns_results() {
        let dir = std::env::temp_dir().join("nabla-lance-search-vec");
        let _ = std::fs::remove_dir_all(&dir);

        let store = setup_store(&dir).await;
        let searcher = HybridSearcher::new(&store);

        let mut query = vec![0.1f32; 8];
        query[0] += 0.5;
        let norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        query.iter_mut().for_each(|x| *x /= norm);

        let results = searcher.vector_search(&query, 3).await.unwrap();
        assert_eq!(results.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn hybrid_search_combines_both() {
        let dir = std::env::temp_dir().join("nabla-lance-search-hybrid");
        let _ = std::fs::remove_dir_all(&dir);

        let store = setup_store(&dir).await;
        let searcher = HybridSearcher::new(&store);

        let mut query = vec![0.1f32; 8];
        query[0] += 0.5;
        let norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        query.iter_mut().for_each(|x| *x /= norm);

        let results = searcher
            .hybrid_search("retrieval ranking fusion", &query, 5)
            .await
            .unwrap();
        assert!(!results.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
