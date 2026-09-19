use std::{
    fs,
    path::Path,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Row, Transaction, params, params_from_iter,
    types::Value,
};
use uuid::Uuid;

use crate::{
    Book, BookFormat, BookPatch, Bookmark, BulkBookAction, CoreError, Folder, PartialDate, Rating,
    ReaderLocator, ReadingStatus, Result, SCHEMA_VERSION,
};

#[derive(Debug, Clone, Copy, Default)]
pub enum BookSort {
    #[default]
    ImportedAt,
    Title,
    Author,
    PublicationDate,
    Rating,
    Progress,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum SortDirection {
    Ascending,
    #[default]
    Descending,
}

#[derive(Debug, Clone, Default)]
pub struct BookQuery {
    pub search: Option<String>,
    pub format: Option<BookFormat>,
    pub folder_id: Option<Uuid>,
    pub tag: Option<String>,
    pub favorite: Option<bool>,
    pub minimum_rating: Option<Rating>,
    pub reading_status: Option<ReadingStatus>,
    pub sort: BookSort,
    pub direction: SortDirection,
}

pub(crate) struct NewBook {
    pub book: Book,
}

/// Strict, read-only preflight for the "open existing library" path. In
/// particular, this must never turn an arbitrary folder into a new database.
pub(crate) fn validate_existing(path: &Path) -> Result<u32> {
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|error| {
            CoreError::Validation(format!("not a readable library database: {error}"))
        })?;
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if !(1..=SCHEMA_VERSION).contains(&version) {
        return Err(CoreError::Validation(format!(
            "unsupported library database schema: {version}"
        )));
    }
    let mut required = vec![
        (
            "books",
            &[
                "id",
                "title",
                "abstract_text",
                "publication_date",
                "edition",
                "publisher",
                "format",
                "folder_id",
                "favorite",
                "rating",
                "reading_status",
                "progress",
                "reader_locator_json",
                "original_path",
                "managed_path",
                "cover_path",
                "sha256",
                "raw_metadata_json",
                "imported_at",
                "updated_at",
            ][..],
        ),
        ("authors", &["id", "name"][..]),
        ("book_authors", &["book_id", "author_id", "position"][..]),
        ("folders", &["id", "name", "parent_id"][..]),
        ("tags", &["id", "name"][..]),
        ("book_tags", &["book_id", "tag_id"][..]),
    ];
    if version >= 2 {
        required.push((
            "pending_removals",
            &["book_id", "managed_directory", "created_at"],
        ));
    }
    if version >= 3 {
        required.push((
            "bookmarks",
            &[
                "id",
                "book_id",
                "label",
                "locator_json",
                "created_at",
                "updated_at",
            ],
        ));
    }
    if version >= 4 {
        required[0].1 = &[
            "id",
            "title",
            "abstract_text",
            "publication_date",
            "edition",
            "publisher",
            "format",
            "folder_id",
            "favorite",
            "rating",
            "reading_status",
            "progress",
            "reader_locator_json",
            "original_path",
            "managed_path",
            "cover_path",
            "sha256",
            "raw_metadata_json",
            "imported_at",
            "updated_at",
            "text_encoding",
        ];
    }
    for (table, columns) in required {
        let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        let present = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if columns
            .iter()
            .any(|column| !present.iter().any(|item| item == column))
        {
            return Err(CoreError::Validation(format!(
                "incomplete library database table: {table}"
            )));
        }
    }
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(CoreError::Validation(
            "library database failed integrity check".into(),
        ));
    }
    let foreign_key_errors: i64 =
        connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })?;
    if foreign_key_errors != 0 {
        return Err(CoreError::Validation(
            "library database contains broken references".into(),
        ));
    }
    Ok(version)
}

pub(crate) fn open(path: &Path) -> Result<Connection> {
    let existed = path.exists();
    let connection = Connection::open(path)?;
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(CoreError::Validation(format!(
            "database schema {version} is newer than supported schema {SCHEMA_VERSION}"
        )));
    }
    if existed && version > 0 && version < SCHEMA_VERSION {
        let stamp = now();
        let backup = path.with_file_name(format!(
            "library.sqlite3.backup-v{version}-{stamp}-{}",
            Uuid::new_v4()
        ));
        fs::copy(path, &backup).map_err(|error| crate::error::io(&backup, error))?;
    }
    connection.execute_batch(
        "PRAGMA foreign_keys = ON; PRAGMA journal_mode = DELETE; PRAGMA synchronous = FULL;",
    )?;
    migrate(&connection, version)?;
    Ok(connection)
}

