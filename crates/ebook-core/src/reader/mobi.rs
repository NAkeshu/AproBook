use std::{collections::HashSet, path::Path};

use mobi::{Mobi, headers::Encryption};
use regex::{Captures, Regex};

use super::{
    MAX_EBOOK_BYTES, MAX_RESOURCE_BYTES, MAX_TEXT_BYTES, ReaderAdapter, ReaderContent,
    ReaderNavigation, ReaderResource, TocEntry, fallback_title,
};
use crate::{BookFormat, CoreError, ExtractedMetadata, PartialDate, ReaderLocator, Result};

pub struct MobiAdapter;

impl ReaderAdapter for MobiAdapter {
    fn format(&self) -> BookFormat {
        BookFormat::Mobi
    }

    fn inspect(&self, path: &Path) -> Result<ExtractedMetadata> {
        let mobi = parse(path)?;
        let content = mobi
            .content_as_string()
            .map_err(|error| CoreError::Reader(format!("MOBI text decode failed: {error}")))?;
        if content.len() > MAX_TEXT_BYTES {
            return Err(CoreError::Reader(format!(
                "MOBI text exceeds {MAX_TEXT_BYTES} byte limit"
            )));
        }
        let title = nonempty(mobi.title()).unwrap_or_else(|| fallback_title(path));
        let author = mobi.author().and_then(nonempty);
        let publisher = mobi.publisher().and_then(nonempty);
        let description = mobi.description().and_then(nonempty);
        let publish_date = mobi
            .publish_date()
            .and_then(|value| parse_date_prefix(&value));
        let cover = mobi
            .image_records()
            .into_iter()
            .find(|record| {
                record.content.len() as u64 <= MAX_RESOURCE_BYTES
                    && image::guess_format(record.content).is_ok()
            })
            .map(|record| record.content.to_vec());
        Ok(ExtractedMetadata {
            title,
            authors: author.into_iter().collect(),
            abstract_text: description,
            publication_date: publish_date,
            edition: None,
            publisher,
            cover,
            text_encoding: None,
            raw: serde_json::json!({
                "format": "mobi", "isbn": mobi.isbn(), "contributor": mobi.contributor(),
                "language": format!("{:?}", mobi.language()), "compression": format!("{:?}", mobi.compression()),
                "image_count": mobi.image_records().len(),
            }),
        })
    }

    fn navigation(&self, path: &Path) -> Result<ReaderNavigation> {
        let mobi = parse(path)?;
        let html = mobi
            .content_as_string()
            .map_err(|error| CoreError::Reader(format!("MOBI text decode failed: {error}")))?;
        if html.len() > MAX_TEXT_BYTES {
            return Err(CoreError::Reader(format!(
                "MOBI text exceeds {MAX_TEXT_BYTES} byte limit"
            )));
        }
        let headings = extract_headings(&html)?;
        let sections = vec![ReaderLocator::new(
            BookFormat::Mobi,
            serde_json::json!({ "section": 0 }),
        )];
        let toc = if headings.is_empty() {
            vec![TocEntry {
                label: "Book".into(),
                locator: ReaderLocator::new(BookFormat::Mobi, serde_json::json!({ "section": 0 })),
                depth: 0,
            }]
        } else {
            headings
            .into_iter()
            .map(|(heading_index, label, depth)| TocEntry {
                label,
                locator: ReaderLocator::new(
                    BookFormat::Mobi,
                    serde_json::json!({ "section": 0, "fragment": format!("theebook-mobi-heading-{heading_index}") }),
                ),
                depth,
            })
            .collect()
        };
        Ok(ReaderNavigation { sections, toc })
    }

    fn read(&self, path: &Path, locator: Option<&ReaderLocator>) -> Result<ReaderContent> {
        if let Some(locator) = locator {
            locator.validate_for(BookFormat::Mobi)?;
        }
        let mobi = parse(path)?;
        let html = mobi
            .content_as_string()
            .map_err(|error| CoreError::Reader(format!("MOBI text decode failed: {error}")))?;
        if html.len() > MAX_TEXT_BYTES {
            return Err(CoreError::Reader(format!(
                "MOBI text exceeds {MAX_TEXT_BYTES} byte limit"
            )));
        }
        let html = rewrite_images(&html, mobi.image_records().len())?;
        let html = annotate_headings(&html)?;
        let mut builder = ammonia::Builder::default();
        builder.url_schemes(HashSet::from(["ebook-resource"]));
        builder.add_generic_attributes(["id"]);
        Ok(ReaderContent::Html {
            body: builder.clean(&html).to_string(),
            base_resource: None,
        })
    }

