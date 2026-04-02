use anyhow::Result;
use nabla_pdf_rag_contracts::DocumentRecord;
use nabla_pdf_rag_core::{DocumentParser, ProgressSink, StructuredDocument};

use crate::docling_sidecar::DoclingSidecarParser;
use crate::native::PdfExtractParser;

/// Auto-selecting parser: tries Docling sidecar first, falls back to pdf-extract.
///
/// On construction, checks if the Docling sidecar script exists and if
/// Python + Docling are available. If so, spawns a long-lived Python
/// process for structure-aware parsing. Otherwise falls back to
/// PdfExtractParser (all-Paragraph output).
pub struct AutoParser {
    docling: Option<DoclingSidecarParser>,
}

impl AutoParser {
    /// Create by auto-discovering the sidecar script and probing availability.
    pub fn new() -> Self {
        let docling = DoclingSidecarParser::find_sidecar().filter(|p| p.is_available());
        Self { docling }
    }

    /// Create with an explicit sidecar script path.
    pub fn with_sidecar_path(path: impl Into<std::path::PathBuf>) -> Self {
        let parser = DoclingSidecarParser::new(path);
        if parser.is_available() {
            Self {
                docling: Some(parser),
            }
        } else {
            Self { docling: None }
        }
    }

    /// Force native-only mode (no Docling attempt).
    pub fn native_only() -> Self {
        Self { docling: None }
    }

    pub fn is_docling_active(&self) -> bool {
        self.docling.is_some()
    }
}

impl Default for AutoParser {
    fn default() -> Self {
        Self::new()
    }
}

impl DocumentParser for AutoParser {
    fn parse(
        &self,
        document: &DocumentRecord,
        progress: &dyn ProgressSink,
    ) -> Result<StructuredDocument> {
        if let Some(docling) = &self.docling {
            match docling.parse(document, progress) {
                Ok(doc) => return Ok(doc),
                Err(e) => {
                    eprintln!(
                        "Docling failed for {}, falling back to native: {e}",
                        document.file_name
                    );
                }
            }
        }

        PdfExtractParser.parse(document, progress)
    }
}
