use crate::model::{
    BookFormat, BookId, BookPatch, BookView, BookmarkView, BulkAction, FolderNode, LibrarySnapshot,
    QueryState, ReaderContent, ReaderLocator, ReaderTocItem, TagSummary,
};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
use uuid::Uuid;

pub struct CoreBackend {
    library: Arc<Mutex<Option<ebook_core::Library>>>,
    active_resource_book: Arc<Mutex<Option<Uuid>>>,
    pdf_cache: Arc<ebook_core::reader::BoundedPdfCache<ebook_core::reader::PdfiumRenderer>>,
}

impl CoreBackend {
    pub fn shared() -> Arc<Self> {
        Arc::new(Self {
            library: Arc::new(Mutex::new(None)),
            active_resource_book: Arc::new(Mutex::new(None)),
            pdf_cache: Arc::new(
                ebook_core::reader::BoundedPdfCache::new(
                    ebook_core::reader::PdfiumRenderer::new(pdfium_library_directory()),
                    4,
                )
                .expect("positive PDF cache size"),
            ),
        })
    }

    pub fn app_backend() -> Arc<Self> {
        static INSTANCE: OnceLock<Arc<CoreBackend>> = OnceLock::new();
        INSTANCE.get_or_init(Self::shared).clone()
    }

    async fn load_library(&self, root: &Path, create: bool) -> Result<LibrarySnapshot> {
        let opened_root = root.to_path_buf();
        let state = self.library.clone();
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let canonical = std::fs::canonicalize(&opened_root).ok();
            if !create
                && slot
                    .as_ref()
                    .is_some_and(|library| canonical.as_deref() == Some(library.root()))
            {
                return core_snapshot(opened(&slot)?);
            }
            let library = if create {
                ebook_core::Library::create(&opened_root)?
            } else {
                ebook_core::Library::open_existing(&opened_root)?
            };
            let snapshot = core_snapshot(&library)?;
            // Keep the previous session and its lock intact if validation fails.
            *slot = Some(library);
            Ok::<_, anyhow::Error>(snapshot)
        })
        .await
        .context("书库任务意外结束")?
    }

    /// Only the book currently open in the reader may serve declared resources.
    pub fn reader_resource(&self, uri: &str) -> Result<ebook_core::reader::ReaderResource> {
        let slot = self.library.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
        let library = opened(&slot)?;
        if let Some(id) = uri.strip_prefix("ebook-resource://cover/") {
            let id = parse_id(id)?;
            let book = library.get_book(id)?;
            if book.cover_path.is_none() {
                anyhow::bail!("该书没有封面");
            }
            let expected = library.items_path().join(id.to_string()).join("cover.webp");
            if expected.symlink_metadata()?.file_type().is_symlink() {
                anyhow::bail!("封面路径不安全");
            }
            let bytes = std::fs::read(&expected)?;
            if bytes.len() as u64 > ebook_core::reader::MAX_RESOURCE_BYTES {
                anyhow::bail!("封面超过大小限制");
            }
            return Ok(ebook_core::reader::ReaderResource {
                media_type: "image/webp".into(),
                bytes,
            });
        }
        let resource = uri
            .strip_prefix("ebook-resource://book/")
            .ok_or_else(|| anyhow!("无效的电子书资源地址"))?;
        let id = self
            .active_resource_book
            .lock()
            .map_err(|_| anyhow!("阅读状态已损坏"))?
            .ok_or_else(|| anyhow!("没有正在阅读的图书"))?;
        let book = library.get_book(id)?;
        let path = library.managed_file_path(&book)?;
        ebook_core::reader::AdapterRegistry::default()
            .get(book.format)
            .resource(&path, resource)
            .map_err(Into::into)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ImportReport {
    pub imported: usize,
    pub duplicates: usize,
    pub failures: Vec<String>,
    pub txt_failures: Vec<(PathBuf, String)>,
}

impl ImportReport {
    pub fn summary(&self) -> String {
        let mut text = format!("已导入 {} 本", self.imported);
        if self.duplicates > 0 {
            text.push_str(&format!("，跳过 {} 个重复文件", self.duplicates));
        }
        if !self.failures.is_empty() {
            text.push_str(&format!("，{} 个失败", self.failures.len()));
        }
        text
    }
}

#[derive(Clone, Debug, Default)]
pub struct BulkRemoveReport {
    pub removed: usize,
    pub failures: Vec<(BookId, String)>,
}

#[async_trait]
pub trait DesktopBackend: Send + Sync {
    async fn open_library(&self, root: &Path) -> Result<LibrarySnapshot>;
    async fn create_library(&self, root: &Path) -> Result<LibrarySnapshot>;
    async fn close_library(&self) -> Result<()>;
    async fn refresh(&self) -> Result<LibrarySnapshot>;
    #[allow(dead_code)] // Stable facade operation; the current grid filters its cached snapshot.
    async fn query(&self, query: &QueryState) -> Result<Vec<BookView>>;
    async fn create_folder(&self, name: String, parent_id: Option<String>) -> Result<()>;
    async fn import_paths(&self, paths: Vec<PathBuf>) -> Result<ImportReport>;
    async fn import_txt_with_encoding(&self, path: PathBuf, encoding: String) -> Result<()>;
    async fn update_book(&self, id: &BookId, patch: BookPatch) -> Result<()>;
    async fn set_text_encoding(&self, id: &BookId, encoding: Option<String>) -> Result<()>;
    async fn set_tags(&self, id: &BookId, tags: Vec<String>) -> Result<()>;
    async fn set_category(&self, id: &BookId, folder_id: Option<String>) -> Result<()>;
    async fn replace_cover(&self, id: &BookId, cover: &Path) -> Result<()>;
    async fn reveal(&self, id: &BookId) -> Result<()>;
    async fn remove(&self, id: &BookId) -> Result<()>;
    async fn bulk_apply(&self, ids: Vec<BookId>, action: BulkAction) -> Result<usize>;
    async fn bulk_remove(&self, ids: Vec<BookId>) -> Result<BulkRemoveReport>;
    async fn list_bookmarks(&self, id: &BookId) -> Result<Vec<BookmarkView>>;
    async fn add_bookmark(&self, id: &BookId, label: String, locator: ReaderLocator) -> Result<()>;
    async fn rename_bookmark(&self, bookmark_id: &str, label: String) -> Result<()>;
    async fn delete_bookmark(&self, bookmark_id: &str) -> Result<()>;
    async fn open_reader(&self, id: &BookId, section: usize) -> Result<ReaderContent>;
    async fn save_progress(&self, id: &BookId, locator: ReaderLocator, finish: bool) -> Result<()>;
}

#[async_trait]
impl DesktopBackend for CoreBackend {
    async fn open_library(&self, root: &Path) -> Result<LibrarySnapshot> {
        self.load_library(root, false).await
    }

    async fn create_library(&self, root: &Path) -> Result<LibrarySnapshot> {
        self.load_library(root, true).await
    }

    async fn close_library(&self) -> Result<()> {
        let library = self.library.clone();
        let active_resource = self.active_resource_book.clone();
        let cache = self.pdf_cache.clone();
        tokio::task::spawn_blocking(move || {
            let mut slot = library.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let mut active = active_resource
                .lock()
                .map_err(|_| anyhow!("阅读状态已损坏"))?;
            *active = None;
            *slot = None;
            let _ = cache.clear();
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("关闭书库任务意外结束")?
    }

    async fn refresh(&self) -> Result<LibrarySnapshot> {
        let state = self.library.clone();
        tokio::task::spawn_blocking(move || {
            let slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            core_snapshot(opened(&slot)?)
        })
        .await
        .context("刷新书库任务意外结束")?
    }

    async fn query(&self, query: &QueryState) -> Result<Vec<BookView>> {
        let state = self.library.clone();
        let query = query.clone();
        tokio::task::spawn_blocking(move || {
            let slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let library = opened(&slot)?;
            let core_query = to_core_query(&query)?;
            let folders = library.list_folders()?;
            library
                .list_books(&core_query)?
                .into_iter()
                .map(|book| core_book_view(library, &folders, book))
                .collect()
        })
        .await
        .context("查询书库任务意外结束")?
    }

    async fn create_folder(&self, name: String, parent_id: Option<String>) -> Result<()> {
        let state = self.library.clone();
        let parent_id = parent_id.map(|value| parse_id(&value)).transpose()?;
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            opened_mut(&mut slot)?.create_folder(&name, parent_id)?;
            Ok(())
        })
        .await
        .context("新建分类任务意外结束")?
    }

    async fn import_paths(&self, paths: Vec<PathBuf>) -> Result<ImportReport> {
        let state = self.library.clone();
        let cache = self.pdf_cache.clone();
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let library = opened_mut(&mut slot)?;
            let report = library.import_paths(paths, &ebook_core::ImportOptions::default());
            for item in &report.items {
                if let ebook_core::ImportOutcome::Imported(id) = &item.outcome {
                    let book = library.get_book(*id)?;
                    if book.format == ebook_core::BookFormat::Pdf && book.cover_path.is_none() {
                        let path = library.managed_file_path(&book)?;
                        if let Ok(image) = cache.render_png(&path, 1, 0.5) {
                            // Rendering is best-effort: the PDF remains imported with a
                            // format placeholder if PDFium is unavailable.
                            let _ = library.set_custom_cover_bytes(*id, &image.bytes);
                        }
                    }
                }
            }
            Ok(ImportReport {
                imported: report.imported_count(),
                duplicates: report.duplicate_count(),
                txt_failures: report
                    .items
                    .iter()
                    .filter_map(|item| match &item.outcome {
                        ebook_core::ImportOutcome::Failed(error)
                            if item
                                .source
                                .extension()
                                .and_then(|value| value.to_str())
                                .is_some_and(|value| value.eq_ignore_ascii_case("txt")) =>
                        {
                            Some((item.source.clone(), error.to_string()))
                        }
                        _ => None,
                    })
                    .collect(),
                failures: report
                    .items
                    .into_iter()
                    .filter_map(|item| match item.outcome {
                        ebook_core::ImportOutcome::Failed(error) => {
                            Some(format!("{}：{error}", item.source.display()))
                        }
                        _ => None,
                    })
                    .collect(),
            })
        })
        .await
        .context("导入任务意外结束")?
    }

    async fn import_txt_with_encoding(&self, path: PathBuf, encoding: String) -> Result<()> {
        let state = self.library.clone();
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            opened_mut(&mut slot)?.import_one_with_encoding(
                &path,
                &ebook_core::ImportOptions::default(),
                Some(&encoding),
            )?;
            Ok(())
        })
        .await
        .context("以指定编码导入 TXT 的任务意外结束")?
    }

    async fn update_book(&self, id: &BookId, patch: BookPatch) -> Result<()> {
        let state = self.library.clone();
        let id = parse_id(id)?;
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let publication_date = optional_date(&patch.publication_date)?;
            let rating = patch.rating.map(ebook_core::Rating::new).transpose()?;
            opened_mut(&mut slot)?.update_book(
                id,
                ebook_core::BookPatch {
                    title: Some(patch.title),
                    authors: Some(patch.authors),
                    abstract_text: Some(nonempty(patch.abstract_text)),
                    publication_date: Some(publication_date),
                    edition: Some(nonempty(patch.edition)),
                    publisher: Some(nonempty(patch.publisher)),
                    favorite: Some(patch.favorite),
                    rating: Some(rating),
                    ..Default::default()
                },
            )?;
            let current = opened(&slot)?.get_book(id)?;
            opened_mut(&mut slot)?.update_reading(
                id,
                to_core_status(patch.status),
                current.progress,
                current.reader_locator.as_ref(),
            )?;
            Ok(())
        })
        .await
        .context("保存图书任务意外结束")?
    }

    async fn set_text_encoding(&self, id: &BookId, encoding: Option<String>) -> Result<()> {
        let state = self.library.clone();
        let id = parse_id(id)?;
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            opened_mut(&mut slot)?.set_text_encoding(id, encoding.as_deref())?;
            Ok(())
        })
        .await
        .context("修改 TXT 编码任务意外结束")?
    }

    async fn set_tags(&self, id: &BookId, tags: Vec<String>) -> Result<()> {
        let state = self.library.clone();
        let id = parse_id(id)?;
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            opened_mut(&mut slot)?.update_book(
                id,
                ebook_core::BookPatch {
                    tags: Some(tags),
                    ..Default::default()
                },
            )?;
            Ok(())
        })
        .await
        .context("保存标签任务意外结束")?
    }

    async fn set_category(&self, id: &BookId, folder_id: Option<String>) -> Result<()> {
        let state = self.library.clone();
        let id = parse_id(id)?;
        let folder_id = folder_id.map(|value| parse_id(&value)).transpose()?;
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            opened_mut(&mut slot)?.update_book(
                id,
                ebook_core::BookPatch {
                    folder_id: Some(folder_id),
                    ..Default::default()
                },
            )?;
            Ok(())
        })
        .await
        .context("保存分类任务意外结束")?
    }

    async fn replace_cover(&self, id: &BookId, cover: &Path) -> Result<()> {
        let state = self.library.clone();
        let id = parse_id(id)?;
        let cover = cover.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            opened_mut(&mut slot)?.set_custom_cover(id, &cover)?;
            Ok(())
        })
        .await
        .context("更换封面任务意外结束")?
    }

    async fn reveal(&self, id: &BookId) -> Result<()> {
        let state = self.library.clone();
        let id = parse_id(id)?;
        let path = tokio::task::spawn_blocking(move || {
            let slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let library = opened(&slot)?;
            let book = library.get_book(id)?;
            let path = library.managed_file_path(&book)?;
            Ok::<_, anyhow::Error>(path)
        })
        .await
        .context("定位文件任务意外结束")??;
        crate::platform::reveal_in_file_manager(&path)
    }

    async fn remove(&self, id: &BookId) -> Result<()> {
        let state = self.library.clone();
        let id = parse_id(id)?;
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let library = opened_mut(&mut slot)?;
            let plan = library.prepare_removal(id)?;
            crate::platform::move_to_trash(&plan.managed_directory)?;
            library.commit_removal(&plan)?;
            Ok(())
        })
        .await
        .context("移除图书任务意外结束")?
    }

    async fn bulk_apply(&self, ids: Vec<BookId>, action: BulkAction) -> Result<usize> {
        let ids = parse_unique_ids(ids)?;
        let action = match action {
            BulkAction::SetFolder(id) => {
                ebook_core::BulkBookAction::SetFolder(id.map(|id| parse_id(&id)).transpose()?)
            }
            BulkAction::AddTags(tags) => ebook_core::BulkBookAction::AddTags(tags),
            BulkAction::RemoveTags(tags) => ebook_core::BulkBookAction::RemoveTags(tags),
            BulkAction::SetFavorite(value) => ebook_core::BulkBookAction::SetFavorite(value),
            BulkAction::SetRating(value) => ebook_core::BulkBookAction::SetRating(
                value.map(ebook_core::Rating::new).transpose()?,
            ),
            BulkAction::SetReadingStatus(value) => {
                ebook_core::BulkBookAction::SetReadingStatus(to_core_status(value))
            }
        };
        let state = self.library.clone();
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            Ok::<_, anyhow::Error>(opened_mut(&mut slot)?.apply_bulk(&ids, action)?)
        })
        .await
        .context("批量修改任务意外结束")?
    }

    async fn bulk_remove(&self, ids: Vec<BookId>) -> Result<BulkRemoveReport> {
        let ids = parse_unique_ids(ids)?;
        let state = self.library.clone();
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let library = opened_mut(&mut slot)?;
            // Reject a stale selection before moving any managed directory.
            for id in &ids {
                library.get_book(*id)?;
            }
            let mut report = BulkRemoveReport::default();
            for id in ids {
                let result = (|| -> Result<()> {
                    let plan = library.prepare_removal(id)?;
                    crate::platform::move_to_trash(&plan.managed_directory)?;
                    library.commit_removal(&plan)?;
                    Ok(())
                })();
                match result {
                    Ok(()) => report.removed += 1,
                    Err(error) => report.failures.push((id.to_string(), error.to_string())),
                }
            }
            Ok::<_, anyhow::Error>(report)
        })
        .await
        .context("批量移除任务意外结束")?
    }

    async fn list_bookmarks(&self, id: &BookId) -> Result<Vec<BookmarkView>> {
        let state = self.library.clone();
        let id = parse_id(id)?;
        tokio::task::spawn_blocking(move || {
            let slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let library = opened(&slot)?;
            let book = library.get_book(id)?;
            let path = library.managed_file_path(&book)?;
            let options = library.reader_options(&book);
            let navigation = ebook_core::reader::AdapterRegistry::default()
                .get(book.format)
                .navigation_with_options(&path, &options)?;
            library
                .list_bookmarks(id)?
                .into_iter()
                .map(|bookmark| bookmark_view(&book, &navigation.sections, bookmark))
                .collect::<Result<Vec<_>>>()
        })
        .await
        .context("读取书签任务意外结束")?
    }

    async fn add_bookmark(&self, id: &BookId, label: String, locator: ReaderLocator) -> Result<()> {
        let state = self.library.clone();
        let id = parse_id(id)?;
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let library = opened_mut(&mut slot)?;
            let book = library.get_book(id)?;
            let core_locator = to_core_locator(library, &book, &locator)?;
            library.add_bookmark(id, &label, &core_locator)?;
            Ok(())
        })
        .await
        .context("添加书签任务意外结束")?
    }

    async fn rename_bookmark(&self, bookmark_id: &str, label: String) -> Result<()> {
        let state = self.library.clone();
        let id = parse_id(bookmark_id)?;
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            opened_mut(&mut slot)?.rename_bookmark(id, &label)?;
            Ok(())
        })
        .await
        .context("重命名书签任务意外结束")?
    }

    async fn delete_bookmark(&self, bookmark_id: &str) -> Result<()> {
        let state = self.library.clone();
        let id = parse_id(bookmark_id)?;
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            opened_mut(&mut slot)?.delete_bookmark(id)?;
            Ok(())
        })
        .await
        .context("删除书签任务意外结束")?
    }

    async fn open_reader(&self, id: &BookId, section: usize) -> Result<ReaderContent> {
        let state = self.library.clone();
        let active = self.active_resource_book.clone();
        let cache = self.pdf_cache.clone();
        let id = parse_id(id)?;
        tokio::task::spawn_blocking(move || {
            let slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let library = opened(&slot)?;
            let book = library.get_book(id)?;
            let path = library.managed_file_path(&book)?;
            let options = library.reader_options(&book);
            let registry = ebook_core::reader::AdapterRegistry::default();
            let adapter = registry.get(book.format);
            let navigation = adapter.navigation_with_options(&path, &options)?;
            let requested = if section == usize::MAX {
                book.reader_locator.clone().unwrap_or_else(|| {
                    navigation.sections.first().cloned().unwrap_or_else(|| {
                        ebook_core::ReaderLocator::new(book.format, serde_json::json!({}))
                    })
                })
            } else {
                navigation
                    .sections
                    .get(section)
                    .or_else(|| navigation.sections.first())
                    .cloned()
                    .unwrap_or_else(|| {
                        ebook_core::ReaderLocator::new(book.format, serde_json::json!({}))
                    })
            };
            let content = match adapter.read_with_options(&path, Some(&requested), &options)? {
                ebook_core::reader::ReaderContent::Html { body, .. } => ReaderContent::Html {
                    title: book.title,
                    body,
                    format: match book.format {
                        ebook_core::BookFormat::Epub => BookFormat::Epub,
                        ebook_core::BookFormat::Mobi => BookFormat::Mobi,
                        ebook_core::BookFormat::Txt => BookFormat::Txt,
                        ebook_core::BookFormat::Pdf => unreachable!("PDF does not return HTML"),
                    },
                    chapter_index: match book.format {
                        ebook_core::BookFormat::Mobi => 0,
                        ebook_core::BookFormat::Txt => requested
                            .location
                            .get("section")
                            .and_then(|value| value.as_u64())
                            .and_then(|value| usize::try_from(value).ok())
                            .unwrap_or(0),
                        _ => navigation
                            .sections
                            .iter()
                            .position(|entry| {
                                entry.location.get("resource") == requested.location.get("resource")
                            })
                            .unwrap_or(0),
                    },
                    chapter_count: navigation.sections.len().max(1),
                    toc: navigation
                        .toc
                        .iter()
                        .filter_map(|entry| {
                            let target = match book.format {
                                ebook_core::BookFormat::Mobi => Some(0),
                                ebook_core::BookFormat::Txt => entry
                                    .locator
                                    .location
                                    .get("section")
                                    .and_then(|value| value.as_u64())
                                    .and_then(|value| usize::try_from(value).ok()),
                                _ => navigation.sections.iter().position(|section| {
                                    section.location.get("resource")
                                        == entry.locator.location.get("resource")
                                }),
                            }?;
                            Some(ReaderTocItem {
                                label: entry.label.clone(),
                                depth: entry.depth,
                                section: target,
                                fragment: entry
                                    .locator
                                    .location
                                    .get("fragment")
                                    .and_then(|value| value.as_str())
                                    .map(str::to_owned),
                            })
                        })
                        .collect(),
                    scroll_fraction: requested
                        .location
                        .get("scroll_fraction")
                        .and_then(|value| value.as_f64())
                        .filter(|value| value.is_finite())
                        .unwrap_or(0.0)
                        .clamp(0.0, 1.0) as f32,
                },
                ebook_core::reader::ReaderContent::PdfPage { page, total_pages } => {
                    let image = cache.render_png(&path, page, 1.0)?;
                    ReaderContent::Pdf {
                        title: book.title,
                        page: page as usize,
                        page_count: total_pages as usize,
                        toc: navigation
                            .toc
                            .iter()
                            .filter_map(|entry| {
                                entry
                                    .locator
                                    .location
                                    .get("page")
                                    .and_then(|value| value.as_u64())
                                    .map(|page| ReaderTocItem {
                                        label: entry.label.clone(),
                                        depth: entry.depth,
                                        section: (page as usize).saturating_sub(1),
                                        fragment: None,
                                    })
                            })
                            .collect(),
                        image_width: image.width,
                        image_height: image.height,
                        rendered_page: Some(format!(
                            "data:{};base64,{}",
                            image.media_type,
                            STANDARD.encode(&image.bytes)
                        )),
                    }
                }
            };
            *active.lock().map_err(|_| anyhow!("阅读状态已损坏"))? = Some(id);
            Ok(content)
        })
        .await
        .context("打开阅读器任务意外结束")?
    }

    async fn save_progress(&self, id: &BookId, locator: ReaderLocator, finish: bool) -> Result<()> {
        let state = self.library.clone();
        let id = parse_id(id)?;
        tokio::task::spawn_blocking(move || {
            let mut slot = state.lock().map_err(|_| anyhow!("书库状态已损坏"))?;
            let library = opened_mut(&mut slot)?;
            let book = library.get_book(id)?;
            let status = if finish || book.reading_status == ebook_core::ReadingStatus::Finished {
                ebook_core::ReadingStatus::Finished
            } else {
                ebook_core::ReadingStatus::Reading
            };
            let core_locator = to_core_locator(library, &book, &locator)?;
            library.update_reading(
                id,
                status,
                locator.offset.clamp(0.0, 1.0) as f64,
                Some(&core_locator),
            )?;
            Ok(())
        })
        .await
        .context("保存阅读进度任务意外结束")?
    }
}

