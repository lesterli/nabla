use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use nabla_pdf_rag_contracts::DocumentRecord;
use nabla_pdf_rag_core::{
    DocElement, DocumentParser, ElementKind, PipelineStage, ProgressSink, ProgressUpdate,
    StructuredDocument,
};
use serde::{Deserialize, Serialize};

// ─── Sidecar JSON Protocol ──────────────────────────────────────────────

#[derive(Serialize)]
struct SidecarRequest {
    pdf_path: String,
    document_id: String,
}

#[derive(Deserialize)]
struct SidecarElement {
    kind: String,
    text: String,
    page_number: u32,
    level: Option<u8>,
}

#[derive(Deserialize)]
struct SidecarResponse {
    #[allow(dead_code)]
    document_id: String,
    title: Option<String>,
    page_count: u32,
    elements: Vec<SidecarElement>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct ReadySignal {
    status: String,
    message: Option<String>,
}

// ─── Managed Sidecar Process ─────────────────────────────────────────────

/// Manages a long-lived Python sidecar process running Docling.
///
/// The sidecar loads Docling models once on startup, then processes files
/// via a JSON-lines protocol over stdin/stdout. This avoids the 10-30s
/// cold start per file that a CLI approach would have.
struct SidecarProcess {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl SidecarProcess {
    fn spawn(sidecar_path: &PathBuf) -> Result<Self> {
        // Determine the project root (parent of scripts/)
        let project_root = sidecar_path
            .parent()
            .and_then(|p| p.parent())
            .unwrap_or(std::path::Path::new("."));

        // Prefer `uv run` if a pyproject.toml exists (venv-aware),
        // otherwise fall back to bare `python3`.
        let (cmd, args) = if project_root.join("pyproject.toml").exists() {
            ("uv", vec!["run", "--project"])
        } else {
            ("python3", vec![])
        };

        let mut command = Command::new(cmd);
        for arg in &args {
            command.arg(arg);
        }
        if cmd == "uv" {
            command.arg(project_root);
            command.arg("python3");
        }
        command.arg(sidecar_path);

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn Docling sidecar: {cmd} {}",
                    sidecar_path.display()
                )
            })?;

        let stdout = child.stdout.take().context("No stdout from sidecar")?;
        let mut reader = BufReader::new(stdout);

        // Wait for the ready signal
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .context("Failed to read sidecar ready signal")?;

        let signal: ReadySignal = serde_json::from_str(line.trim())
            .with_context(|| format!("Invalid ready signal: {line}"))?;

        if signal.status == "error" {
            let msg = signal.message.unwrap_or_else(|| "unknown error".into());
            // Kill the child if it's still running
            let _ = child.kill();
            bail!("Docling sidecar failed to start: {msg}");
        }

        if signal.status != "ready" {
            let _ = child.kill();
            bail!("Unexpected sidecar status: {}", signal.status);
        }

        Ok(Self { child, reader })
    }

    fn send_request(&mut self, request: &SidecarRequest) -> Result<SidecarResponse> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .context("Sidecar stdin closed")?;

        let mut json = serde_json::to_string(request)?;
        json.push('\n');
        stdin
            .write_all(json.as_bytes())
            .context("Failed to write to sidecar stdin")?;
        stdin.flush()?;

        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .context("Failed to read sidecar response")?;

        if line.trim().is_empty() {
            bail!("Sidecar returned empty response");
        }

        let resp: SidecarResponse = serde_json::from_str(line.trim())
            .with_context(|| format!("Failed to parse sidecar response: {line}"))?;

        Ok(resp)
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for SidecarProcess {
    fn drop(&mut self) {
        // Close stdin to signal the sidecar to exit gracefully
        let _ = self.child.stdin.take();
        // Give it a moment, then kill if needed
        match self.child.try_wait() {
            Ok(Some(_)) => {} // already exited
            _ => {
                let _ = self.child.kill();
            }
        }
    }
}

// ─── Public Parser ───────────────────────────────────────────────────────

