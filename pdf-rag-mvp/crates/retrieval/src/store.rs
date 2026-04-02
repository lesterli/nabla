use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::types::Float32Type;
use arrow_array::{FixedSizeListArray, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use lancedb::index::scalar::FtsIndexBuilder;
use lancedb::index::Index;
use lancedb::Connection;
use nabla_pdf_rag_core::EmbeddingRecord;

const TABLE_NAME: &str = "chunks";

/// A single item to be stored in LanceDB — embedding + metadata.
pub struct ChunkEmbedding<'a> {
    pub record: &'a EmbeddingRecord,
    pub document_id: &'a str,
    pub text: &'a str,
}

/// Manages the LanceDB table that stores chunk embeddings + text for retrieval.
///
/// Schema: { chunk_id: Utf8, document_id: Utf8, text: Utf8, vector: FixedSizeList<f32> }
pub struct LanceStore {
    db: Connection,
    dim: i32,
}

impl LanceStore {
    /// Open or create a LanceDB at the given directory path.
    pub async fn open(db_path: &str, dim: i32) -> Result<Self> {
        let db = lancedb::connect(db_path)
            .execute()
            .await
            .with_context(|| format!("Failed to open LanceDB at {db_path}"))?;
        Ok(Self { db, dim })
    }

    fn schema(&self) -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("chunk_id", DataType::Utf8, false),
            Field::new("document_id", DataType::Utf8, false),
            Field::new("text", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    self.dim,
                ),
                false,
            ),
        ]))
    }

    /// Insert chunk embeddings into LanceDB.
    /// Creates the table on first call; appends on subsequent calls.
    pub async fn upsert_embeddings(&self, items: &[ChunkEmbedding<'_>]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }

        let chunk_ids: Vec<&str> = items.iter().map(|i| i.record.chunk_id.as_str()).collect();
        let doc_ids: Vec<&str> = items.iter().map(|i| i.document_id).collect();
        let text_vals: Vec<&str> = items.iter().map(|i| i.text).collect();

        let vectors = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
            items.iter().map(|i| {
                let r = &i.record;
                Some(
                    r.vector
                        .iter()
                        .map(|v| Some(*v))
                        .collect::<Vec<_>>(),
                )
            }),
            self.dim,
        );

        let batch = RecordBatch::try_new(
            self.schema(),
            vec![
                Arc::new(StringArray::from(chunk_ids)),
                Arc::new(StringArray::from(doc_ids)),
                Arc::new(StringArray::from(text_vals)),
                Arc::new(vectors),
            ],
        )?;

        let tables = self.db.table_names().execute().await?;
        if tables.iter().any(|t| t == TABLE_NAME) {
            let table = self.db.open_table(TABLE_NAME).execute().await?;
            table.add(vec![batch]).execute().await?;
        } else {
            let table = self
                .db
                .create_table(TABLE_NAME, vec![batch])
                .execute()
                .await?;
            // Build FTS index on text column for BM25 search
            table
                .create_index(&["text"], Index::FTS(FtsIndexBuilder::default()))
                .execute()
                .await?;
        }

        Ok(())
    }

    /// Rebuild the FTS index after adding new data.
    /// LanceDB FTS needs explicit reindexing for newly added rows.
    pub async fn rebuild_fts_index(&self) -> Result<()> {
        let tables = self.db.table_names().execute().await?;
        if tables.iter().any(|t| t == TABLE_NAME) {
            let table = self.db.open_table(TABLE_NAME).execute().await?;
            table
                .create_index(&["text"], Index::FTS(FtsIndexBuilder::default()))
                .execute()
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn open_table(&self) -> Result<lancedb::Table> {
        self.db
            .open_table(TABLE_NAME)
            .execute()
            .await
            .context("Chunks table not found — import some PDFs first")
    }

    pub fn dim(&self) -> i32 {
        self.dim
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nabla_pdf_rag_contracts::ChunkId;

    fn fake_items(n: usize, dim: usize) -> (Vec<EmbeddingRecord>, Vec<String>) {
        let records: Vec<EmbeddingRecord> = (0..n)
            .map(|i| {
                let mut v = vec![0.0f32; dim];
                v[i % dim] = 1.0;
                EmbeddingRecord {
                    chunk_id: ChunkId::new(format!("c-{i}")),
                    vector: v,
                }
            })
            .collect();
        let texts: Vec<String> = (0..n)
            .map(|i| format!("This is chunk number {i} about topic {}", i % 3))
            .collect();
        (records, texts)
    }

    #[tokio::test]
    async fn store_and_count() {
        let dir = std::env::temp_dir().join("nabla-lance-store-test");
        let _ = std::fs::remove_dir_all(&dir);

        let store = LanceStore::open(dir.to_str().unwrap(), 8).await.unwrap();
        let (records, texts) = fake_items(5, 8);
        let items: Vec<ChunkEmbedding> = records
            .iter()
            .zip(texts.iter())
            .map(|(r, t)| ChunkEmbedding {
                record: r,
                document_id: "doc-1",
                text: t,
            })
            .collect();

        store.upsert_embeddings(&items).await.unwrap();

        let table = store.open_table().await.unwrap();
        let count = table.count_rows(None).await.unwrap();
        assert_eq!(count, 5);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
