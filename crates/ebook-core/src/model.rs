use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CoreError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BookFormat {
    Epub,
    Pdf,
    Mobi,
    Txt,
}

impl BookFormat {
    pub fn from_extension(extension: &str) -> Result<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "epub" => Ok(Self::Epub),
            "pdf" => Ok(Self::Pdf),
            "mobi" => Ok(Self::Mobi),
            "txt" => Ok(Self::Txt),
            other => Err(CoreError::UnsupportedFormat(other.to_owned())),
        }
    }

    pub const fn extension(self) -> &'static str {
        match self {
            Self::Epub => "epub",
            Self::Pdf => "pdf",
            Self::Mobi => "mobi",
            Self::Txt => "txt",
        }
    }
}

impl fmt::Display for BookFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.extension())
    }
}

impl FromStr for BookFormat {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self> {
        Self::from_extension(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReadingStatus {
    #[default]
    Unread,
    Reading,
    Finished,
}

impl fmt::Display for ReadingStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unread => "unread",
            Self::Reading => "reading",
            Self::Finished => "finished",
        })
    }
}

impl FromStr for ReadingStatus {
    type Err = CoreError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "unread" => Ok(Self::Unread),
            "reading" => Ok(Self::Reading),
            "finished" => Ok(Self::Finished),
            _ => Err(CoreError::Validation(format!(
                "invalid reading status: {s}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Rating(u8);

impl Rating {
    pub fn new(value: u8) -> Result<Self> {
        if (1..=5).contains(&value) {
            Ok(Self(value))
        } else {
            Err(CoreError::Validation("rating must be from 1 to 5".into()))
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Rating {
    type Error = CoreError;
    fn try_from(value: u8) -> Result<Self> {
        Self::new(value)
    }
}

impl From<Rating> for u8 {
    fn from(value: Rating) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartialDate(String);

impl PartialDate {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let parts: Vec<_> = value.split('-').collect();
        let valid = match parts.as_slice() {
            [year] => valid_year(year),
            [year, month] => valid_year(year) && number_in(month, 1, 12, 2),
            [year, month, day] => {
                valid_year(year)
                    && number_in(month, 1, 12, 2)
                    && number_in(day, 1, days_in_month(year, month), 2)
            }
            _ => false,
        };
        if valid {
            Ok(Self(value))
        } else {
            Err(CoreError::Validation(format!(
                "invalid partial date (expected YYYY, YYYY-MM, or YYYY-MM-DD): {value}"
            )))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn valid_year(value: &str) -> bool {
    value.len() == 4 && value.parse::<u16>().is_ok_and(|year| year > 0)
}

fn number_in(value: &str, min: u8, max: u8, width: usize) -> bool {
    value.len() == width && value.parse::<u8>().is_ok_and(|v| (min..=max).contains(&v))
}

fn days_in_month(year: &str, month: &str) -> u8 {
    let year = year.parse::<u16>().unwrap_or(1);
    match month.parse::<u8>().unwrap_or(0) {
        4 | 6 | 9 | 11 => 30,
        2 if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) => 29,
        2 => 28,
        _ => 31,
    }
}

impl fmt::Display for PartialDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReaderLocator {
    pub version: u32,
    pub format: BookFormat,
    pub location: serde_json::Value,
}

impl ReaderLocator {
    pub fn new(format: BookFormat, location: serde_json::Value) -> Self {
        Self {
            version: 1,
            format,
            location,
        }
    }

    pub fn validate_for(&self, format: BookFormat) -> Result<()> {
        if self.version != 1 {
            return Err(CoreError::Validation(format!(
                "unsupported reader locator version: {}",
                self.version
            )));
        }
        if self.format != format {
            return Err(CoreError::Validation(
                "reader locator format does not match book".into(),
            ));
        }
        Ok(())
    }

    /// Bookmarks require a concrete, safe target; generic reading progress may
    /// also use an empty locator while a book is first being opened.
    pub fn validate_bookmark_for(&self, format: BookFormat) -> Result<()> {
        self.validate_for(format)?;
        let valid = match format {
            BookFormat::Pdf => self
                .location
                .get("page")
                .and_then(|v| v.as_u64())
                .is_some_and(|v| v > 0),
            BookFormat::Epub => self
                .location
                .get("resource")
                .and_then(|v| v.as_str())
                .is_some_and(|v| {
                    !v.is_empty()
                        && !v.starts_with('/')
                        && !v.contains('\\')
                        && !v.contains(':')
                        && v.split('/').all(|part| !matches!(part, "" | "." | ".."))
                }),
            BookFormat::Mobi => self
                .location
                .get("section")
                .and_then(|v| v.as_u64())
                .is_some(),
            BookFormat::Txt => self
                .location
                .get("section")
                .and_then(|v| v.as_u64())
                .is_some(),
        };
        let fraction_valid = self.location.get("scroll_fraction").is_none_or(|value| {
            value
                .as_f64()
                .is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v))
        });
        if !valid || !fraction_valid {
            return Err(CoreError::Validation(
                "invalid bookmark locator for book format".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: Uuid,
    pub book_id: Uuid,
    pub label: String,
    pub locator: ReaderLocator,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub enum BulkBookAction {
    SetFolder(Option<Uuid>),
    AddTags(Vec<String>),
    RemoveTags(Vec<String>),
    SetFavorite(bool),
    SetRating(Option<Rating>),
    SetReadingStatus(ReadingStatus),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Book {
    pub id: Uuid,
    pub title: String,
    pub authors: Vec<String>,
    pub abstract_text: Option<String>,
    pub publication_date: Option<PartialDate>,
    pub edition: Option<String>,
    pub publisher: Option<String>,
    pub format: BookFormat,
    /// Canonical encoding name for TXT; absent for other formats.
    pub text_encoding: Option<String>,
    pub tags: Vec<String>,
    pub folder_id: Option<Uuid>,
    pub favorite: bool,
    pub rating: Option<Rating>,
    pub reading_status: ReadingStatus,
    pub progress: f64,
    pub reader_locator: Option<ReaderLocator>,
    pub original_path: String,
    pub managed_path: String,
    pub cover_path: Option<String>,
    pub sha256: String,
    pub raw_metadata: serde_json::Value,
    pub imported_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default)]
pub struct BookPatch {
    pub title: Option<String>,
    pub authors: Option<Vec<String>>,
    pub abstract_text: Option<Option<String>>,
    pub publication_date: Option<Option<PartialDate>>,
    pub edition: Option<Option<String>>,
    pub publisher: Option<Option<String>>,
    pub tags: Option<Vec<String>>,
    pub folder_id: Option<Option<Uuid>>,
    pub favorite: Option<bool>,
    pub rating: Option<Option<Rating>>,
}

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    pub folder_id: Option<Uuid>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedMetadata {
    pub title: String,
    pub authors: Vec<String>,
    pub abstract_text: Option<String>,
    pub publication_date: Option<PartialDate>,
    pub edition: Option<String>,
    pub publisher: Option<String>,
    pub cover: Option<Vec<u8>>,
    pub text_encoding: Option<String>,
    pub raw: serde_json::Value,
}

impl ExtractedMetadata {
    pub fn fallback(title: String) -> Self {
        Self {
            title,
            authors: Vec::new(),
            abstract_text: None,
            publication_date: None,
            edition: None,
            publisher: None,
            cover: None,
            text_encoding: None,
            raw: serde_json::json!({}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_rating() {
        assert!(Rating::new(1).is_ok());
        assert!(Rating::new(5).is_ok());
        assert!(Rating::new(0).is_err());
        assert!(Rating::new(6).is_err());
    }

    #[test]
    fn validates_partial_dates_and_leap_years() {
        for date in ["2026", "2026-09", "2024-02-29"] {
            assert!(PartialDate::parse(date).is_ok(), "{date}");
        }
        for date in ["0", "2026-13", "2023-02-29", "2026-09-31", "2026/09"] {
            assert!(PartialDate::parse(date).is_err(), "{date}");
        }
    }

    #[test]
    fn validates_locator_version_and_format() {
        let valid = ReaderLocator::new(BookFormat::Epub, serde_json::json!({"resource":"c"}));
        assert!(valid.validate_for(BookFormat::Epub).is_ok());
        assert!(valid.validate_for(BookFormat::Pdf).is_err());
        let mut future = valid;
        future.version = 2;
        assert!(future.validate_for(BookFormat::Epub).is_err());
    }
}
