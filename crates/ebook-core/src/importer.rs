use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use image::ImageFormat;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::{
    Book, BookFormat, CoreError, ImportOptions, ReadingStatus, Result,
    database::{NewBook, now},
    reader::{AdapterRegistry, MAX_EBOOK_BYTES},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    Imported(Uuid),
    Duplicate(Uuid),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportItemResult {
    pub source: PathBuf,
    pub outcome: ImportOutcome,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub items: Vec<ImportItemResult>,
}

impl ImportReport {
    pub fn imported_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item.outcome, ImportOutcome::Imported(_)))
            .count()
    }
    pub fn duplicate_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item.outcome, ImportOutcome::Duplicate(_)))
            .count()
    }
    pub fn failed_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| matches!(item.outcome, ImportOutcome::Failed(_)))
            .count()
    }
}

impl crate::Library {
    /// Imports files and recursively scans directory inputs. A bad item never stops
    /// the remainder of the batch.
    pub fn import_paths<I, P>(&mut self, inputs: I, options: &ImportOptions) -> ImportReport
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut files = Vec::new();
        let mut report = ImportReport::default();
        for input in inputs {
            let input = input.as_ref();
            if input.is_dir() {
                for entry in WalkDir::new(input).follow_links(false) {
                    match entry {
                        Ok(entry) if entry.file_type().is_file() && supported(entry.path()) => {
                            files.push(entry.into_path())
                        }
                        Ok(_) => {}
                        Err(error) => report.items.push(ImportItemResult {
                            source: input.to_path_buf(),
                            outcome: ImportOutcome::Failed(error.to_string()),
                        }),
                    }
                }
            } else {
                files.push(input.to_path_buf());
            }
        }
        files.sort();
        files.dedup();
        for source in files {
            let outcome = match self.import_one(&source, options) {
                Ok(id) => ImportOutcome::Imported(id),
                Err(CoreError::Duplicate { existing_book_id }) => {
                    match Uuid::parse_str(&existing_book_id) {
                        Ok(id) => ImportOutcome::Duplicate(id),
                        Err(_) => ImportOutcome::Failed(format!(
                            "invalid duplicate id: {existing_book_id}"
                        )),
                    }
                }
                Err(error) => ImportOutcome::Failed(error.to_string()),
            };
            report.items.push(ImportItemResult { source, outcome });
        }
        report
    }

    pub fn import_one(&mut self, source: &Path, options: &ImportOptions) -> Result<Uuid> {
        self.import_one_with_encoding(source, options, None)
    }

    /// Retries a TXT import with an explicitly chosen encoding. The original
    /// source remains untouched, and a failed decode leaves no managed copy.
    pub fn import_one_with_encoding(
        &mut self,
        source: &Path,
        options: &ImportOptions,
        encoding: Option<&str>,
    ) -> Result<Uuid> {
        if !source.is_file() {
            return Err(CoreError::Validation(format!(
                "import source is not a file: {}",
                source.display()
            )));
        }
        let extension = source
            .extension()
            .and_then(|value| value.to_str())
            .ok_or_else(|| CoreError::UnsupportedFormat(source.display().to_string()))?;
        let format = BookFormat::from_extension(extension)?;
        if encoding.is_some() && format != BookFormat::Txt {
            return Err(CoreError::Validation(
                "encoding override is only supported for TXT".into(),
            ));
        }
        if let Some(folder) = options.folder_id {
            if !self.list_folders()?.iter().any(|item| item.id == folder) {
                return Err(CoreError::Validation(format!("folder not found: {folder}")));
            }
        }
        let registry = AdapterRegistry::default();
        let book_id = Uuid::new_v4();
        let temp_dir = self.items_path().join(format!(".import-{book_id}"));
        let final_dir = self.items_path().join(book_id.to_string());
        fs::create_dir(&temp_dir).map_err(|error| crate::error::io(&temp_dir, error))?;
        // Parse under the original basename so formats without embedded titles
        // fall back to the user's filename, not the managed name `source`.
        let temp_source = temp_dir.join(
            source
                .file_name()
                .ok_or_else(|| CoreError::Validation("import source has no filename".into()))?,
        );
        let managed_source = temp_dir.join(format!("source.{}", format.extension()));
        let result = (|| -> Result<Book> {
            copy_synced(source, &temp_source)?;
            let sha256 = hash_file(&temp_source)?;
            if let Some(existing) = self.find_by_sha256(&sha256)? {
                return Err(CoreError::Duplicate {
                    existing_book_id: existing.to_string(),
                });
            }
            let metadata = registry.get(format).inspect_with_options(
                &temp_source,
                &crate::reader::ReaderOptions {
                    text_encoding: encoding.map(str::to_owned),
                    ..Default::default()
                },
            )?;
            let cover_path = if let Some(cover) = metadata.cover.as_deref() {
                let cover_file = temp_dir.join("cover.webp");
                normalize_cover(cover, &cover_file)?;
                Some(format!("items/{book_id}/cover.webp"))
            } else {
                None
            };
            if temp_source != managed_source {
                fs::rename(&temp_source, &managed_source)
                    .map_err(|error| crate::error::io(&managed_source, error))?;
            }
            fs::rename(&temp_dir, &final_dir)
                .map_err(|error| crate::error::io(&final_dir, error))?;
            let timestamp = now();
            Ok(Book {
                id: book_id,
                title: metadata.title,
                authors: metadata.authors,
                abstract_text: metadata.abstract_text,
                publication_date: metadata.publication_date,
                edition: metadata.edition,
                publisher: metadata.publisher,
                format,
                text_encoding: metadata.text_encoding,
                tags: options.tags.clone(),
                folder_id: options.folder_id,
                favorite: false,
                rating: None,
                reading_status: ReadingStatus::Unread,
                progress: 0.0,
                reader_locator: None,
                original_path: absolute_source(source)?.to_string_lossy().to_string(),
                managed_path: format!("items/{book_id}/source.{}", format.extension()),
                cover_path,
                sha256,
                raw_metadata: metadata.raw,
                imported_at: timestamp,
                updated_at: timestamp,
            })
        })();
        let book = match result {
            Ok(book) => book,
            Err(error) => {
                let _ = fs::remove_dir_all(&temp_dir);
                let _ = fs::remove_dir_all(&final_dir);
                return Err(error);
            }
        };
        if let Err(error) = self.insert_imported(NewBook { book }) {
            let _ = fs::remove_dir_all(&final_dir);
            return Err(error);
        }
        Ok(book_id)
    }

    /// Replaces a cover with a normalized managed WebP asset.
    pub fn set_custom_cover(&mut self, book_id: Uuid, image_path: &Path) -> Result<PathBuf> {
        let bytes = fs::read(image_path).map_err(|error| crate::error::io(image_path, error))?;
        self.set_custom_cover_bytes(book_id, &bytes)
    }

    /// Stores a decoded or rendered cover without an intermediate source file.
    pub fn set_custom_cover_bytes(&mut self, book_id: Uuid, bytes: &[u8]) -> Result<PathBuf> {
        let book = self.get_book(book_id)?;
        let managed = self.managed_file_path(&book)?;
        let directory = managed
            .parent()
            .ok_or_else(|| CoreError::Validation("managed file has no directory".into()))?;
        let temp = directory.join("cover.webp.tmp");
        let destination = directory.join("cover.webp");
        normalize_cover(bytes, &temp)?;
        fs::rename(&temp, &destination).map_err(|error| crate::error::io(&destination, error))?;
        let relative = format!("items/{book_id}/cover.webp");
        self.connection.execute(
            "UPDATE books SET cover_path=?2,updated_at=?3 WHERE id=?1",
            rusqlite::params![book_id.to_string(), relative, now()],
        )?;
        Ok(destination)
    }

    pub fn cleanup_interrupted_imports(&self) -> Result<usize> {
        let mut removed = 0;
        for entry in fs::read_dir(self.items_path())
            .map_err(|error| crate::error::io(self.items_path(), error))?
        {
            let entry = entry.map_err(|error| crate::error::io(self.items_path(), error))?;
            if entry
                .file_type()
                .map_err(|error| crate::error::io(entry.path(), error))?
                .is_dir()
                && entry.file_name().to_string_lossy().starts_with(".import-")
            {
                fs::remove_dir_all(entry.path())
                    .map_err(|error| crate::error::io(entry.path(), error))?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| BookFormat::from_extension(extension).is_ok())
}

fn hash_file(path: &Path) -> Result<String> {
    let file = File::open(path).map_err(|error| crate::error::io(path, error))?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| crate::error::io(path, error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn copy_synced(source: &Path, destination: &Path) -> Result<()> {
    let mut input = File::open(source).map_err(|error| crate::error::io(source, error))?;
    let output = File::create(destination).map_err(|error| crate::error::io(destination, error))?;
    let mut output = BufWriter::new(output);
    let mut limited = (&mut input).take(MAX_EBOOK_BYTES + 1);
    let copied = std::io::copy(&mut limited, &mut output)
        .map_err(|error| crate::error::io(destination, error))?;
    if copied > MAX_EBOOK_BYTES {
        return Err(CoreError::Validation(format!(
            "ebook exceeds {MAX_EBOOK_BYTES} byte import limit"
        )));
    }
    output
        .flush()
        .map_err(|error| crate::error::io(destination, error))?;
    output
        .get_ref()
        .sync_all()
        .map_err(|error| crate::error::io(destination, error))?;
    Ok(())
}

fn normalize_cover(bytes: &[u8], destination: &Path) -> Result<()> {
    let image = image::load_from_memory(bytes)?;
    let image = image.thumbnail(1200, 1800);
    let output = File::create(destination).map_err(|error| crate::error::io(destination, error))?;
    let mut output = BufWriter::new(output);
    image.write_to(&mut output, ImageFormat::WebP)?;
    output
        .flush()
        .map_err(|error| crate::error::io(destination, error))?;
    Ok(())
}

fn absolute_source(path: &Path) -> Result<PathBuf> {
    fs::canonicalize(path).map_err(|error| crate::error::io(path, error))
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};
    use tempfile::tempdir;
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;
    use crate::{
        BookPatch, BookQuery, BookSort, BulkBookAction, ImportOptions, Rating, ReaderLocator,
        SortDirection,
    };

    fn epub(path: &Path, title: &str) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        let options = SimpleFileOptions::default();
        zip.start_file("META-INF/container.xml", options).unwrap();
        zip.write_all(br#"<container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#).unwrap();
        zip.start_file("OPS/book.opf", options).unwrap();
        write!(zip, r#"<package xmlns:dc="x"><metadata><dc:title>{title}</dc:title></metadata><manifest><item id="c" href="c.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c"/></spine></package>"#).unwrap();
        zip.start_file("OPS/c.xhtml", options).unwrap();
        zip.write_all(b"<html><body>Readable</body></html>")
            .unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn txt_import_override_and_encoding_change_preserve_reading_data() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("Legacy.TXT");
        let text = "Chapter 1\n中文内容。\nChapter 2\n下一章。\n";
        let (encoded, _, malformed) = encoding_rs::GBK.encode(text);
        assert!(!malformed);
        fs::write(&source, encoded.as_ref()).unwrap();
        let mut library = crate::Library::create(temp.path().join("library")).unwrap();
        let id = library
            .import_one_with_encoding(&source, &ImportOptions::default(), Some("gb18030"))
            .unwrap();
        let book = library.get_book(id).unwrap();
        assert_eq!(book.format, BookFormat::Txt);
        assert_eq!(book.text_encoding.as_deref(), Some("gb18030"));
        assert_eq!(book.title, "Legacy");
        let source_path = library.managed_file_path(&book).unwrap();
        assert_eq!(source_path.extension().unwrap(), "txt");
        let navigation = crate::reader::AdapterRegistry::default()
            .get(BookFormat::Txt)
            .navigation_with_options(&source_path, &library.reader_options(&book))
            .unwrap();
        assert_eq!(navigation.toc.len(), 2);
        let mut locator = navigation.toc[1].locator.clone();
        locator.location["scroll_fraction"] = serde_json::json!(0.35);
        library
            .update_reading(id, ReadingStatus::Reading, 0.65, Some(&locator))
            .unwrap();
        library.add_bookmark(id, "Second", &locator).unwrap();
        assert!(library.set_text_encoding(id, Some("utf-8")).is_err());
        assert_eq!(
            library.get_book(id).unwrap().text_encoding.as_deref(),
            Some("gb18030")
        );
        let updated = library.set_text_encoding(id, Some("gbk")).unwrap();
        assert_eq!(updated.text_encoding.as_deref(), Some("gb18030"));
        assert_eq!(updated.progress, 0.65);
        assert_eq!(library.list_bookmarks(id).unwrap().len(), 1);
        drop(library);
        let reopened = crate::Library::open_existing(temp.path().join("library")).unwrap();
        assert_eq!(
            reopened.get_book(id).unwrap().text_encoding.as_deref(),
            Some("gb18030")
        );
        assert_eq!(reopened.list_bookmarks(id).unwrap()[0].label, "Second");
    }

    #[test]
    fn imports_deduplicates_edits_and_removes() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("sample.epub");
        epub(&source, "Sample");
        let library_dir = temp.path().join("library");
        let mut library = crate::Library::create(&library_dir).unwrap();
        let folder = library.create_folder("Fiction", None).unwrap();
        let options = ImportOptions {
            folder_id: Some(folder.id),
            tags: vec!["classic".into()],
        };
        let id = library.import_one(&source, &options).unwrap();
        let book = library.get_book(id).unwrap();
        assert_eq!(book.title, "Sample");
        assert_eq!(book.tags, ["classic"]);
        assert!(matches!(
            library.import_one(&source, &options),
            Err(CoreError::Duplicate { .. })
        ));
        library
            .update_book(
                id,
                BookPatch {
                    authors: Some(vec!["Ursula Le Guin".into()]),
                    rating: Some(Some(Rating::new(5).unwrap())),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(library.list_books(&BookQuery::default()).unwrap().len(), 1);
        assert_eq!(
            library
                .list_books(&BookQuery {
                    search: Some("Le Guin".into()),
                    sort: BookSort::Author,
                    direction: SortDirection::Ascending,
                    ..Default::default()
                })
                .unwrap()[0]
                .id,
            id
        );
        assert!(library.integrity_check().unwrap());
        let plan = library.prepare_removal(id).unwrap();
        fs::rename(&plan.managed_directory, temp.path().join("trashed")).unwrap();
        library.commit_removal(&plan).unwrap();
        assert!(matches!(
            library.get_book(id),
            Err(CoreError::BookNotFound(_))
        ));
    }

    #[test]
    fn missing_embedded_title_uses_original_filename() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("Original Title.EPUB");
        epub(&source, "");
        let mut library = crate::Library::create(temp.path().join("library")).unwrap();
        let id = library
            .import_one(&source, &ImportOptions::default())
            .unwrap();
        let book = library.get_book(id).unwrap();
        assert_eq!(book.title, "Original Title");
        assert!(library.managed_file_path(&book).unwrap().exists());
    }

    #[test]
    fn prevents_folder_cycles() {
        let temp = tempdir().unwrap();
        let mut library = crate::Library::create(temp.path()).unwrap();
        let a = library.create_folder("a", None).unwrap();
        let b = library.create_folder("b", Some(a.id)).unwrap();
        assert!(library.move_folder(a.id, Some(b.id)).is_err());
    }

    #[test]
    fn rejects_header_shaped_fake_mobi() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("fake.mobi");
        let mut bytes = vec![0_u8; 128];
        bytes[60..68].copy_from_slice(b"BOOKMOBI");
        fs::write(&source, bytes).unwrap();
        let mut library = crate::Library::create(temp.path().join("library")).unwrap();
        assert!(
            library
                .import_one(&source, &ImportOptions::default())
                .is_err()
        );
        assert!(
            fs::read_dir(library.items_path())
                .unwrap()
                .flatten()
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(".import-"))
        );
    }

    #[test]
    fn recovers_temporary_orphan_and_interrupted_removal() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("sample.epub");
        epub(&source, "Recovery");
        let library_path = temp.path().join("library");
        let mut library = crate::Library::create(&library_path).unwrap();
        let id = library
            .import_one(&source, &ImportOptions::default())
            .unwrap();
        let temporary = library.items_path().join(".import-crash");
        fs::create_dir(&temporary).unwrap();
        let orphan = library.items_path().join(Uuid::new_v4().to_string());
        fs::create_dir(&orphan).unwrap();
        let report = library.recover_file_state().unwrap();
        assert_eq!(report.removed_temporary_imports, 1);
        assert_eq!(report.quarantined_orphans, 1);

        let plan = library.prepare_removal(id).unwrap();
        fs::rename(&plan.managed_directory, temp.path().join("trash")).unwrap();
        drop(library);
        let library = crate::Library::open_existing(&library_path).unwrap();
        assert!(matches!(
            library.get_book(id),
            Err(CoreError::BookNotFound(_))
        ));
    }

    #[test]
    fn bookmarks_persist_and_cascade_when_book_is_removed() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("book.epub");
        epub(&source, "Bookmark Test");
        let path = temp.path().join("library");
        let mut library = crate::Library::create(&path).unwrap();
        let id = library
            .import_one(&source, &ImportOptions::default())
            .unwrap();
        let locator = ReaderLocator::new(
            BookFormat::Epub,
            serde_json::json!({
                "resource": "OPS/c.xhtml", "scroll_fraction": 0.42
            }),
        );
        assert!(library.add_bookmark(id, " ", &locator).is_err());
        let unsafe_locator = ReaderLocator::new(
            BookFormat::Epub,
            serde_json::json!({"resource": "../secret"}),
        );
        assert!(library.add_bookmark(id, "Unsafe", &unsafe_locator).is_err());
        let first = library.add_bookmark(id, "Opening", &locator).unwrap();
        let second = library.add_bookmark(id, "Middle", &locator).unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(
            library
                .rename_bookmark(first.id, "  Beginning  ")
                .unwrap()
                .label,
            "Beginning"
        );
        drop(library);

        let mut library = crate::Library::open_existing(&path).unwrap();
        let bookmarks = library.list_bookmarks(id).unwrap();
        assert_eq!(bookmarks.len(), 2);
        assert_eq!(bookmarks[0].locator.location["scroll_fraction"], 0.42);
        library.delete_bookmark(second.id).unwrap();
        assert_eq!(library.list_bookmarks(id).unwrap().len(), 1);
        let plan = library.prepare_removal(id).unwrap();
        fs::rename(&plan.managed_directory, temp.path().join("trashed")).unwrap();
        library.commit_removal(&plan).unwrap();
        let remaining: i64 = library
            .connection
            .query_row("SELECT COUNT(*) FROM bookmarks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn bulk_changes_are_atomic_and_status_keeps_progress() {
        let temp = tempdir().unwrap();
        let a = temp.path().join("a.epub");
        let b = temp.path().join("b.epub");
        epub(&a, "Alpha");
        epub(&b, "Beta");
        let mut library = crate::Library::create(temp.path().join("library")).unwrap();
        let ids = [
            library.import_one(&a, &ImportOptions::default()).unwrap(),
            library.import_one(&b, &ImportOptions::default()).unwrap(),
        ];
        let folder = library.create_folder("Grouped", None).unwrap();
        let locator = ReaderLocator::new(
            BookFormat::Epub,
            serde_json::json!({"resource": "OPS/c.xhtml"}),
        );
        library
            .update_reading(ids[0], ReadingStatus::Reading, 0.37, Some(&locator))
            .unwrap();
        assert_eq!(
            library
                .apply_bulk(&ids, BulkBookAction::SetFolder(Some(folder.id)))
                .unwrap(),
            2
        );
        assert_eq!(
            library
                .apply_bulk(&ids, BulkBookAction::AddTags(vec!["Review".into()]))
                .unwrap(),
            2
        );
        library
            .apply_bulk(&ids, BulkBookAction::SetFavorite(true))
            .unwrap();
        library
            .apply_bulk(
                &ids,
                BulkBookAction::SetRating(Some(Rating::new(4).unwrap())),
            )
            .unwrap();
        library
            .apply_bulk(
                &ids,
                BulkBookAction::SetReadingStatus(ReadingStatus::Finished),
            )
            .unwrap();
        for id in ids {
            let book = library.get_book(id).unwrap();
            assert_eq!(book.folder_id, Some(folder.id));
            assert_eq!(book.tags, ["Review"]);
            assert!(book.favorite);
            assert_eq!(book.rating.unwrap().get(), 4);
            assert_eq!(book.reading_status, ReadingStatus::Finished);
        }
        let first = library.get_book(ids[0]).unwrap();
        assert_eq!(first.progress, 0.37);
        assert_eq!(first.reader_locator.unwrap().location, locator.location);
        let unknown = Uuid::new_v4();
        assert!(
            library
                .apply_bulk(&[ids[0], unknown], BulkBookAction::SetFavorite(false))
                .is_err()
        );
        assert!(library.get_book(ids[0]).unwrap().favorite);
        assert!(
            library
                .apply_bulk(&ids, BulkBookAction::SetFolder(Some(unknown)))
                .is_err()
        );
        assert!(library.get_book(ids[0]).unwrap().folder_id == Some(folder.id));
        assert!(
            library
                .apply_bulk(&ids, BulkBookAction::AddTags(vec![" ".into()]))
                .is_err()
        );
        library
            .apply_bulk(&ids, BulkBookAction::RemoveTags(vec!["review".into()]))
            .unwrap();
        library
            .apply_bulk(&ids, BulkBookAction::SetRating(None))
            .unwrap();
        for id in ids {
            let book = library.get_book(id).unwrap();
            assert!(book.tags.is_empty());
            assert!(book.rating.is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn removal_rejects_symlinked_managed_source() {
        use std::os::unix::fs::symlink;
        let temp = tempdir().unwrap();
        let source = temp.path().join("sample.epub");
        epub(&source, "Symlink");
        let mut library = crate::Library::create(temp.path().join("library")).unwrap();
        let id = library
            .import_one(&source, &ImportOptions::default())
            .unwrap();
        let managed = library
            .managed_file_path(&library.get_book(id).unwrap())
            .unwrap();
        fs::remove_file(&managed).unwrap();
        symlink(&source, &managed).unwrap();
        assert!(library.prepare_removal(id).is_err());
    }
}
