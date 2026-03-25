use std::sync::Mutex;

use anyhow::Result;
use nabla_pdf_rag_contracts::*;
use nabla_pdf_rag_core::DocumentRepository;
use rusqlite::{params, Connection, Row};

/// Single-connection SQLite repository for desktop MVP.
/// Uses Mutex for Send+Sync. Upgrade to a connection pool if
/// multi-threaded pipelines cause contention.
pub struct SqliteRepository {
    conn: Mutex<Connection>,
}

impl SqliteRepository {
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("SQLite mutex poisoned")
    }

    fn query_all<T, F>(&self, sql: &str, params: &[&dyn rusqlite::types::ToSql], mapper: F) -> Result<Vec<T>>
    where
        F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
    {
        let conn = self.lock();
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map(params, mapper)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    pub fn insert_library(&self, lib: &LibraryRecord) -> Result<()> {
        self.lock().execute(
            "INSERT INTO libraries (id, name, root_dir, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![lib.id.as_str(), lib.name, lib.root_dir, lib.created_at],
        )?;
        Ok(())
    }

    pub fn insert_document(&self, doc: &DocumentRecord) -> Result<()> {
        self.lock().execute(
            "INSERT INTO documents (id, library_id, batch_id, file_name, source_path, checksum_sha256, page_count, title, authors, state, created_at, updated_at, error_message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                doc.id.as_str(),
                doc.library_id.as_str(),
                doc.batch_id.as_ref().map(|b| b.as_str()),
                doc.file_name,
                doc.source_path,
                doc.checksum_sha256,
                doc.page_count,
                doc.title,
                serde_json::to_string(&doc.authors)?,
                doc.state.to_string(),
                doc.created_at,
                doc.updated_at,
                doc.error_message,
            ],
        )?;
        Ok(())
    }

    pub fn update_document_state(
        &self,
        doc_id: &DocumentId,
        state: &DocumentState,
        error: Option<&str>,
    ) -> Result<()> {
        self.lock().execute(
            "UPDATE documents SET state = ?1, error_message = ?2, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?3",
            params![state.to_string(), error, doc_id.as_str()],
        )?;
        Ok(())
    }

    pub fn insert_chunk(&self, chunk: &ChunkRecord) -> Result<()> {
        self.lock().execute(
            "INSERT INTO chunks (id, document_id, summary_node_id, ordinal, heading_path, page_span_start, page_span_end, text, token_count, embedding_state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                chunk.id.as_str(),
                chunk.document_id.as_str(),
                chunk.summary_node_id.as_ref().map(|s| s.as_str()),
                chunk.ordinal,
                serde_json::to_string(&chunk.heading_path)?,
                chunk.page_span.as_ref().map(|p| p.start),
                chunk.page_span.as_ref().map(|p| p.end),
                chunk.text,
                chunk.token_count,
                chunk.embedding_state.to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn insert_summary_node(&self, node: &SummaryNode) -> Result<()> {
        self.lock().execute(
            "INSERT INTO summary_nodes (id, document_id, parent_id, kind, depth, ordinal, title, page_span_start, page_span_end, summary, child_ids, source_chunk_ids)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                node.id.as_str(),
                node.document_id.as_str(),
                node.parent_id.as_ref().map(|p| p.as_str()),
                node.kind.to_string(),
                node.depth,
                node.ordinal,
                node.title,
                node.page_span.as_ref().map(|p| p.start),
                node.page_span.as_ref().map(|p| p.end),
                node.summary,
                serde_json::to_string(&node.child_ids)?,
                serde_json::to_string(&node.source_chunk_ids)?,
            ],
        )?;
        Ok(())
    }
}

fn parse_page_span(row: &Row<'_>, start_col: usize, end_col: usize) -> rusqlite::Result<Option<PageSpan>> {
    let start: Option<u32> = row.get(start_col)?;
    let end: Option<u32> = row.get(end_col)?;
    Ok(start.zip(end).map(|(s, e)| PageSpan { start: s, end: e }))
}

