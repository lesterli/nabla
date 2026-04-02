#[cfg(feature = "native")]
mod native;
#[cfg(feature = "docling")]
mod docling_sidecar;
#[cfg(feature = "docling")]
mod auto;

#[cfg(feature = "native")]
pub use native::PdfExtractParser;
#[cfg(feature = "docling")]
pub use docling_sidecar::DoclingSidecarParser;
#[cfg(feature = "docling")]
pub use auto::AutoParser;