fn opened(slot: &Option<ebook_core::Library>) -> Result<&ebook_core::Library> {
    slot.as_ref().ok_or_else(|| anyhow!("请先打开书库"))
}

fn opened_mut(slot: &mut Option<ebook_core::Library>) -> Result<&mut ebook_core::Library> {
    slot.as_mut().ok_or_else(|| anyhow!("请先打开书库"))
}

fn parse_id(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).with_context(|| format!("无效的图书标识：{id}"))
}

fn parse_unique_ids(ids: Vec<BookId>) -> Result<Vec<Uuid>> {
    let mut seen = HashSet::new();
    let mut parsed = Vec::with_capacity(ids.len());
    for id in ids {
        let id = parse_id(&id)?;
        if seen.insert(id) {
            parsed.push(id);
        }
    }
    if parsed.is_empty() {
        anyhow::bail!("请先选择图书");
    }
    Ok(parsed)
}

fn to_core_locator(
    library: &ebook_core::Library,
    book: &ebook_core::Book,
    locator: &ReaderLocator,
) -> Result<ebook_core::ReaderLocator> {
    if locator.version != 1 || !locator.section_fraction.is_finite() {
        anyhow::bail!("无效的阅读位置");
    }
    let location = match book.format {
        ebook_core::BookFormat::Pdf => serde_json::json!({ "page": locator.section + 1 }),
        ebook_core::BookFormat::Epub => {
            let path = library.managed_file_path(book)?;
            let registry = ebook_core::reader::AdapterRegistry::default();
            let mut location = registry
                .get(book.format)
                .navigation(&path)?
                .sections
                .get(locator.section)
                .ok_or_else(|| anyhow!("EPUB 章节已不存在"))?
                .location
                .clone();
            location["scroll_fraction"] =
                serde_json::json!(locator.section_fraction.clamp(0.0, 1.0));
            location
        }
        ebook_core::BookFormat::Mobi => serde_json::json!({
            "section": 0,
            "scroll_fraction": locator.section_fraction.clamp(0.0, 1.0)
        }),
        ebook_core::BookFormat::Txt => {
            let path = library.managed_file_path(book)?;
            let options = library.reader_options(book);
            let registry = ebook_core::reader::AdapterRegistry::default();
            let mut location = registry
                .get(book.format)
                .navigation_with_options(&path, &options)?
                .sections
                .get(locator.section)
                .ok_or_else(|| anyhow!("TXT 章节已不存在"))?
                .location
                .clone();
            location["scroll_fraction"] =
                serde_json::json!(locator.section_fraction.clamp(0.0, 1.0));
            location
        }
    };
    Ok(ebook_core::ReaderLocator::new(book.format, location))
}

