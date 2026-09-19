use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("invalid data: {0}")]
    Validation(String),
    #[error("unsupported ebook format: {0}")]
    UnsupportedFormat(String),
    #[error("the library is already open by another process: {0}")]
    LibraryLocked(PathBuf),
    #[error("book was not found: {0}")]
    BookNotFound(String),
    #[error("duplicate ebook content; existing book is {existing_book_id}")]
    Duplicate { existing_book_id: String },
    #[error("reader error: {0}")]
    Reader(String),
    #[error("native PDF rendering is unavailable: {0}")]
    PdfRendererUnavailable(String),
    #[error("unsafe archive resource path: {0}")]
    UnsafeArchivePath(String),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("archive error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("image error: {0}")]
    Image(#[from] image::ImageError),
    #[error("PDF error: {0}")]
    Pdf(#[from] lopdf::Error),
}

pub type Result<T> = std::result::Result<T, CoreError>;

pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> CoreError {
    CoreError::Io {
        path: path.into(),
        source,
    }
}
