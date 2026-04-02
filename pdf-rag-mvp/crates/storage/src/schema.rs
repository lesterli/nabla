use anyhow::Result;
use rusqlite::Connection;

pub fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS libraries (
            id               TEXT PRIMARY KEY,
            name             TEXT NOT NULL,
            root_dir         TEXT NOT NULL,
            created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            prompt_template  TEXT  -- path to prompt template file for this library's scenario
        );

        CREATE TABLE IF NOT EXISTS import_batches (
            id              TEXT PRIMARY KEY,
            library_id      TEXT NOT NULL REFERENCES libraries(id),
            requested_paths TEXT NOT NULL DEFAULT '[]',   -- JSON array
            status          TEXT NOT NULL DEFAULT 'Pending',
            total_files     INTEGER NOT NULL DEFAULT 0,
            imported_files  INTEGER NOT NULL DEFAULT 0,
            failed_files    INTEGER NOT NULL DEFAULT 0,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
        );

        CREATE TABLE IF NOT EXISTS documents (
            id              TEXT PRIMARY KEY,
            library_id      TEXT NOT NULL REFERENCES libraries(id),
            batch_id        TEXT REFERENCES import_batches(id),
            file_name       TEXT NOT NULL,
            source_path     TEXT NOT NULL,
            checksum_sha256 TEXT NOT NULL,
            page_count      INTEGER,
            title           TEXT,
            authors         TEXT NOT NULL DEFAULT '[]',   -- JSON array
            state           TEXT NOT NULL DEFAULT 'Queued',
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            error_message   TEXT
        );

        CREATE TABLE IF NOT EXISTS summary_nodes (
            id               TEXT PRIMARY KEY,
            document_id      TEXT NOT NULL REFERENCES documents(id),
            parent_id        TEXT REFERENCES summary_nodes(id),
            kind             TEXT NOT NULL,
            depth            INTEGER NOT NULL DEFAULT 0,
            ordinal          INTEGER NOT NULL DEFAULT 0,
            title            TEXT NOT NULL DEFAULT '',
            page_span_start  INTEGER,
            page_span_end    INTEGER,
            summary          TEXT NOT NULL DEFAULT '',
            child_ids        TEXT NOT NULL DEFAULT '[]',  -- JSON array
            source_chunk_ids TEXT NOT NULL DEFAULT '[]'   -- JSON array
        );

        CREATE TABLE IF NOT EXISTS chunks (
            id               TEXT PRIMARY KEY,
            document_id      TEXT NOT NULL REFERENCES documents(id),
            summary_node_id  TEXT REFERENCES summary_nodes(id),
            ordinal          INTEGER NOT NULL DEFAULT 0,
            heading_path     TEXT NOT NULL DEFAULT '[]',  -- JSON array
            page_span_start  INTEGER,
            page_span_end    INTEGER,
            text             TEXT NOT NULL DEFAULT '',
            token_count      INTEGER NOT NULL DEFAULT 0,
            embedding_state  TEXT NOT NULL DEFAULT 'Pending'
        );

        CREATE INDEX IF NOT EXISTS idx_documents_library ON documents(library_id);
        CREATE INDEX IF NOT EXISTS idx_chunks_document ON chunks(document_id);
        CREATE INDEX IF NOT EXISTS idx_summary_nodes_document ON summary_nodes(document_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_documents_checksum ON documents(library_id, checksum_sha256);
        ",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        run_migrations(&conn).unwrap(); // second run should not fail
    }
}
