/// LanceDB Rust SDK Spike
///
/// Validates the full retrieval chain:
///   write chunks → vector index → FTS index → hybrid query (vector + BM25 + RRF)
///
/// Run: cargo run --bin lancedb-spike
use std::sync::Arc;

use anyhow::Result;
use arrow_array::types::Float32Type;
use arrow_array::{Array, FixedSizeListArray, Int32Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lance_index::scalar::FullTextSearchQuery;
use lancedb::index::scalar::FtsIndexBuilder;
use lancedb::index::Index;
use lancedb::query::{ExecutableQuery, QueryBase, QueryExecutionOptions};
use lancedb::DistanceType;

const DIM: i32 = 8; // tiny dimension for spike
const DB_PATH: &str = "/tmp/nabla-lancedb-spike";

fn make_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("text", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                DIM,
            ),
            false,
        ),
    ]))
}

fn make_batch(
    schema: &Arc<Schema>,
    ids: &[i32],
    texts: &[&str],
    vecs: Vec<Vec<f32>>,
) -> Result<RecordBatch> {
    let vectors =
        FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
            vecs.into_iter().map(|v| Some(v.into_iter().map(Some).collect::<Vec<_>>())),
            DIM,
        );
    Ok(RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int32Array::from(ids.to_vec())),
            Arc::new(StringArray::from(texts.to_vec())),
            Arc::new(vectors),
        ],
    )?)
}

/// Generate a simple deterministic vector from text (for spike only).
fn fake_embed(text: &str) -> Vec<f32> {
    let mut v = vec![0.0_f32; DIM as usize];
    for (i, b) in text.bytes().enumerate() {
        v[i % DIM as usize] += b as f32 / 255.0;
    }
    // normalize
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.iter_mut().for_each(|x| *x /= norm);
    v
}

fn print_text_results(label: &str, batches: &[RecordBatch]) {
    println!("\n{label} results (top 3):");
    for batch in batches {
        let col = batch
            .column_by_name("text")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for i in 0..col.len() {
            println!("    - {}", col.value(i));
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Clean previous run
    let _ = std::fs::remove_dir_all(DB_PATH);

    println!("=== LanceDB Rust SDK Spike ===\n");

    // 1. Connect
    let db = lancedb::connect(DB_PATH).execute().await?;
    println!("[1] Connected to {DB_PATH}");

    // 2. Create table with sample chunks
    let schema = make_schema();
    let texts = vec![
        "RAPTOR builds hierarchical summaries over document chunks for multi-level retrieval",
        "BM25 is a probabilistic ranking function used in full-text search engines",
        "Reciprocal Rank Fusion combines results from multiple retrieval channels",
        "PDF parsing extracts text layout and reading order from document pages",
        "Vector embeddings capture semantic similarity between text passages",
        "LanceDB is an embedded vector database with native hybrid search support",
    ];

    let ids: Vec<i32> = (0..texts.len() as i32).collect();
    let vecs: Vec<Vec<f32>> = texts.iter().map(|t| fake_embed(t)).collect();
    let batch = make_batch(&schema, &ids, &texts, vecs)?;

    let table = db.create_table("chunks", batch).execute().await?;
    println!("[2] Created table 'chunks' with {} rows", texts.len());

    // 3. Vector search works without an explicit index (brute-force on small data).
    //    In production with >256 rows, create IVF_PQ:
    //      table.create_index(&["vector"], Index::IvfPq(IvfPqIndexBuilder::default()
    //          .distance_type(DistanceType::Cosine)
    //          .num_partitions(50).num_sub_vectors(16)))
    //          .execute().await?;
    println!("[3] Skipping vector index (brute-force OK for {} rows; IVF_PQ needs ≥256)", texts.len());

    // 4. Build FTS index
    table
        .create_index(&["text"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await?;
    println!("[4] FTS index (BM25) created");

    // 5. Vector-only search
    let query_vec = fake_embed("hierarchical document retrieval");
    let vector_results = table
        .vector_search(query_vec.as_slice())?
        .distance_type(DistanceType::Cosine)
        .limit(3)
        .execute()
        .await?
        .try_collect::<Vec<_>>()
        .await?;

    print_text_results("[5] Vector search", &vector_results);

    // 6. FTS-only search
    let fts_results = table
        .query()
        .full_text_search(FullTextSearchQuery::new("hybrid search".to_owned()))
        .limit(3)
        .execute()
        .await?
        .try_collect::<Vec<_>>()
        .await?;

    print_text_results("[6] FTS search", &fts_results);

    // 7. Hybrid search (vector + FTS + RRF)
    let hybrid_results = table
        .query()
        .full_text_search(FullTextSearchQuery::new("retrieval ranking".to_owned()))
        .nearest_to(query_vec.as_slice())?
        .limit(3)
        .execute_hybrid(QueryExecutionOptions::default())
        .await?
        .try_collect::<Vec<_>>()
        .await?;

    print_text_results("[7] Hybrid search (vector + BM25 + RRF)", &hybrid_results);

    // Cleanup
    let _ = std::fs::remove_dir_all(DB_PATH);
    println!("\n=== Spike complete. All capabilities verified. ===");

    Ok(())
}