fn parse_enum<T: std::str::FromStr>(row: &Row<'_>, col: usize) -> rusqlite::Result<T>
where
    T::Err: std::fmt::Display,
{
    let s: String = row.get(col)?;
    s.parse::<T>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
        )
    })
}

fn parse_json_vec<T: serde::de::DeserializeOwned>(row: &Row<'_>, col: usize) -> rusqlite::Result<Vec<T>> {
    let s: String = row.get(col)?;
    serde_json::from_str(&s).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            col,
            rusqlite::types::Type::Text,
            Box::new(e),
        )
    })
}

impl DocumentRepository for SqliteRepository {
    fn list_documents(&self, library_id: &LibraryId) -> Result<Vec<DocumentRecord>> {
        self.query_all(
            "SELECT id, library_id, batch_id, file_name, source_path, checksum_sha256, page_count, title, authors, state, created_at, updated_at, error_message
             FROM documents WHERE library_id = ?1",
            &[&library_id.as_str()],
            |row| {
                Ok(DocumentRecord {
                    id: DocumentId::new(row.get::<_, String>(0)?),
                    library_id: LibraryId::new(row.get::<_, String>(1)?),
                    batch_id: row.get::<_, Option<String>>(2)?.map(BatchId::new),
                    file_name: row.get(3)?,
                    source_path: row.get(4)?,
                    checksum_sha256: row.get(5)?,
                    page_count: row.get(6)?,
                    title: row.get(7)?,
                    authors: parse_json_vec(row, 8)?,
                    state: parse_enum(row, 9)?,
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
                    error_message: row.get(12)?,
                })
            },
        )
    }

    fn list_chunks(&self, document_id: &DocumentId) -> Result<Vec<ChunkRecord>> {
        self.query_all(
            "SELECT id, document_id, summary_node_id, ordinal, heading_path, page_span_start, page_span_end, text, token_count, embedding_state
             FROM chunks WHERE document_id = ?1 ORDER BY ordinal",
            &[&document_id.as_str()],
            |row| {
                Ok(ChunkRecord {
                    id: ChunkId::new(row.get::<_, String>(0)?),
                    document_id: DocumentId::new(row.get::<_, String>(1)?),
                    summary_node_id: row.get::<_, Option<String>>(2)?.map(SummaryNodeId::new),
                    ordinal: row.get(3)?,
                    heading_path: parse_json_vec(row, 4)?,
                    page_span: parse_page_span(row, 5, 6)?,
                    text: row.get(7)?,
                    token_count: row.get(8)?,
                    embedding_state: parse_enum(row, 9)?,
                })
            },
        )
    }

