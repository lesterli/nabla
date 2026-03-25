use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use nabla_pdf_rag_contracts::DocumentRecord;
use nabla_pdf_rag_core::{
    DocumentParser, ExtractedDocument, ExtractedPage, PipelineStage, ProgressSink, ProgressUpdate,
};
use serde::{Deserialize, Serialize};

/// Sidecar request sent to the Python process via stdin.
#[derive(Serialize)]
struct SidecarRequest {
    pdf_path: String,
    document_id: String,
}

/// A single page in the sidecar response.
#[derive(Deserialize)]
struct SidecarPage {
    page_number: u32,
    text: String,
}

/// Sidecar response received from the Python process via stdout.
#[derive(Deserialize)]
struct SidecarResponse {
    #[allow(dead_code)]
    document_id: String,
    inferred_title: Option<String>,
    pages: Vec<SidecarPage>,
    error: Option<String>,
}

/// PDF parser that delegates to a Python Docling sidecar process.
///
/// Protocol: send one JSON line to stdin, read one JSON line from stdout.
/// The sidecar script lives at `scripts/docling_sidecar.py`.
pub struct DoclingParser {
    sidecar_path: PathBuf,
}

impl DoclingParser {
    pub fn new(sidecar_path: impl Into<PathBuf>) -> Self {
        Self {
            sidecar_path: sidecar_path.into(),
        }
    }
}

impl DocumentParser for DoclingParser {
    fn extract_text(
        &self,
        document: &DocumentRecord,
        progress: &dyn ProgressSink,
    ) -> Result<ExtractedDocument> {
        progress.on_progress(&ProgressUpdate {
            stage: PipelineStage::Parse,
            current: 0,
            total: 1,
            message: Some(format!("Parsing {}", document.file_name)),
        });

        let mut child = Command::new("python3")
            .arg(&self.sidecar_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn sidecar: python3 {}",
                    self.sidecar_path.display()
                )
            })?;

        let request = SidecarRequest {
            pdf_path: document.source_path.clone(),
            document_id: document.id.to_string(),
        };

        // Write request to stdin
        let stdin = child.stdin.as_mut().context("Failed to open sidecar stdin")?;
        serde_json::to_writer(&mut *stdin, &request)?;
        writeln!(stdin)?;
        drop(child.stdin.take());

        // Read response from stdout
        let stdout = child.stdout.take().context("Failed to open sidecar stdout")?;
        let reader = BufReader::new(stdout);
        let mut response_line = String::new();

        for line in reader.lines() {
            let line = line?;
            if !line.trim().is_empty() {
                response_line = line;
            }
        }

        let status = child.wait()?;
        if !status.success() {
            let stderr = child.stderr.take();
            let err_msg = stderr
                .map(|s| {
                    BufReader::new(s)
                        .lines()
                        .map_while(Result::ok)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            bail!(
                "Sidecar exited with {}: {}",
                status,
                if err_msg.is_empty() { "no stderr" } else { &err_msg }
            );
        }

        if response_line.is_empty() {
            bail!("Sidecar returned no output");
        }

        let resp: SidecarResponse = serde_json::from_str(&response_line)
            .with_context(|| format!("Failed to parse sidecar response: {response_line}"))?;

        if let Some(err) = resp.error {
            bail!("Sidecar reported error: {err}");
        }

        progress.on_progress(&ProgressUpdate {
            stage: PipelineStage::Parse,
            current: 1,
            total: 1,
            message: Some(format!("Parsed {} pages", resp.pages.len())),
        });

        Ok(ExtractedDocument {
            document_id: document.id.clone(),
            inferred_title: resp.inferred_title,
            pages: resp
                .pages
                .into_iter()
                .map(|p| ExtractedPage {
                    page_number: p.page_number,
                    text: p.text,
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nabla_pdf_rag_contracts::{DocumentId, DocumentState, LibraryId};
    use nabla_pdf_rag_core::NullProgress;
    use std::io::Write;

    fn make_test_document(pdf_path: &str) -> DocumentRecord {
        DocumentRecord {
            id: DocumentId::new("test-doc"),
            library_id: LibraryId::new("test-lib"),
            batch_id: None,
            file_name: "test.pdf".into(),
            source_path: pdf_path.into(),
            checksum_sha256: "abc".into(),
            page_count: None,
            title: None,
            authors: vec![],
            state: DocumentState::Queued,
            created_at: String::new(),
            updated_at: String::new(),
            error_message: None,
        }
    }

    #[test]
    fn sidecar_protocol_roundtrip() {
        // Write a tiny mock sidecar that echoes back a fixed response
        let dir = std::env::temp_dir().join("nabla-parser-test");
        let _ = std::fs::create_dir_all(&dir);
        let script = dir.join("mock_sidecar.py");
        let mut f = std::fs::File::create(&script).unwrap();
        writeln!(
            f,
            r#"
import sys, json
req = json.loads(sys.stdin.readline())
resp = {{
    "document_id": req["document_id"],
    "inferred_title": "Mock Title",
    "pages": [
        {{"page_number": 1, "text": "Hello world"}},
        {{"page_number": 2, "text": "Second page"}}
    ],
    "error": None
}}
print(json.dumps(resp))
"#
        )
        .unwrap();

        let parser = DoclingParser::new(&script);
        let doc = make_test_document("/tmp/fake.pdf");
        let result = parser.extract_text(&doc, &NullProgress);

        match result {
            Ok(extracted) => {
                assert_eq!(extracted.pages.len(), 2);
                assert_eq!(extracted.pages[0].text, "Hello world");
                assert_eq!(extracted.inferred_title.as_deref(), Some("Mock Title"));
            }
            Err(e) => {
                // Python not available in CI — skip gracefully
                if e.to_string().contains("Failed to spawn") {
                    eprintln!("Skipping sidecar test (python3 not available): {e}");
                } else {
                    panic!("Unexpected error: {e}");
                }
            }
        }

        let _ = std::fs::remove_dir_all(dir);
    }
}
