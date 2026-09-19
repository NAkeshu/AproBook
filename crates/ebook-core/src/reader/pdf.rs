use std::{
    collections::{HashMap, VecDeque},
    io::Cursor,
    path::{Path, PathBuf},
    sync::Mutex,
};

use image::ImageFormat;
use lopdf::{Document, Object};
use pdfium_render::prelude::{PdfRenderConfig, Pdfium};

use super::{
    MAX_EBOOK_BYTES, ReaderAdapter, ReaderContent, ReaderNavigation, TocEntry, fallback_title,
};
use crate::{BookFormat, CoreError, ExtractedMetadata, ReaderLocator, Result};

pub struct PdfAdapter;

impl ReaderAdapter for PdfAdapter {
    fn format(&self) -> BookFormat {
        BookFormat::Pdf
    }

    fn inspect(&self, path: &Path) -> Result<ExtractedMetadata> {
        ensure_size(path)?;
        let document = Document::load(path)?;
        if document.is_encrypted() {
            return Err(CoreError::Reader(
                "password-protected PDF files are not supported".into(),
            ));
        }
        let mut metadata = ExtractedMetadata::fallback(fallback_title(path));
        let mut raw = serde_json::Map::new();
        if let Ok(info_ref) = document.trailer.get(b"Info").and_then(Object::as_reference)
            && let Ok(info) = document.get_dictionary(info_ref)
        {
            for (key, destination) in [
                (b"Title".as_slice(), "title"),
                (b"Author".as_slice(), "author"),
                (b"Subject".as_slice(), "subject"),
                (b"Producer".as_slice(), "producer"),
            ] {
                if let Ok(value) = info.get(key).and_then(Object::as_str) {
                    let value = String::from_utf8_lossy(value).trim().to_owned();
                    if !value.is_empty() {
                        raw.insert(destination.into(), value.clone().into());
                    }
                }
            }
        }
        if let Some(value) = raw.get("title").and_then(|v| v.as_str()) {
            metadata.title = value.to_owned();
        }
        if let Some(value) = raw.get("author").and_then(|v| v.as_str()) {
            metadata.authors = vec![value.to_owned()];
        }
        if let Some(value) = raw.get("subject").and_then(|v| v.as_str()) {
            metadata.abstract_text = Some(value.to_owned());
        }
        raw.insert("page_count".into(), document.get_pages().len().into());
        metadata.raw = raw.into();
        Ok(metadata)
    }

    fn navigation(&self, path: &Path) -> Result<ReaderNavigation> {
        ensure_size(path)?;
        let document = Document::load(path)?;
        let pages = document.get_pages().len();
        let sections = (1..=pages)
            .map(|page| ReaderLocator::new(BookFormat::Pdf, serde_json::json!({ "page": page })))
            .collect();
        // A PDF without an outline still supports numeric page navigation.
        // Some malformed outline trees fail parsing; they must not block reading.
        let entries = document.get_toc().map(|toc| toc.toc).unwrap_or_default();
        let toc = entries
            .into_iter()
            .filter(|item| (1..=pages).contains(&item.page))
            .map(|item| TocEntry {
                label: item.title,
                locator: ReaderLocator::new(
                    BookFormat::Pdf,
                    serde_json::json!({ "page": item.page }),
                ),
                depth: item.level.saturating_sub(1).min(8) as u8,
            })
            .take(2_000)
            .collect();
        Ok(ReaderNavigation { sections, toc })
    }