fn bookmark_view(
    book: &ebook_core::Book,
    sections: &[ebook_core::ReaderLocator],
    bookmark: ebook_core::Bookmark,
) -> Result<BookmarkView> {
    bookmark.locator.validate_for(book.format)?;
    let fraction = bookmark
        .locator
        .location
        .get("scroll_fraction")
        .and_then(|value| value.as_f64())
        .filter(|value| value.is_finite())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0) as f32;
    let section = match book.format {
        ebook_core::BookFormat::Pdf => bookmark
            .locator
            .location
            .get("page")
            .and_then(|value| value.as_u64())
            .and_then(|page| usize::try_from(page).ok())
            .and_then(|page| page.checked_sub(1))
            .ok_or_else(|| anyhow!("书签页码无效"))?,
        ebook_core::BookFormat::Epub => sections
            .iter()
            .position(|entry| {
                entry.location.get("resource") == bookmark.locator.location.get("resource")
            })
            .ok_or_else(|| anyhow!("书签章节已不存在"))?,
        ebook_core::BookFormat::Mobi => 0,
        ebook_core::BookFormat::Txt => bookmark
            .locator
            .location
            .get("section")
            .and_then(|value| value.as_u64())
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| anyhow!("TXT 书签章节无效"))?,
    };
    let position_label = if book.format == ebook_core::BookFormat::Pdf {
        format!("第 {} 页", section + 1)
    } else {
        format!("第 {} 章 · {}%", section + 1, (fraction * 100.0).round())
    };
    Ok(BookmarkView {
        id: bookmark.id.to_string(),
        label: bookmark.label,
        position_label,
        section,
        fraction,
    })
}

