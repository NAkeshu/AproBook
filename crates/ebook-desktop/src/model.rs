use serde::{Deserialize, Serialize};
use std::{fmt, path::PathBuf, str::FromStr};

pub type BookId = String;

#[derive(Clone, Debug, PartialEq)]
pub enum BulkAction {
    SetFolder(Option<String>),
    AddTags(Vec<String>),
    RemoveTags(Vec<String>),
    SetFavorite(bool),
    SetRating(Option<u8>),
    SetReadingStatus(ReadingStatus),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BookFormat {
    #[default]
    Epub,
    Pdf,
    Mobi,
    Txt,
}

impl BookFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Epub => "EPUB",
            Self::Pdf => "PDF",
            Self::Mobi => "MOBI",
            Self::Txt => "TXT",
        }
    }
}

impl fmt::Display for BookFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingStatus {
    #[default]
    Unread,
    Reading,
    Finished,
}

impl ReadingStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unread => "未读",
            Self::Reading => "阅读中",
            Self::Finished => "已读完",
        }
    }
}

impl fmt::Display for ReadingStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for ReadingStatus {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "unread" => Ok(Self::Unread),
            "reading" => Ok(Self::Reading),
            "finished" => Ok(Self::Finished),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct FolderNode {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub book_count: usize,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct TagSummary {
    pub id: String,
    pub name: String,
    pub book_count: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BookView {
    pub id: BookId,
    pub title: String,
    pub authors: Vec<String>,
    pub abstract_text: String,
    pub publication_date: String,
    pub edition: String,
    pub publisher: String,
    pub format: BookFormat,
    pub text_encoding: Option<String>,
    pub favorite: bool,
    pub rating: Option<u8>,
    pub status: ReadingStatus,
    pub progress: f32,
    pub folder_id: Option<String>,
    pub folder_name: Option<String>,
    pub tags: Vec<TagSummary>,
    pub source_path: PathBuf,
    pub cover_path: Option<PathBuf>,
    pub added_at: String,
}

impl BookView {
    pub fn author_line(&self) -> String {
        if self.authors.is_empty() {
            "未知作者".to_string()
        } else {
            self.authors.join("、")
        }
    }

    pub fn progress_percent(&self) -> u8 {
        (self.progress.clamp(0.0, 1.0) * 100.0).round() as u8
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct BookPatch {
    pub title: String,
    pub authors: Vec<String>,
    pub abstract_text: String,
    pub publication_date: String,
    pub edition: String,
    pub publisher: String,
    pub favorite: bool,
    pub rating: Option<u8>,
    pub status: ReadingStatus,
}

impl From<&BookView> for BookPatch {
    fn from(book: &BookView) -> Self {
        Self {
            title: book.title.clone(),
            authors: book.authors.clone(),
            abstract_text: book.abstract_text.clone(),
            publication_date: book.publication_date.clone(),
            edition: book.edition.clone(),
            publisher: book.publisher.clone(),
            favorite: book.favorite,
            rating: book.rating,
            status: book.status,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LibrarySnapshot {
    pub root: PathBuf,
    pub books: Vec<BookView>,
    pub folders: Vec<FolderNode>,
    pub tags: Vec<TagSummary>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum SidebarFilter {
    #[default]
    All,
    Favorite,
    Status(ReadingStatus),
    Folder(String),
    Tag(String),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortMode {
    #[default]
    AddedDesc,
    TitleAsc,
    AuthorAsc,
    PublicationDesc,
    RatingDesc,
    ProgressDesc,
}

impl SortMode {
    pub fn from_value(value: &str) -> Self {
        match value {
            "title" => Self::TitleAsc,
            "author" => Self::AuthorAsc,
            "publication" => Self::PublicationDesc,
            "rating" => Self::RatingDesc,
            "progress" => Self::ProgressDesc,
            _ => Self::AddedDesc,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct QueryState {
    pub search: String,
    pub sidebar: SidebarFilter,
    pub format: Option<BookFormat>,
    pub status: Option<ReadingStatus>,
    pub minimum_rating: Option<u8>,
    pub sort: SortMode,
}

impl QueryState {
    pub fn apply(&self, books: &[BookView]) -> Vec<BookView> {
        let needle = self.search.trim().to_lowercase();
        let mut result: Vec<_> = books
            .iter()
            .filter(|book| {
                needle.is_empty()
                    || book.title.to_lowercase().contains(&needle)
                    || book.author_line().to_lowercase().contains(&needle)
                    || book.publisher.to_lowercase().contains(&needle)
                    || book
                        .tags
                        .iter()
                        .any(|tag| tag.name.to_lowercase().contains(&needle))
            })
            .filter(|book| match &self.sidebar {
                SidebarFilter::All => true,
                SidebarFilter::Favorite => book.favorite,
                SidebarFilter::Status(status) => book.status == *status,
                SidebarFilter::Folder(id) => book.folder_id.as_deref() == Some(id),
                SidebarFilter::Tag(id) => book.tags.iter().any(|tag| tag.id == *id),
            })
            .filter(|book| self.format.is_none_or(|format| book.format == format))
            .filter(|book| self.status.is_none_or(|status| book.status == status))
            .filter(|book| {
                self.minimum_rating
                    .is_none_or(|rating| book.rating.unwrap_or(0) >= rating)
            })
            .cloned()
            .collect();

        result.sort_by(|a, b| match self.sort {
            SortMode::AddedDesc => b.added_at.cmp(&a.added_at),
            SortMode::TitleAsc => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            SortMode::AuthorAsc => a
                .author_line()
                .to_lowercase()
                .cmp(&b.author_line().to_lowercase()),
            SortMode::PublicationDesc => b.publication_date.cmp(&a.publication_date),
            SortMode::RatingDesc => b.rating.unwrap_or(0).cmp(&a.rating.unwrap_or(0)),
            SortMode::ProgressDesc => b.progress.total_cmp(&a.progress),
        });
        result
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum ReaderContent {
    Html {
        title: String,
        body: String,
        format: BookFormat,
        chapter_index: usize,
        chapter_count: usize,
        toc: Vec<ReaderTocItem>,
        scroll_fraction: f32,
    },
    Pdf {
        title: String,
        page: usize,
        page_count: usize,
        toc: Vec<ReaderTocItem>,
        rendered_page: Option<String>,
        image_width: u32,
        image_height: u32,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReaderTocItem {
    pub label: String,
    pub depth: u8,
    /// Zero-based reading section or PDF page.
    pub section: usize,
    pub fragment: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct BookmarkView {
    pub id: String,
    pub label: String,
    pub position_label: String,
    pub section: usize,
    pub fraction: f32,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ReaderOptions {
    pub font_size: u8,
    pub line_height: f32,
    pub margin: u16,
    pub zoom: f32,
    pub continuous: bool,
    pub theme: ReaderTheme,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderTheme {
    #[default]
    Light,
    Dark,
    Sepia,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ReaderLocator {
    pub version: u8,
    pub section: usize,
    /// Overall book progress shown on the shelf.
    pub offset: f32,
    /// Position within the current HTML document.
    pub section_fraction: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_matches_tags_and_sorts_rating() {
        let mut low = sample_book("甲", 2);
        low.tags.push(TagSummary {
            id: "rust".into(),
            name: "Rust".into(),
            book_count: 1,
        });
        let high = sample_book("乙", 5);
        let query = QueryState {
            search: "rust".into(),
            sort: SortMode::RatingDesc,
            ..Default::default()
        };
        assert_eq!(query.apply(&[high, low])[0].title, "甲");
    }

    #[test]
    fn progress_is_clamped() {
        let mut book = sample_book("测试", 3);
        book.progress = 1.5;
        assert_eq!(book.progress_percent(), 100);
    }

    fn sample_book(title: &str, rating: u8) -> BookView {
        BookView {
            id: title.into(),
            title: title.into(),
            authors: vec!["作者".into()],
            abstract_text: String::new(),
            publication_date: String::new(),
            edition: String::new(),
            publisher: String::new(),
            format: BookFormat::Epub,
            text_encoding: None,
            favorite: false,
            rating: Some(rating),
            status: ReadingStatus::Unread,
            progress: 0.0,
            folder_id: None,
            folder_name: None,
            tags: vec![],
            source_path: PathBuf::new(),
            cover_path: None,
            added_at: String::new(),
        }
    }
}