/// PDF parser using a managed Docling sidecar process.
///
/// Spawns a Python process that loads Docling models once, then processes
/// files via stdin/stdout JSON protocol. The sidecar stays alive across
/// multiple parse calls.
pub struct DoclingSidecarParser {
    sidecar_path: PathBuf,
    process: Mutex<Option<SidecarProcess>>,
}

impl DoclingSidecarParser {
    pub fn new(sidecar_path: impl Into<PathBuf>) -> Self {
        Self {
            sidecar_path: sidecar_path.into(),
            process: Mutex::new(None),
        }
    }

    /// Locate the sidecar script relative to the executable.
    /// Searches: `scripts/docling_sidecar.py` from crate root or CWD.
    pub fn find_sidecar() -> Option<Self> {
        let candidates = [
            PathBuf::from("scripts/docling_sidecar.py"),
            PathBuf::from("../scripts/docling_sidecar.py"),
            PathBuf::from("../../scripts/docling_sidecar.py"),
            // Tauri app: navigate from src-tauri to project root
            PathBuf::from("../../../scripts/docling_sidecar.py"),
        ];

        for path in &candidates {
            if path.exists() {
                return Some(Self::new(path.canonicalize().unwrap_or(path.clone())));
            }
        }

        None
    }

    /// Check if Docling is available by trying to spawn the sidecar.
    pub fn is_available(&self) -> bool {
        if !self.sidecar_path.exists() {
            return false;
        }

        // Try to start the sidecar — if it succeeds, Docling is installed
        let mut guard = self.process.lock().unwrap();
        if guard.is_some() {
            return true;
        }

        match SidecarProcess::spawn(&self.sidecar_path) {
            Ok(proc) => {
                *guard = Some(proc);
                true
            }
            Err(_) => false,
        }
    }

    fn ensure_process(&self) -> Result<()> {
        let mut guard = self.process.lock().unwrap();

        // Check if process is alive
        let needs_restart = match guard.as_mut() {
            Some(proc) => !proc.is_alive(),
            None => true,
        };

        if needs_restart {
            *guard = Some(SidecarProcess::spawn(&self.sidecar_path)?);
        }

        Ok(())
    }
}

impl DocumentParser for DoclingSidecarParser {
    fn parse(
        &self,
        document: &DocumentRecord,
        progress: &dyn ProgressSink,
    ) -> Result<StructuredDocument> {
        progress.on_progress(&ProgressUpdate {
            stage: PipelineStage::Parse,
            current: 0,
            total: 1,
            message: Some(format!("Parsing {} with Docling", document.file_name)),
        });

        self.ensure_process()?;

        let request = SidecarRequest {
            pdf_path: document.source_path.clone(),
            document_id: document.id.to_string(),
        };

        let resp = {
            let mut guard = self.process.lock().unwrap();
            let proc = guard.as_mut().context("Sidecar process not running")?;
            proc.send_request(&request)?
        };

        if let Some(err) = resp.error {
            bail!("Docling sidecar error: {err}");
        }

        let elements: Vec<DocElement> = resp
            .elements
            .into_iter()
            .map(|e| DocElement {
                kind: label_to_kind(&e.kind, e.level),
                text: e.text,
                page_number: e.page_number,
            })
            .collect();

        progress.on_progress(&ProgressUpdate {
            stage: PipelineStage::Parse,
            current: 1,
            total: 1,
            message: Some(format!(
                "Docling: {} elements from {}",
                elements.len(),
                document.file_name
            )),
        });

        Ok(StructuredDocument {
            document_id: document.id.clone(),
            title: resp.title,
            page_count: resp.page_count,
            elements,
        })
    }
}