fn optional_date(value: &str) -> Result<Option<ebook_core::PartialDate>> {
    let value = value.trim();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ebook_core::PartialDate::parse(value)?))
    }
}

fn nonempty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn to_core_status(status: crate::model::ReadingStatus) -> ebook_core::ReadingStatus {
    match status {
        crate::model::ReadingStatus::Unread => ebook_core::ReadingStatus::Unread,
        crate::model::ReadingStatus::Reading => ebook_core::ReadingStatus::Reading,
        crate::model::ReadingStatus::Finished => ebook_core::ReadingStatus::Finished,
    }
}

fn from_core_status(status: ebook_core::ReadingStatus) -> crate::model::ReadingStatus {
    match status {
        ebook_core::ReadingStatus::Unread => crate::model::ReadingStatus::Unread,
        ebook_core::ReadingStatus::Reading => crate::model::ReadingStatus::Reading,
        ebook_core::ReadingStatus::Finished => crate::model::ReadingStatus::Finished,
    }
}

fn from_core_format(format: ebook_core::BookFormat) -> BookFormat {
    match format {
        ebook_core::BookFormat::Epub => BookFormat::Epub,
        ebook_core::BookFormat::Pdf => BookFormat::Pdf,
        ebook_core::BookFormat::Mobi => BookFormat::Mobi,
        ebook_core::BookFormat::Txt => BookFormat::Txt,
    }
}