    fn read(&self, path: &Path, locator: Option<&ReaderLocator>) -> Result<ReaderContent> {
        ensure_size(path)?;
        if let Some(locator) = locator {
            locator.validate_for(BookFormat::Pdf)?;
        }
        let pages = Document::load(path)?.get_pages().len() as u32;
        let page = locator
            .and_then(|value| value.location.get("page"))
            .and_then(|value| value.as_u64())
            .unwrap_or(1) as u32;
        if page == 0 || page > pages {
            return Err(CoreError::Reader(format!(
                "PDF page {page} is out of range"
            )));
        }
        Ok(ReaderContent::PdfPage {
            page,
            total_pages: pages,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfPageImage {
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

pub trait PdfPageRenderer: Send + Sync {
    fn render_png(&self, path: &Path, page: u32, scale: f32) -> Result<PdfPageImage>;
}

/// Runtime PDFium renderer. It searches an optional bundled library directory
/// first, then the operating system library search path.
pub struct PdfiumRenderer {
    library_directory: Option<PathBuf>,
}

impl PdfiumRenderer {
    pub fn new(library_directory: Option<PathBuf>) -> Self {
        Self { library_directory }
    }
}

impl PdfPageRenderer for PdfiumRenderer {
    fn render_png(&self, path: &Path, page: u32, scale: f32) -> Result<PdfPageImage> {
        ensure_size(path)?;
        if page == 0 {
            return Err(CoreError::Reader("PDF page numbers are one-based".into()));
        }
        let bindings = if let Some(directory) = &self.library_directory {
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(directory))
                .or_else(|_| Pdfium::bind_to_system_library())
        } else {
            Pdfium::bind_to_system_library()
        }
        .map_err(|error| {
            CoreError::PdfRendererUnavailable(format!("could not bind PDFium: {error}"))
        })?;
        let pdfium = Pdfium::new(bindings);
        let document = pdfium.load_pdf_from_file(path, None).map_err(|error| {
            CoreError::Reader(format!("PDFium could not open document: {error}"))
        })?;
        let index = u16::try_from(page - 1)
            .map_err(|_| CoreError::Reader("PDF page index exceeds PDFium range".into()))?;
        let page = document
            .pages()
            .get(index)
            .map_err(|error| CoreError::Reader(format!("PDF page is out of range: {error}")))?;
        let width = (1200.0 * scale.clamp(0.25, 4.0)).round() as i32;
        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new()
                    .set_target_width(width)
                    .set_maximum_height(8192),
            )
            .map_err(|error| CoreError::Reader(format!("PDF page render failed: {error}")))?;
        let image = bitmap.as_image();
        let dimensions = (image.width(), image.height());
        let mut cursor = Cursor::new(Vec::new());
        image.write_to(&mut cursor, ImageFormat::Png)?;
        Ok(PdfPageImage {
            media_type: "image/png".into(),
            width: dimensions.0,
            height: dimensions.1,
            bytes: cursor.into_inner(),
        })
    }
}

pub struct UnavailablePdfRenderer;
impl PdfPageRenderer for UnavailablePdfRenderer {
    fn render_png(&self, _path: &Path, _page: u32, _scale: f32) -> Result<PdfPageImage> {
        Err(CoreError::PdfRendererUnavailable(
            "install or bundle PDFium in the desktop application".into(),
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    path: PathBuf,
    page: u32,
    scale_bits: u32,
}
struct CacheState {
    values: HashMap<CacheKey, PdfPageImage>,
    order: VecDeque<CacheKey>,
}

/// Thread-safe least-recently-used cache whose entry count can never exceed the
/// configured capacity. The desktop layer can keep one instance per reader.
pub struct BoundedPdfCache<R> {
    renderer: R,
    capacity: usize,
    state: Mutex<CacheState>,
}

impl<R: PdfPageRenderer> BoundedPdfCache<R> {
    pub fn new(renderer: R, capacity: usize) -> Result<Self> {
        if capacity == 0 {
            return Err(CoreError::Validation(
                "PDF cache capacity must be positive".into(),
            ));
        }
        Ok(Self {
            renderer,
            capacity,
            state: Mutex::new(CacheState {
                values: HashMap::new(),
                order: VecDeque::new(),
            }),
        })
    }

    pub fn render_png(&self, path: &Path, page: u32, scale: f32) -> Result<PdfPageImage> {
        let key = CacheKey {
            path: path.to_path_buf(),
            page,
            scale_bits: scale.to_bits(),
        };
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| CoreError::Reader("PDF cache lock was poisoned".into()))?;
            if let Some(value) = state.values.get(&key).cloned() {
                touch(&mut state.order, &key);
                return Ok(value);
            }
        }
        let rendered = self.renderer.render_png(path, page, scale)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| CoreError::Reader("PDF cache lock was poisoned".into()))?;
        state.values.insert(key.clone(), rendered.clone());
        touch(&mut state.order, &key);
        while state.values.len() > self.capacity {
            if let Some(oldest) = state.order.pop_front() {
                state.values.remove(&oldest);
            }
        }
        Ok(rendered)
    }

    pub fn len(&self) -> Result<usize> {
        Ok(self
            .state
            .lock()
            .map_err(|_| CoreError::Reader("PDF cache lock was poisoned".into()))?
            .values
            .len())
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    pub fn clear(&self) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CoreError::Reader("PDF cache lock was poisoned".into()))?;
        state.values.clear();
        state.order.clear();
        Ok(())
    }
}

fn touch(order: &mut VecDeque<CacheKey>, key: &CacheKey) {
    if let Some(index) = order.iter().position(|candidate| candidate == key) {
        order.remove(index);
    }
    order.push_back(key.clone());
}

fn ensure_size(path: &Path) -> Result<()> {
    let size = path
        .metadata()
        .map_err(|error| crate::error::io(path, error))?
        .len();
    if size > MAX_EBOOK_BYTES {
        Err(CoreError::Reader(format!(
            "PDF exceeds {MAX_EBOOK_BYTES} byte limit"
        )))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::tempdir;

    struct FakeRenderer(AtomicUsize);
    impl PdfPageRenderer for FakeRenderer {
        fn render_png(&self, _path: &Path, page: u32, _scale: f32) -> Result<PdfPageImage> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(PdfPageImage {
                media_type: "image/png".into(),
                width: page,
                height: 1,
                bytes: vec![page as u8],
            })
        }
    }

    #[test]
    fn cache_is_bounded_and_reuses_entries() {
        let cache = BoundedPdfCache::new(FakeRenderer(AtomicUsize::new(0)), 2).unwrap();
        cache.render_png(Path::new("a.pdf"), 1, 1.0).unwrap();
        cache.render_png(Path::new("a.pdf"), 1, 1.0).unwrap();
        cache.render_png(Path::new("a.pdf"), 2, 1.0).unwrap();
        cache.render_png(Path::new("a.pdf"), 3, 1.0).unwrap();
        assert_eq!(cache.len().unwrap(), 2);
        assert_eq!(cache.renderer.0.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn navigation_includes_every_page_without_an_outline() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("pages.pdf");
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let page_ids = (0..2)
            .map(|_| {
                document.add_object(dictionary! {
                    "Type" => "Page",
                    "Parent" => pages_id,
                    "MediaBox" => vec![0.into(), 0.into(), 200.into(), 300.into()],
                })
            })
            .collect::<Vec<_>>();
        document.objects.insert(pages_id, dictionary! {
            "Type" => "Pages", "Kids" => page_ids.iter().map(|id| Object::Reference(*id)).collect::<Vec<_>>(),
            "Count" => 2,
        }.into());
        let catalog_id =
            document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        document.trailer.set("Root", catalog_id);
        document.save(&path).unwrap();
        let navigation = PdfAdapter.navigation(&path).unwrap();
        assert_eq!(navigation.sections.len(), 2);
        assert!(navigation.toc.is_empty());
        assert_eq!(navigation.sections[0].location["page"], 1);
        assert_eq!(navigation.sections[1].location["page"], 2);
    }
}
