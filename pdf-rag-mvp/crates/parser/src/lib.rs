#[cfg(feature = "native")]
mod native;
#[cfg(feature = "docling")]
mod sidecar;

#[cfg(feature = "native")]
pub use native::PdfExtractParser;
#[cfg(feature = "docling")]
pub use sidecar::DoclingParser;