#[allow(dead_code)]
fn to_core_format(format: BookFormat) -> ebook_core::BookFormat {
    match format {
        BookFormat::Epub => ebook_core::BookFormat::Epub,
        BookFormat::Pdf => ebook_core::BookFormat::Pdf,
        BookFormat::Mobi => ebook_core::BookFormat::Mobi,
        BookFormat::Txt => ebook_core::BookFormat::Txt,
    }
}

#[allow(dead_code)]
fn to_core_query(query: &QueryState) -> Result<ebook_core::BookQuery> {
    let (folder_id, tag, favorite, sidebar_status) = match &query.sidebar {
        crate::model::SidebarFilter::Folder(id) => (Some(parse_id(id)?), None, None, None),
        crate::model::SidebarFilter::Tag(tag) => (None, Some(tag.clone()), None, None),
        crate::model::SidebarFilter::Favorite => (None, None, Some(true), None),
        crate::model::SidebarFilter::Status(status) => {
            (None, None, None, Some(to_core_status(*status)))
        }
        crate::model::SidebarFilter::All => (None, None, None, None),
    };
    let (sort, direction) = match query.sort {
        crate::model::SortMode::AddedDesc => (
            ebook_core::BookSort::ImportedAt,
            ebook_core::SortDirection::Descending,
        ),
        crate::model::SortMode::TitleAsc => (
            ebook_core::BookSort::Title,
            ebook_core::SortDirection::Ascending,
        ),
        crate::model::SortMode::AuthorAsc => (
            ebook_core::BookSort::Author,
            ebook_core::SortDirection::Ascending,
        ),
        crate::model::SortMode::PublicationDesc => (
            ebook_core::BookSort::PublicationDate,
            ebook_core::SortDirection::Descending,
        ),
        crate::model::SortMode::RatingDesc => (
            ebook_core::BookSort::Rating,
            ebook_core::SortDirection::Descending,
        ),
        crate::model::SortMode::ProgressDesc => (
            ebook_core::BookSort::Progress,
            ebook_core::SortDirection::Descending,
        ),
    };
    Ok(ebook_core::BookQuery {
        search: nonempty(query.search.clone()),
        format: query.format.map(to_core_format),
        folder_id,
        tag,
        favorite,
        minimum_rating: query
            .minimum_rating
            .map(ebook_core::Rating::new)
            .transpose()?,
        reading_status: query.status.map(to_core_status).or(sidebar_status),
        sort,
        direction,
    })
}