    fn list_summary_nodes(&self, document_id: &DocumentId) -> Result<Vec<SummaryNode>> {
        self.query_all(
            "SELECT id, document_id, parent_id, kind, depth, ordinal, title, page_span_start, page_span_end, summary, child_ids, source_chunk_ids
             FROM summary_nodes WHERE document_id = ?1 ORDER BY depth, ordinal",
            &[&document_id.as_str()],
            |row| {
                Ok(SummaryNode {
                    id: SummaryNodeId::new(row.get::<_, String>(0)?),
                    document_id: DocumentId::new(row.get::<_, String>(1)?),
                    parent_id: row.get::<_, Option<String>>(2)?.map(SummaryNodeId::new),
                    kind: parse_enum(row, 3)?,
                    depth: row.get::<_, u8>(4)?,
                    ordinal: row.get(5)?,
                    title: row.get(6)?,
                    page_span: parse_page_span(row, 7, 8)?,
                    summary: row.get(9)?,
                    child_ids: parse_json_vec(row, 10)?,
                    source_chunk_ids: parse_json_vec(row, 11)?,
                })
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_migrations;

    fn test_db() -> SqliteRepository {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        SqliteRepository::new(conn)
    }

    fn make_library() -> LibraryRecord {
        LibraryRecord {
            id: LibraryId::new("lib-1"),
            name: "Test Library".into(),
            root_dir: "/tmp/test".into(),
            created_at: "2025-01-01T00:00:00Z".into(),
        }
    }

    fn make_document(library_id: &LibraryId) -> DocumentRecord {
        DocumentRecord {
            id: DocumentId::new("doc-1"),
            library_id: library_id.clone(),
            batch_id: None,
            file_name: "test.pdf".into(),
            source_path: "/tmp/test/test.pdf".into(),
            checksum_sha256: "abc123".into(),
            page_count: Some(10),
            title: Some("Test Paper".into()),
            authors: vec!["Alice".into()],
            state: DocumentState::Queued,
            created_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
            error_message: None,
        }
    }

    #[test]
    fn roundtrip_library_and_document() {
        let repo = test_db();
        let lib = make_library();
        repo.insert_library(&lib).unwrap();

        let doc = make_document(&lib.id);
        repo.insert_document(&doc).unwrap();

        let docs = repo.list_documents(&lib.id).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].file_name, "test.pdf");
        assert_eq!(docs[0].authors, vec!["Alice"]);
    }

    #[test]
    fn roundtrip_chunks() {
        let repo = test_db();
        let lib = make_library();
        repo.insert_library(&lib).unwrap();
        let doc = make_document(&lib.id);
        repo.insert_document(&doc).unwrap();

        let chunk = ChunkRecord {
            id: ChunkId::new("chunk-1"),
            document_id: doc.id.clone(),
            summary_node_id: None,
            ordinal: 0,
            heading_path: vec!["Introduction".into()],
            page_span: Some(PageSpan { start: 1, end: 3 }),
            text: "This is the introduction.".into(),
            token_count: 5,
            embedding_state: EmbeddingState::Pending,
        };
        repo.insert_chunk(&chunk).unwrap();

        let chunks = repo.list_chunks(&doc.id).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading_path, vec!["Introduction"]);
        assert_eq!(chunks[0].page_span.as_ref().unwrap().start, 1);
    }

    #[test]
    fn roundtrip_summary_nodes() {
        let repo = test_db();
        let lib = make_library();
        repo.insert_library(&lib).unwrap();
        let doc = make_document(&lib.id);
        repo.insert_document(&doc).unwrap();

        let node = SummaryNode {
            id: SummaryNodeId::new("sn-1"),
            document_id: doc.id.clone(),
            parent_id: None,
            kind: SummaryNodeKind::Document,
            depth: 0,
            ordinal: 0,
            title: "Document Summary".into(),
            page_span: Some(PageSpan { start: 1, end: 10 }),
            summary: "This paper discusses...".into(),
            child_ids: vec![SummaryNodeId::new("sn-2")],
            source_chunk_ids: vec![ChunkId::new("chunk-1")],
        };
        repo.insert_summary_node(&node).unwrap();

        let nodes = repo.list_summary_nodes(&doc.id).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].kind, SummaryNodeKind::Document);
        assert_eq!(nodes[0].child_ids.len(), 1);
    }

    #[test]
    fn update_document_state_works() {
        let repo = test_db();
        let lib = make_library();
        repo.insert_library(&lib).unwrap();
        let doc = make_document(&lib.id);
        repo.insert_document(&doc).unwrap();

        repo.update_document_state(&doc.id, &DocumentState::Ready, None)
            .unwrap();

        let docs = repo.list_documents(&lib.id).unwrap();
        assert_eq!(docs[0].state, DocumentState::Ready);
    }

    #[test]
    fn duplicate_checksum_rejected() {
        let repo = test_db();
        let lib = make_library();
        repo.insert_library(&lib).unwrap();
        let doc = make_document(&lib.id);
        repo.insert_document(&doc).unwrap();

        let mut dup = make_document(&lib.id);
        dup.id = DocumentId::new("doc-2");
        assert!(repo.insert_document(&dup).is_err());
    }
}