fn migrate(connection: &Connection, from: u32) -> Result<()> {
    if from < 1 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE books (
               id TEXT PRIMARY KEY,
               title TEXT NOT NULL CHECK(length(trim(title)) > 0),
               abstract_text TEXT,
               publication_date TEXT,
               edition TEXT,
               publisher TEXT,
               format TEXT NOT NULL CHECK(format IN ('epub','pdf','mobi')),
               folder_id TEXT REFERENCES folders(id) ON DELETE SET NULL,
               favorite INTEGER NOT NULL DEFAULT 0 CHECK(favorite IN (0,1)),
               rating INTEGER CHECK(rating BETWEEN 1 AND 5),
               reading_status TEXT NOT NULL DEFAULT 'unread' CHECK(reading_status IN ('unread','reading','finished')),
               progress REAL NOT NULL DEFAULT 0 CHECK(progress BETWEEN 0 AND 1),
               reader_locator_json TEXT,
               original_path TEXT NOT NULL,
               managed_path TEXT NOT NULL UNIQUE,
               cover_path TEXT,
               sha256 TEXT NOT NULL UNIQUE,
               raw_metadata_json TEXT NOT NULL DEFAULT '{}',
               imported_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL
             );
             CREATE TABLE authors (
               id INTEGER PRIMARY KEY,
               name TEXT NOT NULL UNIQUE COLLATE NOCASE
             );
             CREATE TABLE book_authors (
               book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
               author_id INTEGER NOT NULL REFERENCES authors(id) ON DELETE RESTRICT,
               position INTEGER NOT NULL,
               PRIMARY KEY(book_id, author_id), UNIQUE(book_id, position)
             );
             CREATE TABLE folders (
               id TEXT PRIMARY KEY,
               name TEXT NOT NULL CHECK(length(trim(name)) > 0),
               parent_id TEXT REFERENCES folders(id) ON DELETE RESTRICT
             );
             CREATE UNIQUE INDEX folders_siblings_unique ON folders(COALESCE(parent_id, ''), name COLLATE NOCASE);
             CREATE TABLE tags (
               id TEXT PRIMARY KEY,
               name TEXT NOT NULL UNIQUE COLLATE NOCASE
             );
             CREATE TABLE book_tags (
               book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
               tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
               PRIMARY KEY(book_id, tag_id)
             );
             CREATE INDEX books_title_idx ON books(title COLLATE NOCASE);
             CREATE INDEX books_folder_idx ON books(folder_id);
             CREATE INDEX book_tags_tag_idx ON book_tags(tag_id);
             PRAGMA user_version = 1;
             COMMIT;"
        )?;
    }
    if from < 2 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE pending_removals (
               book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
               managed_directory TEXT NOT NULL,
               created_at INTEGER NOT NULL
             );
             PRAGMA user_version = 2;
             COMMIT;",
        )?;
    }
    if from < 3 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE bookmarks (
               id TEXT PRIMARY KEY,
               book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
               label TEXT NOT NULL CHECK(length(trim(label)) > 0),
               locator_json TEXT NOT NULL,
               created_at INTEGER NOT NULL,
               updated_at INTEGER NOT NULL
             );
             CREATE INDEX bookmarks_book_idx ON bookmarks(book_id, created_at);
             PRAGMA user_version = 3;
             COMMIT;",
        )?;
    }
    if from < 4 {
        // SQLite cannot widen an existing CHECK constraint in place. Rebuild the
        // parent table with foreign-key enforcement temporarily disabled, then
        // prove all child relationships still resolve before committing.
        connection.execute_batch("PRAGMA foreign_keys = OFF; BEGIN IMMEDIATE;")?;
        let upgraded = (|| -> Result<()> {
            connection.execute_batch(
                "CREATE TABLE books_new (
                   id TEXT PRIMARY KEY,
                   title TEXT NOT NULL CHECK(length(trim(title)) > 0),
                   abstract_text TEXT,
                   publication_date TEXT,
                   edition TEXT,
                   publisher TEXT,
                   format TEXT NOT NULL CHECK(format IN ('epub','pdf','mobi','txt')),
                   folder_id TEXT REFERENCES folders(id) ON DELETE SET NULL,
                   favorite INTEGER NOT NULL DEFAULT 0 CHECK(favorite IN (0,1)),
                   rating INTEGER CHECK(rating BETWEEN 1 AND 5),
                   reading_status TEXT NOT NULL DEFAULT 'unread' CHECK(reading_status IN ('unread','reading','finished')),
                   progress REAL NOT NULL DEFAULT 0 CHECK(progress BETWEEN 0 AND 1),
                   reader_locator_json TEXT,
                   original_path TEXT NOT NULL,
                   managed_path TEXT NOT NULL UNIQUE,
                   cover_path TEXT,
                   sha256 TEXT NOT NULL UNIQUE,
                   raw_metadata_json TEXT NOT NULL DEFAULT '{}',
                   imported_at INTEGER NOT NULL,
                   updated_at INTEGER NOT NULL,
                   text_encoding TEXT CHECK(format = 'txt' OR text_encoding IS NULL)
                 );
                 INSERT INTO books_new(id,title,abstract_text,publication_date,edition,publisher,format,folder_id,favorite,rating,reading_status,progress,reader_locator_json,original_path,managed_path,cover_path,sha256,raw_metadata_json,imported_at,updated_at)
                   SELECT id,title,abstract_text,publication_date,edition,publisher,format,folder_id,favorite,rating,reading_status,progress,reader_locator_json,original_path,managed_path,cover_path,sha256,raw_metadata_json,imported_at,updated_at FROM books;
                 DROP TABLE books;
                 ALTER TABLE books_new RENAME TO books;
                 CREATE INDEX books_title_idx ON books(title COLLATE NOCASE);
                 CREATE INDEX books_folder_idx ON books(folder_id);",
            )?;
            let foreign_key_errors: i64 = connection.query_row(
                "SELECT COUNT(*) FROM pragma_foreign_key_check",
                [],
                |row| row.get(0),
            )?;
            if foreign_key_errors != 0 {
                return Err(CoreError::Validation(
                    "schema upgrade broke book relationships".into(),
                ));
            }
            connection.execute_batch("PRAGMA user_version = 4; COMMIT;")?;
            Ok(())
        })();
        if upgraded.is_err() {
            let _ = connection.execute_batch("ROLLBACK;");
        }
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        upgraded?;
    }
    Ok(())
}