fn core_snapshot(library: &ebook_core::Library) -> Result<LibrarySnapshot> {
    let folders = library.list_folders()?;
    let books = library.list_books(&ebook_core::BookQuery::default())?;
    let mut folder_counts = std::collections::HashMap::<Uuid, usize>::new();
    let mut tag_counts = std::collections::BTreeMap::<String, usize>::new();
    for book in &books {
        if let Some(folder_id) = book.folder_id {
            *folder_counts.entry(folder_id).or_default() += 1;
        }
        for tag in &book.tags {
            *tag_counts.entry(tag.clone()).or_default() += 1;
        }
    }
    let views = books
        .into_iter()
        .map(|book| core_book_view(library, &folders, book))
        .collect::<Result<Vec<_>>>()?;
    Ok(LibrarySnapshot {
        root: library.root().to_path_buf(),
        books: views,
        folders: folders
            .into_iter()
            .map(|folder| FolderNode {
                id: folder.id.to_string(),
                name: folder.name,
                parent_id: folder.parent_id.map(|id| id.to_string()),
                book_count: folder_counts.get(&folder.id).copied().unwrap_or(0),
            })
            .collect(),
        tags: tag_counts
            .into_iter()
            .map(|(name, book_count)| TagSummary {
                id: name.to_lowercase(),
                name,
                book_count,
            })
            .collect(),
    })
}

fn core_book_view(
    library: &ebook_core::Library,
    folders: &[ebook_core::Folder],
    book: ebook_core::Book,
) -> Result<BookView> {
    let folder_name = book.folder_id.and_then(|id| {
        folders
            .iter()
            .find(|folder| folder.id == id)
            .map(|folder| folder.name.clone())
    });
    let source_path = library.managed_file_path(&book)?;
    Ok(BookView {
        id: book.id.to_string(),
        title: book.title,
        authors: book.authors,
        abstract_text: book.abstract_text.unwrap_or_default(),
        publication_date: book
            .publication_date
            .map(|value| value.to_string())
            .unwrap_or_default(),
        edition: book.edition.unwrap_or_default(),
        publisher: book.publisher.unwrap_or_default(),
        format: from_core_format(book.format),
        text_encoding: book.text_encoding,
        favorite: book.favorite,
        rating: book.rating.map(|value| value.get()),
        status: from_core_status(book.reading_status),
        progress: book.progress as f32,
        folder_id: book.folder_id.map(|id| id.to_string()),
        folder_name,
        tags: book
            .tags
            .into_iter()
            .map(|name| TagSummary {
                id: name.to_lowercase(),
                name,
                book_count: 0,
            })
            .collect(),
        source_path,
        cover_path: book.cover_path.map(|path| library.root().join(path)),
        added_at: format!("{:020}", book.imported_at),
    })
}