    fn resource(&self, path: &Path, resource: &str) -> Result<ReaderResource> {
        let index = resource
            .strip_prefix("mobi-image/")
            .ok_or_else(|| CoreError::Reader("invalid MOBI resource name".into()))?
            .parse::<usize>()
            .map_err(|_| CoreError::Reader("invalid MOBI image index".into()))?;
        let mobi = parse(path)?;
        let images = mobi.image_records();
        let record = images.get(index).ok_or_else(|| {
            CoreError::Reader(format!("MOBI image index is out of range: {index}"))
        })?;
        if record.content.len() as u64 > MAX_RESOURCE_BYTES {
            return Err(CoreError::Reader(
                "MOBI image exceeds resource limit".into(),
            ));
        }
        let media_type = match image::guess_format(record.content) {
            Ok(image::ImageFormat::Png) => "image/png",
            Ok(image::ImageFormat::Jpeg) => "image/jpeg",
            Ok(image::ImageFormat::Gif) => "image/gif",
            Ok(image::ImageFormat::WebP) => "image/webp",
            _ => return Err(CoreError::Reader("unsupported MOBI image format".into())),
        };
        Ok(ReaderResource {
            media_type: media_type.into(),
            bytes: record.content.to_vec(),
        })
    }
}

fn parse(path: &Path) -> Result<Mobi> {
    let size = path
        .metadata()
        .map_err(|error| crate::error::io(path, error))?
        .len();
    if size > MAX_EBOOK_BYTES {
        return Err(CoreError::Reader(format!(
            "MOBI exceeds {MAX_EBOOK_BYTES} byte limit"
        )));
    }
    let mobi = Mobi::from_path(path)
        .map_err(|error| CoreError::Reader(format!("invalid MOBI: {error}")))?;
    if mobi.encryption() != Encryption::No {
        return Err(CoreError::Reader(
            "DRM/encrypted MOBI is not supported".into(),
        ));
    }
    Ok(mobi)
}

