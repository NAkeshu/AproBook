use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{BookFormat, ExtractedMetadata, ReaderLocator, Result};

mod epub;
mod mobi;
mod pdf;
mod txt;

pub use epub::EpubAdapter;
pub use mobi::MobiAdapter;
pub use pdf::{
    BoundedPdfCache, PdfAdapter, PdfPageImage, PdfPageRenderer, PdfiumRenderer,
    UnavailablePdfRenderer,
};
pub use txt::{TXT_ENCODING_OPTIONS, TxtAdapter};

pub const MAX_EBOOK_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_RESOURCE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TocEntry {
    pub label: String,
    pub locator: ReaderLocator,
    pub depth: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReaderNavigation {
    /// Every sequentially readable section, independent of the book's authored TOC.
    pub sections: Vec<ReaderLocator>,
    /// Authored navigation labels, which may target fragments within a section.
    pub toc: Vec<TocEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReaderContent {
    Html {
        body: String,
        base_resource: Option<String>,
    },
    PdfPage {
        page: u32,
        total_pages: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderResource {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct ReaderOptions {
    /// Canonical encoding for a TXT book. None asks the adapter to detect it.
    pub text_encoding: Option<String>,
    /// Regenerable system cache directory; never inside a managed book item.
    pub cache_dir: Option<PathBuf>,
    /// Source content hash, used to isolate cache entries.
    pub sha256: Option<String>,
}

/// Format boundary used by the desktop reader. Adapters do not execute content.
pub trait ReaderAdapter: Send + Sync {
    fn format(&self) -> BookFormat;
    fn inspect(&self, path: &Path) -> Result<ExtractedMetadata>;
    fn inspect_with_options(
        &self,
        path: &Path,
        _options: &ReaderOptions,
    ) -> Result<ExtractedMetadata> {
        self.inspect(path)
    }
    fn navigation(&self, path: &Path) -> Result<ReaderNavigation>;
    fn read(&self, path: &Path, locator: Option<&ReaderLocator>) -> Result<ReaderContent>;
    fn navigation_with_options(
        &self,
        path: &Path,
        _options: &ReaderOptions,
    ) -> Result<ReaderNavigation> {
        self.navigation(path)
    }
    fn read_with_options(
        &self,
        path: &Path,
        locator: Option<&ReaderLocator>,
        _options: &ReaderOptions,
    ) -> Result<ReaderContent> {
        self.read(path, locator)
    }
    /// Loads a named, format-owned resource for a restricted custom protocol.
    /// Implementations must reject resources not declared by the ebook container.
    fn resource(&self, _path: &Path, resource: &str) -> Result<ReaderResource> {
        Err(crate::CoreError::Reader(format!(
            "format has no resource named {resource}"
        )))
    }
}

pub struct AdapterRegistry {
    adapters: BTreeMap<String, Box<dyn ReaderAdapter>>,
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        let mut registry = Self {
            adapters: BTreeMap::new(),
        };
        registry.register(EpubAdapter);
        registry.register(PdfAdapter);
        registry.register(MobiAdapter);
        registry.register(TxtAdapter);
        registry
    }
}

impl AdapterRegistry {
    pub fn register(&mut self, adapter: impl ReaderAdapter + 'static) {
        self.adapters
            .insert(adapter.format().to_string(), Box::new(adapter));
    }

    pub fn get(&self, format: BookFormat) -> &dyn ReaderAdapter {
        self.adapters
            .get(&format.to_string())
            .expect("all BookFormat variants have a registered adapter")
            .as_ref()
    }
}

pub(crate) fn fallback_title(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Untitled")
        .to_owned()
}