fn pdfium_library_directory() -> Option<PathBuf> {
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/pdfium/lib");
    if development.join("libpdfium.dylib").is_file() {
        return Some(development);
    }
    let executable = std::env::current_exe().ok()?;
    let macos = executable.parent()?;
    let bundled = macos.join("../Resources/assets/pdfium/lib");
    bundled.join("libpdfium.dylib").is_file().then_some(bundled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ReadingStatus;
    use std::{fs::File, io::Write};
    use zip::{ZipWriter, write::SimpleFileOptions};

    fn write_test_epub(path: &Path) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        let options = SimpleFileOptions::default();
        zip.start_file("META-INF/container.xml", options).unwrap();
        zip.write_all(br#"<container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#).unwrap();
        zip.start_file("OPS/book.opf", options).unwrap();
        zip.write_all(br#"<package xmlns:dc="x"><metadata><dc:title>Smoke</dc:title></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="picture" href="picture.png" media-type="image/png"/></manifest><spine><itemref idref="chapter"/></spine></package>"#).unwrap();
        zip.start_file("OPS/chapter.xhtml", options).unwrap();
        zip.write_all(br#"<h1>Smoke chapter</h1><img src="picture.png"/>"#)
            .unwrap();
        zip.start_file("OPS/picture.png", options).unwrap();
        zip.write_all(b"placeholder image bytes").unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn epub_scroll_fraction_survives_reopening() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("Scroll.epub");
            write_test_epub(&source);
            let backend = CoreBackend::shared();
            backend
                .create_library(&temp.path().join("library"))
                .await
                .unwrap();
            assert_eq!(
                backend.import_paths(vec![source]).await.unwrap().imported,
                1
            );
            let id = backend.refresh().await.unwrap().books[0].id.clone();
            let ReaderContent::Html {
                scroll_fraction, ..
            } = backend.open_reader(&id, usize::MAX).await.unwrap()
            else {
                panic!("expected EPUB HTML");
            };
            assert_eq!(scroll_fraction, 0.0);
            backend
                .save_progress(
                    &id,
                    ReaderLocator {
                        version: 1,
                        section: 0,
                        offset: 0.37,
                        section_fraction: 0.37,
                    },
                    false,
                )
                .await
                .unwrap();
            let ReaderContent::Html {
                chapter_index,
                scroll_fraction,
                ..
            } = backend.open_reader(&id, usize::MAX).await.unwrap()
            else {
                panic!("expected EPUB HTML");
            };
            assert_eq!(chapter_index, 0);
            assert!((scroll_fraction - 0.37).abs() < 0.001);
            assert!((backend.refresh().await.unwrap().books[0].progress - 0.37).abs() < 0.001);
        });
    }

    #[cfg(target_os = "macos")]
    fn write_test_pdf(path: &Path) {
        use lopdf::{Bookmark, Document, Object, Stream, dictionary};
        let mut pdf = Document::with_version("1.5");
        let pages_id = pdf.new_object_id();
        let content_id = pdf.add_object(Stream::new(dictionary! {}, Vec::new()));
        let page_ids: Vec<_> = (0..3)
            .map(|_| {
                pdf.add_object(dictionary! {
                    "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
                    "MediaBox" => vec![0.into(), 0.into(), 400.into(), 500.into()],
                })
            })
            .collect();
        pdf.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Kids" => page_ids.iter().copied().map(Object::from).collect::<Vec<_>>(), "Count" => 3,
            }),
        );
        let catalog_id = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        pdf.trailer.set("Root", catalog_id);
        pdf.add_bookmark(
            Bookmark::new("Middle chapter".into(), [0.0; 3], 0, page_ids[1]),
            None,
        );
        let outline_id = pdf.build_outline().unwrap();
        pdf.objects
            .get_mut(&catalog_id)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Outlines", outline_id);
        pdf.save(path).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bundled_pdfium_renders_imported_page() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("Rendered.pdf");
            write_test_pdf(&source);
            let backend = CoreBackend::shared();
            backend
                .create_library(&temp.path().join("library"))
                .await
                .unwrap();
            assert_eq!(
                backend.import_paths(vec![source]).await.unwrap().imported,
                1
            );
            let book = backend.refresh().await.unwrap().books.remove(0);
            assert!(book.cover_path.as_ref().is_some_and(|path| path.exists()));
            let id = book.id;
            let ReaderContent::Pdf {
                page,
                page_count,
                toc,
                rendered_page: Some(data),
                ..
            } = backend.open_reader(&id, 0).await.unwrap()
            else {
                panic!("expected a rendered PDF page");
            };
            let png = STANDARD
                .decode(data.strip_prefix("data:image/png;base64,").unwrap())
                .unwrap();
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
            assert_eq!((page, page_count), (1, 3));
            assert_eq!(
                toc,
                vec![ReaderTocItem {
                    label: "Middle chapter".into(),
                    depth: 0,
                    section: 1,
                    fragment: None
                }]
            );
            assert!(matches!(
                backend.open_reader(&id, 1).await.unwrap(),
                ReaderContent::Pdf { page: 2, .. }
            ));
            let final_page = ReaderLocator {
                version: 1,
                section: 2,
                offset: 1.0,
                section_fraction: 0.0,
            };
            backend
                .save_progress(&id, final_page.clone(), false)
                .await
                .unwrap();
            assert_eq!(
                backend.refresh().await.unwrap().books[0].status,
                ReadingStatus::Reading
            );
            backend.save_progress(&id, final_page, true).await.unwrap();
            assert_eq!(
                backend.refresh().await.unwrap().books[0].status,
                ReadingStatus::Finished
            );
        });
    }

    #[test]
    fn core_backend_creates_imports_queries_and_updates() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("Smoke.epub");
            write_test_epub(&source);

            let backend = CoreBackend::shared();
            let library_root = temp.path().join("library");
            let empty = backend.create_library(&library_root).await.unwrap();
            assert!(empty.books.is_empty());

            let report = backend.import_paths(vec![source]).await.unwrap();
            assert_eq!(report.imported, 1);
            let books = backend
                .query(&QueryState {
                    search: "Smoke".into(),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(books.len(), 1);

            let id = books[0].id.clone();
            let ReaderContent::Html { body, .. } = backend.open_reader(&id, 0).await.unwrap()
            else {
                panic!("expected EPUB HTML content");
            };
            assert!(body.contains("ebook-resource://book/OPS%2Fpicture%2Epng"));
            let resource = backend
                .reader_resource("ebook-resource://book/OPS%2Fpicture%2Epng")
                .unwrap();
            assert_eq!(resource.bytes, b"placeholder image bytes");
            backend
                .update_book(
                    &id,
                    BookPatch {
                        title: "Smoke Test".into(),
                        favorite: true,
                        rating: Some(5),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let refreshed = backend.refresh().await.unwrap();
            assert_eq!(refreshed.books[0].title, "Smoke Test");
            assert!(refreshed.books[0].favorite);
        });
    }

    #[test]
    fn txt_import_reader_toc_bookmark_and_encoding_survive_reopen() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("两章.TXT");
            std::fs::write(
                &source,
                "第一章 开始\n你好 <script>alert(1)</script>\n第二章 继续\n再见 & 完。\n",
            )
            .unwrap();
            let root = temp.path().join("library");
            let backend = CoreBackend::shared();
            backend.create_library(&root).await.unwrap();
            let report = backend.import_paths(vec![source]).await.unwrap();
            assert_eq!(report.imported, 1);
            let book = backend.refresh().await.unwrap().books.remove(0);
            assert_eq!(book.format, BookFormat::Txt);
            assert_eq!(book.text_encoding.as_deref(), Some("utf-8"));
            let ReaderContent::Html {
                body,
                toc,
                chapter_count,
                ..
            } = backend.open_reader(&book.id, 0).await.unwrap()
            else {
                panic!("expected TXT HTML");
            };
            assert!(body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
            assert_eq!(toc.len(), 2);
            assert!(chapter_count >= 2);
            assert_eq!(toc[1].section, 1);
            let ReaderContent::Html {
                body,
                chapter_index,
                ..
            } = backend.open_reader(&book.id, 1).await.unwrap()
            else {
                panic!("expected second TXT section");
            };
            assert_eq!(chapter_index, 1);
            assert!(body.contains("第二章 继续"));

            let locator = ReaderLocator {
                version: 1,
                section: 1,
                offset: 0.75,
                section_fraction: 0.25,
            };
            backend
                .save_progress(&book.id, locator.clone(), false)
                .await
                .unwrap();
            backend
                .add_bookmark(&book.id, "继续阅读".into(), locator)
                .await
                .unwrap();
            backend
                .set_text_encoding(&book.id, Some("utf8".into()))
                .await
                .unwrap();
            backend.close_library().await.unwrap();
            backend.open_library(&root).await.unwrap();
            assert_eq!(
                backend.refresh().await.unwrap().books[0]
                    .text_encoding
                    .as_deref(),
                Some("utf-8")
            );
            let ReaderContent::Html {
                chapter_index,
                scroll_fraction,
                ..
            } = backend.open_reader(&book.id, usize::MAX).await.unwrap()
            else {
                panic!("expected restored TXT reading position");
            };
            assert_eq!(chapter_index, 1);
            assert!((scroll_fraction - 0.25).abs() < 0.001);
            let bookmarks = backend.list_bookmarks(&book.id).await.unwrap();
            assert_eq!(bookmarks.len(), 1);
            assert_eq!(bookmarks[0].section, 1);
        });
    }

    #[test]
    fn closing_library_releases_lock_and_allows_reopen() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("library");
            let backend = CoreBackend::shared();
            backend.create_library(&root).await.unwrap();
            backend.close_library().await.unwrap();
            assert!(backend.refresh().await.is_err());
            let other = CoreBackend::shared();
            other.open_library(&root).await.unwrap();
            other.close_library().await.unwrap();
            backend.open_library(&root).await.unwrap();
        });
    }

    #[test]
    fn desktop_facade_rejects_invalid_library_and_persists_bulk_and_bookmarks() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("Book.epub");
            write_test_epub(&source);
            let root = temp.path().join("library");
            let invalid = temp.path().join("ordinary");
            std::fs::create_dir(&invalid).unwrap();
            let backend = CoreBackend::shared();
            backend.create_library(&root).await.unwrap();
            assert!(backend.open_library(&invalid).await.is_err());
            assert!(!invalid.join("library.sqlite3").exists());
            assert!(backend.refresh().await.is_ok());

            backend.import_paths(vec![source]).await.unwrap();
            let id = backend.refresh().await.unwrap().books[0].id.clone();
            assert_eq!(
                backend
                    .bulk_apply(vec![id.clone()], BulkAction::SetFavorite(true))
                    .await
                    .unwrap(),
                1
            );
            assert_eq!(
                backend
                    .bulk_apply(vec![id.clone()], BulkAction::AddTags(vec!["检查".into()]))
                    .await
                    .unwrap(),
                1
            );
            let book = &backend.refresh().await.unwrap().books[0];
            assert!(book.favorite);
            assert!(book.tags.iter().any(|tag| tag.name == "检查"));

            let locator = ReaderLocator {
                version: 1,
                section: 0,
                offset: 0.4,
                section_fraction: 0.4,
            };
            backend
                .add_bookmark(&id, "中段".into(), locator)
                .await
                .unwrap();
            let bookmarks = backend.list_bookmarks(&id).await.unwrap();
            assert_eq!(bookmarks.len(), 1);
            assert_eq!(bookmarks[0].section, 0);
            assert!((bookmarks[0].fraction - 0.4).abs() < 0.001);
            backend
                .rename_bookmark(&bookmarks[0].id, "继续".into())
                .await
                .unwrap();
            backend.close_library().await.unwrap();
            backend.open_library(&root).await.unwrap();
            let bookmarks = backend.list_bookmarks(&id).await.unwrap();
            assert_eq!(bookmarks[0].label, "继续");
            backend.delete_bookmark(&bookmarks[0].id).await.unwrap();
            assert!(backend.list_bookmarks(&id).await.unwrap().is_empty());
        });
    }
}