impl crate::Library {
    pub fn schema_version(&self) -> Result<u32> {
        Ok(self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    pub fn integrity_check(&self) -> Result<bool> {
        let value: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(value == "ok")
    }

    pub fn get_book(&self, id: Uuid) -> Result<Book> {
        let mut book = self.connection.query_row(
            "SELECT id,title,abstract_text,publication_date,edition,publisher,format,folder_id,favorite,rating,reading_status,progress,reader_locator_json,original_path,managed_path,cover_path,sha256,raw_metadata_json,imported_at,updated_at,text_encoding FROM books WHERE id=?1",
            [id.to_string()], row_to_book,
        ).optional()?.ok_or_else(|| CoreError::BookNotFound(id.to_string()))?;
        book.authors = self.relations("SELECT a.name FROM authors a JOIN book_authors ba ON ba.author_id=a.id WHERE ba.book_id=?1 ORDER BY ba.position", id)?;
        book.tags = self.relations("SELECT t.name FROM tags t JOIN book_tags bt ON bt.tag_id=t.id WHERE bt.book_id=?1 ORDER BY t.name COLLATE NOCASE", id)?;
        Ok(book)
    }

    pub fn find_by_sha256(&self, sha256: &str) -> Result<Option<Uuid>> {
        let value: Option<String> = self
            .connection
            .query_row("SELECT id FROM books WHERE sha256=?1", [sha256], |row| {
                row.get(0)
            })
            .optional()?;
        value.map(parse_uuid).transpose()
    }

    pub fn list_books(&self, query: &BookQuery) -> Result<Vec<Book>> {
        let mut sql = String::from(
            "SELECT DISTINCT b.id FROM books b LEFT JOIN book_authors ba ON ba.book_id=b.id LEFT JOIN authors a ON a.id=ba.author_id LEFT JOIN book_tags bt ON bt.book_id=b.id LEFT JOIN tags t ON t.id=bt.tag_id WHERE 1=1",
        );
        let mut values = Vec::<Value>::new();
        if let Some(search) = query.search.as_ref().filter(|s| !s.trim().is_empty()) {
            sql.push_str(
                " AND (b.title LIKE ? OR a.name LIKE ? OR b.publisher LIKE ? OR t.name LIKE ?)",
            );
            let value = Value::from(format!("%{}%", search.trim()));
            values.extend([value.clone(), value.clone(), value.clone(), value]);
        }
        if let Some(format) = query.format {
            sql.push_str(" AND b.format=?");
            values.push(format.to_string().into());
        }
        if let Some(folder) = query.folder_id {
            sql.push_str(" AND b.folder_id=?");
            values.push(folder.to_string().into());
        }
        if let Some(tag) = query.tag.as_ref() {
            sql.push_str(" AND t.name=? COLLATE NOCASE");
            values.push(tag.clone().into());
        }
        if let Some(favorite) = query.favorite {
            sql.push_str(" AND b.favorite=?");
            values.push(i64::from(favorite).into());
        }
        if let Some(rating) = query.minimum_rating {
            sql.push_str(" AND b.rating>=?");
            values.push(i64::from(rating.get()).into());
        }
        if let Some(status) = query.reading_status {
            sql.push_str(" AND b.reading_status=?");
            values.push(status.to_string().into());
        }
        sql.push_str(" ORDER BY ");
        sql.push_str(match query.sort {
            BookSort::ImportedAt => "b.imported_at",
            BookSort::Title => "b.title COLLATE NOCASE",
            BookSort::Author => "(SELECT MIN(a2.name) FROM authors a2 JOIN book_authors ba2 ON ba2.author_id=a2.id WHERE ba2.book_id=b.id) COLLATE NOCASE",
            BookSort::PublicationDate => "b.publication_date",
            BookSort::Rating => "b.rating",
            BookSort::Progress => "b.progress",
        });
        sql.push_str(match query.direction {
            SortDirection::Ascending => " ASC",
            SortDirection::Descending => " DESC",
        });
        let mut statement = self.connection.prepare(&sql)?;
        let ids = statement
            .query_map(params_from_iter(values), |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| self.get_book(parse_uuid(id)?))
            .collect()
    }

    pub fn update_book(&mut self, id: Uuid, patch: BookPatch) -> Result<Book> {
        let current = self.get_book(id)?;
        let title = patch.title.unwrap_or(current.title);
        if title.trim().is_empty() {
            return Err(CoreError::Validation("book title cannot be empty".into()));
        }
        let tx = self.connection.transaction()?;
        tx.execute("UPDATE books SET title=?2,abstract_text=?3,publication_date=?4,edition=?5,publisher=?6,folder_id=?7,favorite=?8,rating=?9,updated_at=?10 WHERE id=?1", params![
            id.to_string(), title.trim(), patch.abstract_text.unwrap_or(current.abstract_text),
            patch.publication_date.unwrap_or(current.publication_date).map(|v| v.to_string()),
            patch.edition.unwrap_or(current.edition), patch.publisher.unwrap_or(current.publisher),
            patch.folder_id.unwrap_or(current.folder_id).map(|v| v.to_string()),
            patch.favorite.unwrap_or(current.favorite),
            patch.rating.unwrap_or(current.rating).map(|v| v.get()), now(),
        ])?;
        if let Some(authors) = patch.authors {
            replace_authors(&tx, id, &authors)?;
        }
        if let Some(tags) = patch.tags {
            replace_tags(&tx, id, &tags)?;
        }
        tx.commit()?;
        self.get_book(id)
    }

    /// Change a TXT book's decoding without changing its managed source. A
    /// missing label requests fresh automatic detection. Locators are mapped by
    /// their approximate whole-book progress when section boundaries change.
    pub fn set_text_encoding(&mut self, id: Uuid, encoding: Option<&str>) -> Result<Book> {
        let book = self.get_book(id)?;
        if book.format != BookFormat::Txt {
            return Err(CoreError::Validation(
                "text encoding only applies to TXT books".into(),
            ));
        }
        let path = self.managed_file_path(&book)?;
        let adapter = crate::reader::AdapterRegistry::default();
        let adapter = adapter.get(BookFormat::Txt);
        let old_options = self.reader_options(&book);
        let old_navigation = adapter.navigation_with_options(&path, &old_options)?;
        let mut new_options = old_options.clone();
        new_options.text_encoding = encoding.map(str::to_owned);
        let metadata = adapter.inspect_with_options(&path, &new_options)?;
        let canonical = metadata.text_encoding.ok_or_else(|| {
            CoreError::Validation("TXT decoding did not produce an encoding".into())
        })?;
        new_options.text_encoding = Some(canonical.clone());
        let new_navigation = adapter.navigation_with_options(&path, &new_options)?;
        let old_count = old_navigation.sections.len();
        let new_count = new_navigation.sections.len();
        let mapped_reading = book
            .reader_locator
            .as_ref()
            .map(|locator| {
                if locator.location.get("section").is_some() {
                    remap_txt_locator(locator, old_count, new_count)
                } else {
                    locator.validate_for(BookFormat::Txt)?;
                    Ok(locator.clone())
                }
            })
            .transpose()?;
        let bookmarks = self.list_bookmarks(id)?;
        let mapped_bookmarks = bookmarks
            .iter()
            .map(|bookmark| {
                Ok((
                    bookmark.id,
                    remap_txt_locator(&bookmark.locator, old_count, new_count)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let tx = self.connection.transaction()?;
        tx.execute(
            "UPDATE books SET text_encoding=?2,reader_locator_json=?3,updated_at=?4 WHERE id=?1",
            params![
                id.to_string(),
                canonical,
                mapped_reading
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                now()
            ],
        )?;
        for (bookmark_id, locator) in mapped_bookmarks {
            tx.execute(
                "UPDATE bookmarks SET locator_json=?2,updated_at=?3 WHERE id=?1",
                params![
                    bookmark_id.to_string(),
                    serde_json::to_string(&locator)?,
                    now()
                ],
            )?;
        }
        tx.commit()?;
        self.get_book(id)
    }

    pub fn update_reading(
        &mut self,
        id: Uuid,
        status: ReadingStatus,
        progress: f64,
        locator: Option<&ReaderLocator>,
    ) -> Result<()> {
        if !(0.0..=1.0).contains(&progress) || !progress.is_finite() {
            return Err(CoreError::Validation(
                "reading progress must be from 0 to 1".into(),
            ));
        }
        if let Some(locator) = locator {
            let book = self.get_book(id)?;
            locator.validate_for(book.format)?;
        }
        let affected = self.connection.execute("UPDATE books SET reading_status=?2,progress=?3,reader_locator_json=?4,updated_at=?5 WHERE id=?1", params![id.to_string(),status.to_string(),progress,locator.map(serde_json::to_string).transpose()?,now()])?;
        if affected == 0 {
            return Err(CoreError::BookNotFound(id.to_string()));
        }
        Ok(())
    }

    pub fn list_bookmarks(&self, book_id: Uuid) -> Result<Vec<Bookmark>> {
        self.get_book(book_id)?;
        let mut statement = self.connection.prepare(
            "SELECT id,book_id,label,locator_json,created_at,updated_at FROM bookmarks WHERE book_id=?1 ORDER BY created_at,id",
        )?;
        Ok(statement
            .query_map([book_id.to_string()], row_to_bookmark)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn add_bookmark(
        &mut self,
        book_id: Uuid,
        label: &str,
        locator: &ReaderLocator,
    ) -> Result<Bookmark> {
        let label = bookmark_label(label)?;
        let book = self.get_book(book_id)?;
        locator.validate_bookmark_for(book.format)?;
        let path = self.managed_file_path(&book)?;
        crate::reader::AdapterRegistry::default()
            .get(book.format)
            .read_with_options(&path, Some(locator), &self.reader_options(&book))?;
        let stamp = now();
        let bookmark = Bookmark {
            id: Uuid::new_v4(),
            book_id,
            label,
            locator: locator.clone(),
            created_at: stamp,
            updated_at: stamp,
        };
        self.connection.execute(
            "INSERT INTO bookmarks(id,book_id,label,locator_json,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![bookmark.id.to_string(), book_id.to_string(), bookmark.label,
                serde_json::to_string(locator)?, stamp, stamp],
        )?;
        Ok(bookmark)
    }

    pub fn rename_bookmark(&mut self, id: Uuid, label: &str) -> Result<Bookmark> {
        let label = bookmark_label(label)?;
        let affected = self.connection.execute(
            "UPDATE bookmarks SET label=?2,updated_at=?3 WHERE id=?1",
            params![id.to_string(), label, now()],
        )?;
        if affected == 0 {
            return Err(CoreError::Validation(format!("bookmark not found: {id}")));
        }
        self.connection.query_row(
            "SELECT id,book_id,label,locator_json,created_at,updated_at FROM bookmarks WHERE id=?1",
            [id.to_string()], row_to_bookmark,
        ).map_err(Into::into)
    }

    pub fn delete_bookmark(&mut self, id: Uuid) -> Result<()> {
        let affected = self
            .connection
            .execute("DELETE FROM bookmarks WHERE id=?1", [id.to_string()])?;
        if affected == 0 {
            return Err(CoreError::Validation(format!("bookmark not found: {id}")));
        }
        Ok(())
    }

    /// Validates every target and action before beginning one all-or-nothing
    /// transaction. Reading-status edits deliberately preserve progress/locator.
    pub fn apply_bulk(&mut self, ids: &[Uuid], action: BulkBookAction) -> Result<usize> {
        let mut seen = std::collections::HashSet::new();
        let ids: Vec<Uuid> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
        if ids.is_empty() {
            return Ok(0);
        }
        for id in &ids {
            self.get_book(*id)?;
        }
        match &action {
            BulkBookAction::SetFolder(Some(folder_id)) => {
                self.folder(*folder_id)?;
            }
            BulkBookAction::AddTags(tags) | BulkBookAction::RemoveTags(tags)
                if tags.is_empty() || tags.iter().any(|tag| tag.trim().is_empty()) =>
            {
                return Err(CoreError::Validation("bulk tags must not be empty".into()));
            }
            _ => {}
        }
        let tx = self.connection.transaction()?;
        let stamp = now();
        for id in &ids {
            let id_string = id.to_string();
            match &action {
                BulkBookAction::SetFolder(folder_id) => {
                    tx.execute(
                        "UPDATE books SET folder_id=?2,updated_at=?3 WHERE id=?1",
                        params![id_string, folder_id.map(|v| v.to_string()), stamp],
                    )?;
                }
                BulkBookAction::AddTags(tags) => {
                    for tag in normalized(tags) {
                        tx.execute(
                            "INSERT INTO tags(id,name) VALUES(?1,?2) ON CONFLICT(name) DO NOTHING",
                            params![Uuid::new_v4().to_string(), tag],
                        )?;
                        let tag_id: String = tx.query_row(
                            "SELECT id FROM tags WHERE name=?1 COLLATE NOCASE",
                            [&tag],
                            |row| row.get(0),
                        )?;
                        tx.execute(
                            "INSERT OR IGNORE INTO book_tags(book_id,tag_id) VALUES(?1,?2)",
                            params![id_string, tag_id],
                        )?;
                    }
                    tx.execute(
                        "UPDATE books SET updated_at=?2 WHERE id=?1",
                        params![id_string, stamp],
                    )?;
                }
                BulkBookAction::RemoveTags(tags) => {
                    for tag in normalized(tags) {
                        tx.execute("DELETE FROM book_tags WHERE book_id=?1 AND tag_id IN (SELECT id FROM tags WHERE name=?2 COLLATE NOCASE)", params![id_string, tag])?;
                    }
                    tx.execute(
                        "UPDATE books SET updated_at=?2 WHERE id=?1",
                        params![id_string, stamp],
                    )?;
                }
                BulkBookAction::SetFavorite(favorite) => {
                    tx.execute(
                        "UPDATE books SET favorite=?2,updated_at=?3 WHERE id=?1",
                        params![id_string, favorite, stamp],
                    )?;
                }
                BulkBookAction::SetRating(rating) => {
                    tx.execute(
                        "UPDATE books SET rating=?2,updated_at=?3 WHERE id=?1",
                        params![id_string, rating.map(|v| v.get()), stamp],
                    )?;
                }
                BulkBookAction::SetReadingStatus(status) => {
                    tx.execute(
                        "UPDATE books SET reading_status=?2,updated_at=?3 WHERE id=?1",
                        params![id_string, status.to_string(), stamp],
                    )?;
                }
            }
        }
        if matches!(action, BulkBookAction::RemoveTags(_)) {
            tx.execute(
                "DELETE FROM tags WHERE NOT EXISTS (SELECT 1 FROM book_tags WHERE tag_id=tags.id)",
                [],
            )?;
        }
        tx.commit()?;
        Ok(ids.len())
    }

    pub fn create_folder(&mut self, name: &str, parent_id: Option<Uuid>) -> Result<Folder> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CoreError::Validation("folder name cannot be empty".into()));
        }
        if let Some(parent) = parent_id {
            self.folder(parent)?;
        }
        let folder = Folder {
            id: Uuid::new_v4(),
            name: name.into(),
            parent_id,
        };
        self.connection.execute(
            "INSERT INTO folders(id,name,parent_id) VALUES(?1,?2,?3)",
            params![
                folder.id.to_string(),
                folder.name,
                folder.parent_id.map(|v| v.to_string())
            ],
        )?;
        Ok(folder)
    }

    pub fn move_folder(&mut self, id: Uuid, parent_id: Option<Uuid>) -> Result<()> {
        self.folder(id)?;
        if parent_id == Some(id) {
            return Err(CoreError::Validation(
                "a folder cannot contain itself".into(),
            ));
        }
        let mut cursor = parent_id;
        while let Some(parent) = cursor {
            if parent == id {
                return Err(CoreError::Validation(
                    "folder move would create a cycle".into(),
                ));
            }
            cursor = self.folder(parent)?.parent_id;
        }
        self.connection.execute(
            "UPDATE folders SET parent_id=?2 WHERE id=?1",
            params![id.to_string(), parent_id.map(|v| v.to_string())],
        )?;
        Ok(())
    }

    pub fn rename_folder(&mut self, id: Uuid, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CoreError::Validation("folder name cannot be empty".into()));
        }
        let affected = self.connection.execute(
            "UPDATE folders SET name=?2 WHERE id=?1",
            params![id.to_string(), name],
        )?;
        if affected == 0 {
            return Err(CoreError::Validation(format!("folder not found: {id}")));
        }
        Ok(())
    }

    pub fn delete_folder(&mut self, id: Uuid) -> Result<()> {
        let affected = self
            .connection
            .execute("DELETE FROM folders WHERE id=?1", [id.to_string()])?;
        if affected == 0 {
            return Err(CoreError::Validation(format!("folder not found: {id}")));
        }
        Ok(())
    }

    pub fn list_folders(&self) -> Result<Vec<Folder>> {
        let mut statement = self
            .connection
            .prepare("SELECT id,name,parent_id FROM folders ORDER BY name COLLATE NOCASE")?;
        Ok(statement
            .query_map([], folder_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn list_tags(&self) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT name FROM tags ORDER BY name COLLATE NOCASE")?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub(crate) fn insert_imported(&mut self, new: NewBook) -> Result<()> {
        let b = &new.book;
        let tx = self.connection.transaction()?;
        tx.execute("INSERT INTO books(id,title,abstract_text,publication_date,edition,publisher,format,folder_id,favorite,rating,reading_status,progress,reader_locator_json,original_path,managed_path,cover_path,sha256,raw_metadata_json,imported_at,updated_at,text_encoding) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)", params![
            b.id.to_string(),b.title,b.abstract_text,b.publication_date.as_ref().map(ToString::to_string),b.edition,b.publisher,b.format.to_string(),b.folder_id.map(|v| v.to_string()),b.favorite,b.rating.map(|v|v.get()),b.reading_status.to_string(),b.progress,b.reader_locator.as_ref().map(serde_json::to_string).transpose()?,b.original_path,b.managed_path,b.cover_path,b.sha256,serde_json::to_string(&b.raw_metadata)?,b.imported_at,b.updated_at,b.text_encoding
        ])?;
        replace_authors(&tx, b.id, &b.authors)?;
        replace_tags(&tx, b.id, &b.tags)?;
        tx.commit()?;
        Ok(())
    }

    fn relations(&self, sql: &str, id: Uuid) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(sql)?;
        Ok(statement
            .query_map([id.to_string()], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?)
    }

    fn folder(&self, id: Uuid) -> Result<Folder> {
        self.connection
            .query_row(
                "SELECT id,name,parent_id FROM folders WHERE id=?1",
                [id.to_string()],
                folder_row,
            )
            .optional()?
            .ok_or_else(|| CoreError::Validation(format!("folder not found: {id}")))
    }
}

fn replace_authors(tx: &Transaction<'_>, book_id: Uuid, authors: &[String]) -> Result<()> {
    tx.execute(
        "DELETE FROM book_authors WHERE book_id=?1",
        [book_id.to_string()],
    )?;
    for (position, author) in normalized(authors).into_iter().enumerate() {
        tx.execute(
            "INSERT INTO authors(name) VALUES(?1) ON CONFLICT(name) DO NOTHING",
            [&author],
        )?;
        let author_id: i64 = tx.query_row(
            "SELECT id FROM authors WHERE name=?1 COLLATE NOCASE",
            [&author],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO book_authors(book_id,author_id,position) VALUES(?1,?2,?3)",
            params![book_id.to_string(), author_id, position],
        )?;
    }
    tx.execute("DELETE FROM authors WHERE NOT EXISTS (SELECT 1 FROM book_authors WHERE author_id=authors.id)", [])?;
    Ok(())
}

fn replace_tags(tx: &Transaction<'_>, book_id: Uuid, tags: &[String]) -> Result<()> {
    tx.execute(
        "DELETE FROM book_tags WHERE book_id=?1",
        [book_id.to_string()],
    )?;
    for tag in normalized(tags) {
        tx.execute(
            "INSERT INTO tags(id,name) VALUES(?1,?2) ON CONFLICT(name) DO NOTHING",
            params![Uuid::new_v4().to_string(), tag],
        )?;
        let tag_id: String = tx.query_row(
            "SELECT id FROM tags WHERE name=?1 COLLATE NOCASE",
            [&tag],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO book_tags(book_id,tag_id) VALUES(?1,?2)",
            params![book_id.to_string(), tag_id],
        )?;
    }
    tx.execute(
        "DELETE FROM tags WHERE NOT EXISTS (SELECT 1 FROM book_tags WHERE tag_id=tags.id)",
        [],
    )?;
    Ok(())
}

fn normalized(values: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for value in values.iter().map(|v| v.trim()).filter(|v| !v.is_empty()) {
        if !result
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(value))
        {
            result.push(value.to_owned());
        }
    }
    result
}

fn row_to_book(row: &Row<'_>) -> rusqlite::Result<Book> {
    let parse_failure = |index, error: CoreError| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    };
    let id: String = row.get(0)?;
    let date: Option<String> = row.get(3)?;
    let format: String = row.get(6)?;
    let folder: Option<String> = row.get(7)?;
    let rating: Option<u8> = row.get(9)?;
    let status: String = row.get(10)?;
    let locator: Option<String> = row.get(12)?;
    let raw: String = row.get(17)?;
    Ok(Book {
        id: Uuid::parse_str(&id)
            .map_err(|e| parse_failure(0, CoreError::Validation(e.to_string())))?,
        title: row.get(1)?,
        authors: vec![],
        abstract_text: row.get(2)?,
        publication_date: date
            .map(PartialDate::parse)
            .transpose()
            .map_err(|e| parse_failure(3, e))?,
        edition: row.get(4)?,
        publisher: row.get(5)?,
        format: BookFormat::from_str(&format).map_err(|e| parse_failure(6, e))?,
        text_encoding: row.get(20)?,
        folder_id: folder
            .map(|v| Uuid::parse_str(&v))
            .transpose()
            .map_err(|e| parse_failure(7, CoreError::Validation(e.to_string())))?,
        favorite: row.get(8)?,
        rating: rating
            .map(Rating::new)
            .transpose()
            .map_err(|e| parse_failure(9, e))?,
        reading_status: ReadingStatus::from_str(&status).map_err(|e| parse_failure(10, e))?,
        progress: row.get(11)?,
        reader_locator: locator
            .map(|v| serde_json::from_str(&v))
            .transpose()
            .map_err(|e| parse_failure(12, e.into()))?,
        original_path: row.get(13)?,
        managed_path: row.get(14)?,
        cover_path: row.get(15)?,
        sha256: row.get(16)?,
        raw_metadata: serde_json::from_str(&raw).map_err(|e| parse_failure(17, e.into()))?,
        imported_at: row.get(18)?,
        updated_at: row.get(19)?,
        tags: vec![],
    })
}

fn folder_row(row: &Row<'_>) -> rusqlite::Result<Folder> {
    let id: String = row.get(0)?;
    let parent: Option<String> = row.get(2)?;
    Ok(Folder {
        id: parse_uuid_sql(id, 0)?,
        name: row.get(1)?,
        parent_id: parent.map(|v| parse_uuid_sql(v, 2)).transpose()?,
    })
}

fn row_to_bookmark(row: &Row<'_>) -> rusqlite::Result<Bookmark> {
    let id: String = row.get(0)?;
    let book_id: String = row.get(1)?;
    let locator: String = row.get(3)?;
    Ok(Bookmark {
        id: parse_uuid_sql(id, 0)?,
        book_id: parse_uuid_sql(book_id, 1)?,
        label: row.get(2)?,
        locator: serde_json::from_str(&locator).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn bookmark_label(label: &str) -> Result<String> {
    let label = label.trim();
    if label.is_empty() || label.chars().count() > 120 {
        return Err(CoreError::Validation(
            "bookmark name must contain 1 to 120 characters".into(),
        ));
    }
    Ok(label.to_owned())
}

fn remap_txt_locator(
    locator: &ReaderLocator,
    old_count: usize,
    new_count: usize,
) -> Result<ReaderLocator> {
    locator.validate_bookmark_for(BookFormat::Txt)?;
    let old_section = locator.location["section"].as_u64().unwrap_or(0) as usize;
    let fraction = locator
        .location
        .get("scroll_fraction")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let old_count = old_count.max(1);
    let new_count = new_count.max(1);
    let absolute =
        ((old_section.min(old_count - 1) as f64 + fraction) / old_count as f64).clamp(0.0, 1.0);
    let position = absolute * new_count as f64;
    let section = (position.floor() as usize).min(new_count - 1);
    let section_fraction = (position - section as f64).clamp(0.0, 1.0);
    Ok(ReaderLocator::new(
        BookFormat::Txt,
        serde_json::json!({
            "section": section, "scroll_fraction": section_fraction,
        }),
    ))
}

fn parse_uuid(value: String) -> Result<Uuid> {
    Uuid::parse_str(&value).map_err(|e| CoreError::Validation(e.to_string()))
}
fn parse_uuid_sql(value: String, index: usize) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&value).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn migrates_a_new_database() {
        let dir = tempdir().unwrap();
        let db = open(&dir.path().join("db.sqlite")).unwrap();
        let version: u32 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn backs_up_before_upgrading_an_existing_database() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("library.sqlite3");
        let connection = open(&path).unwrap();
        connection
            .execute_batch("PRAGMA user_version = 3;")
            .unwrap();
        drop(connection);
        let connection = open(&path).unwrap();
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let backups = fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("library.sqlite3.backup-v3-")
            })
            .count();
        assert_eq!(backups, 1);
    }

    #[test]
    fn v3_upgrade_keeps_relations_and_accepts_txt() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("library.sqlite3");
        let old = Connection::open(&path).unwrap();
        old.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE folders(id TEXT PRIMARY KEY,name TEXT NOT NULL,parent_id TEXT);
             CREATE TABLE books(
               id TEXT PRIMARY KEY,title TEXT NOT NULL,abstract_text TEXT,publication_date TEXT,
               edition TEXT,publisher TEXT,format TEXT NOT NULL CHECK(format IN ('epub','pdf','mobi')),
               folder_id TEXT REFERENCES folders(id),favorite INTEGER NOT NULL,rating INTEGER,
               reading_status TEXT NOT NULL,progress REAL NOT NULL,reader_locator_json TEXT,
               original_path TEXT NOT NULL,managed_path TEXT NOT NULL UNIQUE,cover_path TEXT,
               sha256 TEXT NOT NULL UNIQUE,raw_metadata_json TEXT NOT NULL,
               imported_at INTEGER NOT NULL,updated_at INTEGER NOT NULL
             );
             CREATE TABLE authors(id INTEGER PRIMARY KEY,name TEXT NOT NULL UNIQUE);
             CREATE TABLE book_authors(book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,author_id INTEGER NOT NULL REFERENCES authors(id),position INTEGER NOT NULL,PRIMARY KEY(book_id,author_id));
             CREATE TABLE tags(id TEXT PRIMARY KEY,name TEXT NOT NULL UNIQUE);
             CREATE TABLE book_tags(book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,tag_id TEXT NOT NULL REFERENCES tags(id),PRIMARY KEY(book_id,tag_id));
             CREATE TABLE pending_removals(book_id TEXT PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,managed_directory TEXT NOT NULL,created_at INTEGER NOT NULL);
             CREATE TABLE bookmarks(id TEXT PRIMARY KEY,book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,label TEXT NOT NULL,locator_json TEXT NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
             INSERT INTO folders VALUES('folder','Shelf',NULL);
             INSERT INTO books VALUES('book','Old book',NULL,NULL,NULL,NULL,'epub','folder',1,5,'reading',0.4,NULL,'original.epub','items/book/source.epub',NULL,'hash','{}',100,101);
             INSERT INTO authors VALUES(1,'Author');
             INSERT INTO book_authors VALUES('book',1,0);
             INSERT INTO tags VALUES('tag','Tag');
             INSERT INTO book_tags VALUES('book','tag');
             INSERT INTO bookmarks VALUES('mark','book','Place','{}',102,103);
             PRAGMA user_version=3;",
        ).unwrap();
        drop(old);

        let upgraded = open(&path).unwrap();
        let version: u32 = upgraded
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4);
        let old_book: (String, Option<String>, String) = upgraded
            .query_row(
                "SELECT title,text_encoding,folder_id FROM books WHERE id='book'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(old_book, ("Old book".into(), None, "folder".into()));
        for table in ["book_authors", "book_tags", "bookmarks"] {
            let count: i64 = upgraded
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 1, "{table} relation lost");
        }
        let fk_errors: i64 = upgraded
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(fk_errors, 0);
        upgraded.execute(
            "INSERT INTO books(id,title,format,favorite,reading_status,progress,original_path,managed_path,sha256,raw_metadata_json,imported_at,updated_at,text_encoding) VALUES('txt','Text','txt',0,'unread',0,'original.txt','items/txt/source.txt','txt-hash','{}',104,104,'utf-8')", [],
        ).unwrap();
        assert!(fs::read_dir(dir.path()).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.sqlite3.backup-v3-")
        }));
    }
}
