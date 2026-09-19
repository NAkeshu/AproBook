use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CoreError, Result, SCHEMA_VERSION, database};

const MARKER: &str = ".theebook-library.json";
const DATABASE: &str = "library.sqlite3";
const LOCK: &str = ".theebook-library.lock";

#[derive(Debug, Serialize, Deserialize)]
struct LibraryMarker {
    library_id: Uuid,
    schema_version: u32,
}

pub struct Library {
    pub(crate) root: PathBuf,
    pub(crate) connection: Connection,
    _lock: File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalPlan {
    pub book_id: Uuid,
    pub managed_directory: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    pub removed_temporary_imports: usize,
    pub quarantined_orphans: usize,
    pub completed_removals: usize,
}

impl Library {
    /// Creates a new library in a new or empty directory.
    pub fn create(root: impl AsRef<Path>) -> Result<Self> {
        let root = absolute_path(root.as_ref())?;
        if root.exists()
            && (!root.is_dir()
                || fs::read_dir(&root)
                    .map_err(|error| crate::error::io(&root, error))?
                    .next()
                    .is_some())
        {
            return Err(CoreError::Validation(format!(
                "new library directory is not empty: {}",
                root.display()
            )));
        }
        fs::create_dir_all(root.join("items")).map_err(|error| crate::error::io(&root, error))?;
        let root = fs::canonicalize(&root).map_err(|error| crate::error::io(&root, error))?;
        let lock = lock_library(&root)?;
        let marker_path = root.join(MARKER);
        let marker = LibraryMarker {
            library_id: Uuid::new_v4(),
            schema_version: SCHEMA_VERSION,
        };
        let db_path = root.join(DATABASE);
        let connection = database::open(&db_path)?;
        write_marker(&marker_path, marker)?;
        let mut library = Self {
            root,
            connection,
            _lock: lock,
        };
        library.recover_file_state()?;
        Ok(library)
    }