fn label_to_kind(label: &str, level: Option<u8>) -> ElementKind {
    match label {
        "title" => ElementKind::Title,
        "section_header" => ElementKind::SectionHeader {
            level: level.unwrap_or(1),
        },
        "paragraph" | "text" => ElementKind::Paragraph,
        "table" => ElementKind::Table,
        "list_item" => ElementKind::ListItem,
        "figure" | "picture" => ElementKind::Figure,
        "code" => ElementKind::Code,
        "equation" | "formula" => ElementKind::Equation,
        "page_header" => ElementKind::PageHeader,
        "page_footer" => ElementKind::PageFooter,
        _ => ElementKind::Paragraph,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nabla_pdf_rag_contracts::{DocumentId, DocumentState, LibraryId};
    use nabla_pdf_rag_core::NullProgress;

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
        // Write a mock sidecar that returns structured elements
        let dir = std::env::temp_dir().join("nabla-sidecar-test");
        let _ = std::fs::create_dir_all(&dir);
        let script = dir.join("mock_docling.py");
        std::fs::write(
            &script,
            r#"
import sys, json

# Signal ready
print(json.dumps({"status": "ready"}), flush=True)

# Process requests
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    resp = {
        "document_id": req["document_id"],
        "title": "Mock Title",
        "page_count": 2,
        "elements": [
            {"kind": "title", "text": "Mock Title", "page_number": 1, "level": None},
            {"kind": "section_header", "text": "Introduction", "page_number": 1, "level": 1},
            {"kind": "paragraph", "text": "Hello world", "page_number": 1, "level": None},
            {"kind": "table", "text": "| A | B |", "page_number": 2, "level": None},
        ],
        "error": None,
    }
    print(json.dumps(resp), flush=True)
"#,
        )
        .unwrap();

        let parser = DoclingSidecarParser::new(&script);
        let doc = make_test_document("/tmp/fake.pdf");

        match parser.parse(&doc, &NullProgress) {
            Ok(structured) => {
                assert_eq!(structured.title.as_deref(), Some("Mock Title"));
                assert_eq!(structured.page_count, 2);
                assert_eq!(structured.elements.len(), 4);
                assert_eq!(structured.elements[0].kind, ElementKind::Title);
                assert_eq!(
                    structured.elements[1].kind,
                    ElementKind::SectionHeader { level: 1 }
                );
                assert_eq!(structured.elements[2].kind, ElementKind::Paragraph);
                assert_eq!(structured.elements[3].kind, ElementKind::Table);
            }
            Err(e) => {
                if e.to_string().contains("Failed to spawn") {
                    eprintln!("Skipping sidecar test (python3 not available): {e}");
                } else {
                    panic!("Unexpected error: {e}");
                }
            }
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sidecar_handles_multiple_requests() {
        let dir = std::env::temp_dir().join("nabla-sidecar-multi");
        let _ = std::fs::create_dir_all(&dir);
        let script = dir.join("mock_multi.py");
        std::fs::write(
            &script,
            r#"
import sys, json
print(json.dumps({"status": "ready"}), flush=True)
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    resp = {
        "document_id": req["document_id"],
        "title": f"Doc {req['document_id']}",
        "page_count": 1,
        "elements": [{"kind": "paragraph", "text": "content", "page_number": 1, "level": None}],
        "error": None,
    }
    print(json.dumps(resp), flush=True)
"#,
        )
        .unwrap();

        let parser = DoclingSidecarParser::new(&script);

        // Send two requests to the same sidecar process
        let doc1 = make_test_document("/tmp/a.pdf");
        let doc2 = DocumentRecord {
            id: DocumentId::new("doc-2"),
            ..make_test_document("/tmp/b.pdf")
        };

        match parser.parse(&doc1, &NullProgress) {
            Ok(s) => assert_eq!(s.title.as_deref(), Some("Doc test-doc")),
            Err(e) if e.to_string().contains("Failed to spawn") => {
                eprintln!("Skipping (no python3): {e}");
                let _ = std::fs::remove_dir_all(dir);
                return;
            }
            Err(e) => panic!("Unexpected: {e}"),
        }

        match parser.parse(&doc2, &NullProgress) {
            Ok(s) => assert_eq!(s.title.as_deref(), Some("Doc doc-2")),
            Err(e) => panic!("Second request failed: {e}"),
        }

        let _ = std::fs::remove_dir_all(dir);
    }
}