fn nonempty(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn parse_date_prefix(value: &str) -> Option<PartialDate> {
    [10, 7, 4]
        .into_iter()
        .filter(|length| value.len() >= *length)
        .find_map(|length| PartialDate::parse(&value[..length]).ok())
}

fn rewrite_images(html: &str, image_count: usize) -> Result<String> {
    let expression = Regex::new(r#"(?i)<img\b([^>]*?)\brecindex\s*=\s*['\"]?(\d+)['\"]?([^>]*)>"#)
        .map_err(|error| CoreError::Reader(error.to_string()))?;
    Ok(expression
        .replace_all(html, |capture: &Captures<'_>| {
            let record_index = capture[2].parse::<usize>().unwrap_or(0);
            if record_index == 0 || record_index > image_count {
                String::new()
            } else {
                format!(
                    "<img{} src=\"ebook-resource://book/mobi-image/{}\"{}>",
                    &capture[1],
                    record_index - 1,
                    &capture[3]
                )
            }
        })
        .into_owned())
}

fn heading_regex() -> Result<Regex> {
    Regex::new(r"(?is)<h([1-6])\b([^>]*)>(.*?)</h[1-6]>")
        .map_err(|error| CoreError::Reader(error.to_string()))
}

fn extract_headings(html: &str) -> Result<Vec<(usize, String, u8)>> {
    let expression = heading_regex()?;
    let tags = Regex::new(r"(?is)<[^>]+>").map_err(|error| CoreError::Reader(error.to_string()))?;
    Ok(expression
        .captures_iter(html)
        .enumerate()
        .take(2_000)
        .filter_map(|(index, capture)| {
            let value = tags.replace_all(&capture[3], "");
            let value = value.trim();
            (!value.is_empty()).then(|| {
                (
                    index,
                    value.chars().take(200).collect(),
                    capture[1].parse::<u8>().unwrap_or(1) - 1,
                )
            })
        })
        .collect())
}

fn annotate_headings(html: &str) -> Result<String> {
    let expression = heading_regex()?;
    let mut index = 0usize;
    Ok(expression
        .replace_all(html, |capture: &Captures<'_>| {
            let current = index;
            index += 1;
            if current >= 2_000 {
                return capture[0].to_owned();
            }
            format!(
                "<h{} id=\"theebook-mobi-heading-{current}\"{}>{}</h{}>",
                &capture[1], &capture[2], &capture[3], &capture[1]
            )
        })
        .into_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use tempfile::tempdir;

    use super::*;

    fn minimal_mobi(title: &str, html: &str) -> Vec<u8> {
        let record_zero_offset = 104_u32;
        let record_zero_len = 16 + 232 + title.len();
        let text_offset = record_zero_offset + record_zero_len as u32;
        let trailer_offset = text_offset + html.len() as u32;
        let mut bytes = vec![0_u8; record_zero_offset as usize];
        bytes[..title.len()].copy_from_slice(title.as_bytes());
        bytes[60..64].copy_from_slice(b"BOOK");
        bytes[64..68].copy_from_slice(b"MOBI");
        bytes[76..78].copy_from_slice(&3_u16.to_be_bytes());
        bytes[78..82].copy_from_slice(&record_zero_offset.to_be_bytes());
        bytes[86..90].copy_from_slice(&text_offset.to_be_bytes());
        bytes[94..98].copy_from_slice(&trailer_offset.to_be_bytes());

        let mut record_zero = vec![0_u8; record_zero_len];
        record_zero[0..2].copy_from_slice(&1_u16.to_be_bytes());
        record_zero[4..8].copy_from_slice(&(html.len() as u32).to_be_bytes());
        record_zero[8..10].copy_from_slice(&1_u16.to_be_bytes());
        record_zero[10..12].copy_from_slice(&4096_u16.to_be_bytes());
        record_zero[16..20].copy_from_slice(b"MOBI");
        record_zero[20..24].copy_from_slice(&232_u32.to_be_bytes());
        record_zero[24..28].copy_from_slice(&2_u32.to_be_bytes());
        record_zero[28..32].copy_from_slice(&65_001_u32.to_be_bytes());
        record_zero[80..84].copy_from_slice(&3_u32.to_be_bytes());
        record_zero[84..88].copy_from_slice(&248_u32.to_be_bytes());
        record_zero[88..92].copy_from_slice(&(title.len() as u32).to_be_bytes());
        record_zero[108..112].copy_from_slice(&2_u32.to_be_bytes());
        record_zero[192..194].copy_from_slice(&1_u16.to_be_bytes());
        record_zero[194..196].copy_from_slice(&1_u16.to_be_bytes());
        record_zero[248..].copy_from_slice(title.as_bytes());
        bytes.extend(record_zero);
        bytes.extend_from_slice(html.as_bytes());
        bytes.extend_from_slice(b"END");
        bytes
    }

    #[test]
    fn official_parser_reads_real_container_structure() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("real.mobi");
        fs::write(
            &path,
            minimal_mobi("Real Book", "<h1>Start</h1><p>Hello</p>"),
        )
        .unwrap();
        let adapter = MobiAdapter;
        let metadata = adapter.inspect(&path).unwrap();
        assert_eq!(metadata.title, "Real Book");
        let ReaderContent::Html { body, .. } = adapter.read(&path, None).unwrap() else {
            panic!()
        };
        assert!(body.contains("Hello"), "{body:?}");
        assert!(body.contains("id=\"theebook-mobi-heading-0\""), "{body:?}");
        let navigation = adapter.navigation(&path).unwrap();
        assert_eq!(navigation.sections.len(), 1);
        assert_eq!(navigation.toc[0].label, "Start");
        assert_eq!(
            navigation.toc[0].locator.location["fragment"],
            "theebook-mobi-heading-0"
        );
    }
    #[test]
    fn rewrites_only_valid_mobi_image_indices() {
        let html = rewrite_images(r#"<p><img recindex="1"><img recindex="3"></p>"#, 1).unwrap();
        assert!(html.contains("mobi-image/0"));
        assert!(!html.contains("mobi-image/2"));
    }
    #[test]
    fn extracts_bounded_heading_labels() {
        assert_eq!(
            extract_headings("<h1>One</h1><p>x</p><h2>Two</h2>").unwrap(),
            [(0, "One".into(), 0), (1, "Two".into(), 1)]
        );
        assert!(
            annotate_headings("<h2>Two</h2>")
                .unwrap()
                .contains("id=\"theebook-mobi-heading-0\"")
        );
    }
}