    /// Opens an existing library. Invalid directories are rejected before
    /// creating a lock, modifying a marker, or opening SQLite for writes.
    pub fn open_existing(root: impl AsRef<Path>) -> Result<Self> {
        let root = absolute_path(root.as_ref())?;
        let root = fs::canonicalize(&root).map_err(|error| crate::error::io(&root, error))?;
        let marker = preflight_existing(&root)?;
        let lock = lock_library(&root)?;
        // Close the race between the read-only preflight and lock acquisition.
        let current = preflight_existing(&root)?;
        if current.library_id != marker.library_id {
            return Err(CoreError::Validation(
                "library identity changed while opening".into(),
            ));
        }
        let db_path = root.join(DATABASE);
        let connection = database::open(&db_path)?;
        write_marker(
            &root.join(MARKER),
            LibraryMarker {
                schema_version: SCHEMA_VERSION,
                ..current
            },
        )?;
        let mut library = Self {
            root,
            connection,
            _lock: lock,
        };
        library.recover_file_state()?;
        Ok(library)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn database_path(&self) -> PathBuf {
        self.root.join(DATABASE)
    }
    pub fn items_path(&self) -> PathBuf {
        self.root.join("items")
    }

    pub fn reader_options(&self, book: &crate::Book) -> crate::reader::ReaderOptions {
        crate::reader::ReaderOptions {
            text_encoding: book.text_encoding.clone(),
            cache_dir: Some(system_cache_dir()),
            sha256: Some(book.sha256.clone()),
        }
    }

    pub fn managed_file_path(&self, book: &crate::Book) -> Result<PathBuf> {
        let path = self.root.join(&book.managed_path);
        ensure_beneath(&self.root.join("items"), &path)?;
        Ok(path)
    }

    /// Resolves and validates the only directory that may be moved to Trash.
    pub fn prepare_removal(&self, book_id: Uuid) -> Result<RemovalPlan> {
        let book = self.get_book(book_id)?;
        let source = self.managed_file_path(&book)?;
        let directory = source
            .parent()
            .ok_or_else(|| CoreError::Validation("managed book has no parent directory".into()))?
            .to_path_buf();
        ensure_beneath(&self.items_path(), &directory)?;
        if directory == self.items_path() {
            return Err(CoreError::Validation(
                "refusing to remove the entire items directory".into(),
            ));
        }
        let items = fs::canonicalize(self.items_path())
            .map_err(|error| crate::error::io(self.items_path(), error))?;
        let directory_metadata = fs::symlink_metadata(&directory)
            .map_err(|error| crate::error::io(&directory, error))?;
        let source_metadata =
            fs::symlink_metadata(&source).map_err(|error| crate::error::io(&source, error))?;
        if directory_metadata.file_type().is_symlink() || source_metadata.file_type().is_symlink() {
            return Err(CoreError::Validation(
                "refusing to remove a library item containing a symlink boundary".into(),
            ));
        }
        let canonical_directory =
            fs::canonicalize(&directory).map_err(|error| crate::error::io(&directory, error))?;
        ensure_beneath(&items, &canonical_directory)?;
        self.connection.execute(
            "INSERT INTO pending_removals(book_id,managed_directory,created_at) VALUES(?1,?2,?3)
             ON CONFLICT(book_id) DO UPDATE SET managed_directory=excluded.managed_directory,created_at=excluded.created_at",
            rusqlite::params![book_id.to_string(), directory.to_string_lossy(), database::now()],
        )?;
        Ok(RemovalPlan {
            book_id,
            managed_directory: directory,
        })
    }

    /// Deletes the database record only after the platform layer moved the managed
    /// directory away (normally into the system Trash).
    pub fn commit_removal(&mut self, plan: &RemovalPlan) -> Result<()> {
        let book = self.get_book(plan.book_id)?;
        let source = self.managed_file_path(&book)?;
        let expected = source
            .parent()
            .ok_or_else(|| CoreError::Validation("managed book has no parent directory".into()))?;
        let pending: Option<String> = self
            .connection
            .query_row(
                "SELECT managed_directory FROM pending_removals WHERE book_id=?1",
                [plan.book_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if expected != plan.managed_directory
            || pending.as_deref() != Some(plan.managed_directory.to_string_lossy().as_ref())
        {
            return Err(CoreError::Validation(
                "removal plan does not match the managed book directory".into(),
            ));
        }
        if plan.managed_directory.exists() {
            return Err(CoreError::Validation(
                "managed directory still exists; file disposal did not complete".into(),
            ));
        }
        let tx = self.connection.transaction()?;
        tx.execute(
            "DELETE FROM pending_removals WHERE book_id=?1",
            [plan.book_id.to_string()],
        )?;
        let affected = tx.execute(
            "DELETE FROM books WHERE id = ?1",
            [plan.book_id.to_string()],
        )?;
        if affected == 0 {
            return Err(CoreError::BookNotFound(plan.book_id.to_string()));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn recover_file_state(&mut self) -> Result<RecoveryReport> {
        let mut report = RecoveryReport::default();
        let items = self.items_path();
        let orphan_root = items.join(".orphans");
        fs::create_dir_all(&orphan_root).map_err(|error| crate::error::io(&orphan_root, error))?;

        let pending = {
            let mut statement = self
                .connection
                .prepare("SELECT book_id,managed_directory FROM pending_removals")?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        for (book_id, directory) in pending {
            if !Path::new(&directory).exists() {
                let tx = self.connection.transaction()?;
                tx.execute("DELETE FROM pending_removals WHERE book_id=?1", [&book_id])?;
                tx.execute("DELETE FROM books WHERE id=?1", [&book_id])?;
                tx.commit()?;
                report.completed_removals += 1;
            }
        }

        for entry in fs::read_dir(&items).map_err(|error| crate::error::io(&items, error))? {
            let entry = entry.map_err(|error| crate::error::io(&items, error))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".orphans" {
                continue;
            }
            if name.starts_with(".import-") {
                if entry
                    .file_type()
                    .map_err(|error| crate::error::io(entry.path(), error))?
                    .is_symlink()
                {
                    fs::remove_file(entry.path())
                        .map_err(|error| crate::error::io(entry.path(), error))?;
                } else {
                    fs::remove_dir_all(entry.path())
                        .map_err(|error| crate::error::io(entry.path(), error))?;
                }
                report.removed_temporary_imports += 1;
                continue;
            }
            let Ok(id) = Uuid::parse_str(&name) else {
                continue;
            };
            let exists: Option<i64> = self
                .connection
                .query_row("SELECT 1 FROM books WHERE id=?1", [id.to_string()], |row| {
                    row.get(0)
                })
                .optional()?;
            if exists.is_none() {
                let destination = orphan_root.join(format!("{name}-{}", database::now()));
                fs::rename(entry.path(), &destination)
                    .map_err(|error| crate::error::io(&destination, error))?;
                report.quarantined_orphans += 1;
            }
        }
        Ok(report)
    }
}

fn system_cache_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join("Library/Caches/AproBook/txt");
    }
    #[cfg(target_os = "windows")]
    if let Some(cache) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(cache).join("AproBook/txt");
    }
    #[cfg(not(target_os = "windows"))]
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(cache).join("AproBook/txt");
    }
    std::env::temp_dir().join("AproBook/txt")
}

fn preflight_existing(root: &Path) -> Result<LibraryMarker> {
    if !root.is_dir() {
        return Err(CoreError::Validation(
            "library root is not a directory".into(),
        ));
    }
    let items = root.join("items");
    let marker_path = root.join(MARKER);
    let db_path = root.join(DATABASE);
    for path in [&items, &marker_path, &db_path] {
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            CoreError::Validation(format!("incomplete library directory: {}", path.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(CoreError::Validation(format!(
                "library component is a symlink: {}",
                path.display()
            )));
        }
        let correct_type = if path == &items {
            metadata.is_dir()
        } else {
            metadata.is_file()
        };
        if !correct_type {
            return Err(CoreError::Validation(format!(
                "invalid library component: {}",
                path.display()
            )));
        }
    }
    let marker_bytes =
        fs::read(&marker_path).map_err(|error| crate::error::io(&marker_path, error))?;
    let marker: LibraryMarker = serde_json::from_slice(&marker_bytes)
        .map_err(|error| CoreError::Validation(format!("invalid library marker: {error}")))?;
    if !(1..=SCHEMA_VERSION).contains(&marker.schema_version) {
        return Err(CoreError::Validation(format!(
            "unsupported library marker schema: {}",
            marker.schema_version
        )));
    }
    let db_version = database::validate_existing(&db_path)?;
    if db_version < marker.schema_version {
        return Err(CoreError::Validation(format!(
            "library database schema {db_version} predates marker schema {}",
            marker.schema_version
        )));
    }
    Ok(marker)
}

fn lock_library(root: &Path) -> Result<File> {
    let lock_path = root.join(LOCK);
    if let Ok(metadata) = fs::symlink_metadata(&lock_path)
        && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err(CoreError::Validation(format!(
            "invalid library lock file: {}",
            lock_path.display()
        )));
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| crate::error::io(&lock_path, error))?;
    FileExt::try_lock_exclusive(&lock).map_err(|_| CoreError::LibraryLocked(root.to_path_buf()))?;
    Ok(lock)
}

fn write_marker(path: &Path, marker: LibraryMarker) -> Result<()> {
    let temp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(&marker)?;
    let mut file = File::create(&temp).map_err(|error| crate::error::io(&temp, error))?;
    file.write_all(&data)
        .map_err(|error| crate::error::io(&temp, error))?;
    file.sync_all()
        .map_err(|error| crate::error::io(&temp, error))?;
    fs::rename(&temp, path).map_err(|error| crate::error::io(path, error))?;
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|error| crate::error::io(path, error))
    }
}

pub(crate) fn ensure_beneath(root: &Path, candidate: &Path) -> Result<()> {
    let root = lexical_normalize(root)?;
    let candidate = lexical_normalize(candidate)?;
    if candidate.starts_with(&root) {
        Ok(())
    } else {
        Err(CoreError::Validation(format!(
            "path escapes the managed library: {}",
            candidate.display()
        )))
    }
}

fn lexical_normalize(path: &Path) -> Result<PathBuf> {
    use std::path::Component;
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Prefix(prefix) => result.push(prefix.as_os_str()),
            Component::RootDir => result.push(std::path::MAIN_SEPARATOR_STR),
            Component::CurDir => {}
            Component::ParentDir => {
                if !result.pop() {
                    return Err(CoreError::Validation(format!(
                        "invalid path: {}",
                        path.display()
                    )));
                }
            }
            Component::Normal(value) => result.push(value),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn opening_an_arbitrary_directory_is_read_only_and_rejected() {
        let temp = tempdir().unwrap();
        let empty = temp.path().join("ordinary");
        fs::create_dir(&empty).unwrap();
        assert!(Library::open_existing(&empty).is_err());
        assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
        assert!(Library::open_existing(temp.path().join("missing")).is_err());
        assert!(!temp.path().join("missing").exists());

        let incomplete = temp.path().join("incomplete");
        fs::create_dir(&incomplete).unwrap();
        fs::write(incomplete.join(MARKER), b"invalid JSON").unwrap();
        let before = fs::read_dir(&incomplete).unwrap().count();
        assert!(Library::open_existing(&incomplete).is_err());
        assert_eq!(fs::read_dir(&incomplete).unwrap().count(), before);
        assert!(!incomplete.join(LOCK).exists());
    }

    #[test]
    fn opening_v2_migrates_with_backup_and_preserves_identity() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("library");
        let library = Library::create(&path).unwrap();
        let original: LibraryMarker =
            serde_json::from_slice(&fs::read(path.join(MARKER)).unwrap()).unwrap();
        drop(library);
        let connection = Connection::open(path.join(DATABASE)).unwrap();
        connection
            .execute_batch("DROP TABLE bookmarks; PRAGMA user_version = 2;")
            .unwrap();
        drop(connection);
        write_marker(
            &path.join(MARKER),
            LibraryMarker {
                library_id: original.library_id,
                schema_version: 2,
            },
        )
        .unwrap();

        let library = Library::open_existing(&path).unwrap();
        assert_eq!(library.schema_version().unwrap(), SCHEMA_VERSION);
        let marker: LibraryMarker =
            serde_json::from_slice(&fs::read(path.join(MARKER)).unwrap()).unwrap();
        assert_eq!(marker.library_id, original.library_id);
        assert_eq!(marker.schema_version, SCHEMA_VERSION);
        assert!(fs::read_dir(&path).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.sqlite3.backup-v2-")
        }));
    }

    #[test]
    fn opening_v1_migrates_through_all_versions() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("library");
        drop(Library::create(&path).unwrap());
        let marker_path = path.join(MARKER);
        let original: LibraryMarker =
            serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
        let connection = Connection::open(path.join(DATABASE)).unwrap();
        connection
            .execute_batch(
                "DROP TABLE bookmarks; DROP TABLE pending_removals; PRAGMA user_version = 1;",
            )
            .unwrap();
        drop(connection);
        write_marker(
            &marker_path,
            LibraryMarker {
                library_id: original.library_id,
                schema_version: 1,
            },
        )
        .unwrap();
        let library = Library::open_existing(&path).unwrap();
        assert_eq!(library.schema_version().unwrap(), SCHEMA_VERSION);
        assert!(fs::read_dir(&path).unwrap().flatten().any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("library.sqlite3.backup-v1-")
        }));
    }

    #[test]
    fn rejected_newer_or_incomplete_library_is_not_modified() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("library");
        drop(Library::create(&path).unwrap());
        let marker_path = path.join(MARKER);
        let mut marker: LibraryMarker =
            serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
        marker.schema_version = SCHEMA_VERSION + 1;
        write_marker(&marker_path, marker).unwrap();
        let marker_bytes = fs::read(&marker_path).unwrap();
        assert!(Library::open_existing(&path).is_err());
        assert_eq!(fs::read(&marker_path).unwrap(), marker_bytes);

        let mut marker: LibraryMarker = serde_json::from_slice(&marker_bytes).unwrap();
        marker.schema_version = SCHEMA_VERSION;
        write_marker(&marker_path, marker).unwrap();
        let connection = Connection::open(path.join(DATABASE)).unwrap();
        connection.execute_batch("DROP TABLE bookmarks;").unwrap();
        drop(connection);
        let db_bytes = fs::read(path.join(DATABASE)).unwrap();
        assert!(Library::open_existing(&path).is_err());
        assert_eq!(fs::read(path.join(DATABASE)).unwrap(), db_bytes);
    }
}
