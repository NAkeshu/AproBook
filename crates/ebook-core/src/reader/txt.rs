use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use chardetng::EncodingDetector;
use encoding_rs::{BIG5, CoderResult, Encoding, GBK, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    MAX_EBOOK_BYTES, ReaderAdapter, ReaderContent, ReaderNavigation, ReaderOptions, TocEntry,
    fallback_title,
};
use crate::{BookFormat, CoreError, ExtractedMetadata, ReaderLocator, Result};

pub struct TxtAdapter;

pub const TXT_ENCODING_OPTIONS: &[&str] = &[
    "utf-8",
    "utf-16le",
    "utf-16be",
    "gb18030",
    "big5",
    "shift_jis",
];

const SECTION_BYTES: u64 = 256 * 1024;
const INDEX_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Section {
    start: u64,
    end: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedIndex {
    version: u32,
    encoding: String,
    source_len: u64,
    source_mtime_nanos: u128,
    text_len: u64,
    sections: Vec<Section>,
    toc: Vec<CachedHeading>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedHeading {
    label: String,
    section: usize,
}

struct CachePaths {
    text: PathBuf,
    index: PathBuf,
}

impl ReaderAdapter for TxtAdapter {
    fn format(&self) -> BookFormat {
        BookFormat::Txt
    }

    fn inspect(&self, path: &Path) -> Result<ExtractedMetadata> {
        self.inspect_with_options(path, &ReaderOptions::default())
    }

    fn inspect_with_options(
        &self,
        path: &Path,
        options: &ReaderOptions,
    ) -> Result<ExtractedMetadata> {
        let encoding = select_encoding(path, options.text_encoding.as_deref())?;
        decode_stream(path, encoding, std::io::sink())?;
        Ok(ExtractedMetadata {
            title: fallback_title(path),
            authors: Vec::new(),
            abstract_text: None,
            publication_date: None,
            edition: None,
            publisher: None,
            cover: None,
            text_encoding: Some(encoding.to_owned()),
            raw: serde_json::json!({"format":"txt", "detected_encoding":encoding}),
        })
    }

    fn navigation(&self, path: &Path) -> Result<ReaderNavigation> {
        self.navigation_with_options(path, &ReaderOptions::default())
    }

    fn read(&self, path: &Path, locator: Option<&ReaderLocator>) -> Result<ReaderContent> {
        self.read_with_options(path, locator, &ReaderOptions::default())
    }

    fn navigation_with_options(
        &self,
        path: &Path,
        options: &ReaderOptions,
    ) -> Result<ReaderNavigation> {
        let (index, _) = ensure_cache(path, options)?;
        Ok(ReaderNavigation {
            sections: (0..index.sections.len()).map(section_locator).collect(),
            toc: if index.toc.is_empty() {
                (0..index.sections.len())
                    .map(|section| TocEntry {
                        label: format!("第 {} 节", section + 1),
                        locator: section_locator(section),
                        depth: 0,
                    })
                    .collect()
            } else {
                index
                    .toc
                    .iter()
                    .map(|heading| TocEntry {
                        label: heading.label.clone(),
                        locator: section_locator(heading.section),
                        depth: 0,
                    })
                    .collect()
            },
        })
    }

    fn read_with_options(
        &self,
        path: &Path,
        locator: Option<&ReaderLocator>,
        options: &ReaderOptions,
    ) -> Result<ReaderContent> {
        let (index, cache) = ensure_cache(path, options)?;
        let number = if let Some(locator) = locator {
            locator.validate_for(BookFormat::Txt)?;
            locator
                .location
                .get("section")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize
        } else {
            0
        };
        let section = index
            .sections
            .get(number)
            .ok_or_else(|| CoreError::Reader(format!("TXT section is out of range: {number}")))?;
        let mut file = File::open(&cache.text).map_err(|e| crate::error::io(&cache.text, e))?;
        file.seek(SeekFrom::Start(section.start))
            .map_err(|e| crate::error::io(&cache.text, e))?;
        let mut bytes = Vec::with_capacity((section.end - section.start) as usize);
        file.take(section.end - section.start)
            .read_to_end(&mut bytes)
            .map_err(|e| crate::error::io(&cache.text, e))?;
        let body = std::str::from_utf8(&bytes)
            .map_err(|e| CoreError::Reader(format!("TXT cache is invalid UTF-8: {e}")))?;
        Ok(ReaderContent::Html {
            body: format!(
                "<div style=\"white-space:pre-wrap;overflow-wrap:anywhere\">{}</div>",
                escape_html(body)
            ),
            base_resource: None,
        })
    }
}

fn section_locator(section: usize) -> ReaderLocator {
    ReaderLocator::new(BookFormat::Txt, serde_json::json!({"section":section}))
}

/// Returns the canonical stored name; aliases are intentionally limited to the
/// six encodings exposed in the GUI, avoiding surprising browser encodings.
pub fn canonical_encoding(label: &str) -> Result<&'static str> {
    let normalized = label.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "utf-8" | "utf8" => Ok("utf-8"),
        "utf-16le" | "utf16le" => Ok("utf-16le"),
        "utf-16be" | "utf16be" => Ok("utf-16be"),
        "gb18030" | "gbk" => Ok("gb18030"),
        "big5" => Ok("big5"),
        "shift-jis" | "shiftjis" | "sjis" => Ok("shift_jis"),
        _ => Err(CoreError::Validation(format!(
            "unsupported TXT encoding: {label}"
        ))),
    }
}

fn encoding_for(label: &str) -> &'static Encoding {
    match label {
        "utf-8" => UTF_8,
        "utf-16le" => UTF_16LE,
        "utf-16be" => UTF_16BE,
        "gb18030" => GBK,
        "big5" => BIG5,
        "shift_jis" => SHIFT_JIS,
        _ => unreachable!("canonical encoding"),
    }
}

fn select_encoding(path: &Path, override_encoding: Option<&str>) -> Result<&'static str> {
    let metadata = fs::metadata(path).map_err(|e| crate::error::io(path, e))?;
    if metadata.len() == 0 || metadata.len() > MAX_EBOOK_BYTES {
        return Err(CoreError::Validation(
            "TXT file is empty or exceeds the import limit".into(),
        ));
    }
    if let Some(encoding) = override_encoding {
        return canonical_encoding(encoding);
    }
    let mut input = File::open(path).map_err(|e| crate::error::io(path, e))?;
    let mut sample = vec![0; 128 * 1024];
    let size = input
        .read(&mut sample)
        .map_err(|e| crate::error::io(path, e))?;
    sample.truncate(size);
    if sample.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Ok("utf-8");
    }
    if sample.starts_with(&[0xff, 0xfe]) {
        return Ok("utf-16le");
    }
    if sample.starts_with(&[0xfe, 0xff]) {
        return Ok("utf-16be");
    }
    if !sample.contains(&0) {
        match std::str::from_utf8(&sample) {
            Ok(_) => return Ok("utf-8"),
            Err(error)
                if error.error_len().is_none() && error.valid_up_to() + 4 >= sample.len() =>
            {
                return Ok("utf-8");
            }
            _ => {}
        }
    }
    if sample.len() >= 8 {
        let pairs = sample.len() / 2;
        let odd = sample
            .iter()
            .skip(1)
            .step_by(2)
            .filter(|b| **b == 0)
            .count();
        let even = sample.iter().step_by(2).filter(|b| **b == 0).count();
        if odd > pairs / 3 && even < pairs / 20 {
            return Ok("utf-16le");
        }
        if even > pairs / 3 && odd < pairs / 20 {
            return Ok("utf-16be");
        }
    }
    if sample.contains(&0) {
        return Err(CoreError::Validation(
            "TXT appears to contain binary data".into(),
        ));
    }
    let mut detector = EncodingDetector::new();
    detector.feed(&sample, true);
    match detector.guess(None, false).name() {
        "GBK" => Ok("gb18030"),
        "Big5" => Ok("big5"),
        "Shift_JIS" => Ok("shift_jis"),
        other => Err(CoreError::Validation(format!(
            "cannot confidently detect TXT encoding ({other}); choose an encoding and retry"
        ))),
    }
}

fn decode_stream(path: &Path, encoding: &str, output: impl Write) -> Result<u64> {
    let mut input = BufReader::new(File::open(path).map_err(|e| crate::error::io(path, e))?);
    let mut output = BufWriter::new(output);
    let mut decoder = encoding_for(encoding).new_decoder_with_bom_removal();
    let mut source = [0u8; 64 * 1024];
    let mut decoded = [0u8; 256 * 1024];
    let mut written_total = 0u64;
    let mut controls = 0u64;
    loop {
        let amount = input
            .read(&mut source)
            .map_err(|e| crate::error::io(path, e))?;
        let last = amount == 0;
        let mut consumed = 0;
        loop {
            let (result, read, written, malformed) =
                decoder.decode_to_utf8(&source[consumed..amount], &mut decoded, last);
            if malformed {
                return Err(CoreError::Validation(format!(
                    "TXT cannot be decoded as {encoding}"
                )));
            }
            consumed += read;
            let chunk = &decoded[..written];
            if chunk.contains(&0) {
                return Err(CoreError::Validation("TXT contains NUL/binary data".into()));
            }
            controls += chunk
                .iter()
                .filter(|b| **b < 0x20 && !matches!(**b, b'\n' | b'\r' | b'\t' | 0x0c))
                .count() as u64;
            written_total += written as u64;
            if controls > 64 && controls * 100 > written_total {
                return Err(CoreError::Validation(
                    "TXT contains excessive control characters".into(),
                ));
            }
            output
                .write_all(chunk)
                .map_err(|e| crate::error::io(path, e))?;
            if result == CoderResult::InputEmpty {
                break;
            }
        }
        if last {
            break;
        }
    }
    output.flush().map_err(|e| crate::error::io(path, e))?;
    Ok(written_total)
}

fn ensure_cache(path: &Path, options: &ReaderOptions) -> Result<(CachedIndex, CachePaths)> {
    let encoding = select_encoding(path, options.text_encoding.as_deref())?;
    let metadata = fs::metadata(path).map_err(|e| crate::error::io(path, e))?;
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|v| v.duration_since(UNIX_EPOCH).ok())
        .map(|v| v.as_nanos())
        .unwrap_or(0);
    let hash = match options.sha256.as_deref() {
        Some(hash) if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
            hash.to_owned()
        }
        _ => hash_source(path)?,
    };
    let directory = options
        .cache_dir
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("AproBook/txt"));
    fs::create_dir_all(&directory).map_err(|e| crate::error::io(&directory, e))?;
    let stem = format!("{hash}-{encoding}-v{INDEX_VERSION}");
    let paths = CachePaths {
        text: directory.join(format!("{stem}.utf8")),
        index: directory.join(format!("{stem}.json")),
    };
    if let Ok(bytes) = fs::read(&paths.index)
        && let Ok(index) = serde_json::from_slice::<CachedIndex>(&bytes)
        && index.version == INDEX_VERSION
        && index.encoding == encoding
        && index.source_len == metadata.len()
        && index.source_mtime_nanos == mtime
        && !index.sections.is_empty()
        && fs::metadata(&paths.text).is_ok_and(|meta| meta.len() == index.text_len)
    {
        return Ok((index, paths));
    }
    let temporary_text = directory.join(format!("{stem}.{}.tmp", Uuid::new_v4()));
    let temporary_index = directory.join(format!("{stem}.{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<CachedIndex> {
        let output =
            File::create(&temporary_text).map_err(|e| crate::error::io(&temporary_text, e))?;
        let text_len = decode_stream(path, encoding, output)?;
        let (sections, toc) = index_text(&temporary_text, text_len)?;
        let index = CachedIndex {
            version: INDEX_VERSION,
            encoding: encoding.to_owned(),
            source_len: metadata.len(),
            source_mtime_nanos: mtime,
            text_len,
            sections,
            toc,
        };
        let mut output =
            File::create(&temporary_index).map_err(|e| crate::error::io(&temporary_index, e))?;
        serde_json::to_writer(&mut output, &index)?;
        output
            .sync_all()
            .map_err(|e| crate::error::io(&temporary_index, e))?;
        fs::rename(&temporary_text, &paths.text).map_err(|e| crate::error::io(&paths.text, e))?;
        fs::rename(&temporary_index, &paths.index)
            .map_err(|e| crate::error::io(&paths.index, e))?;
        Ok(index)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_text);
        let _ = fs::remove_file(&temporary_index);
    }
    result.map(|index| (index, paths))
}

fn hash_source(path: &Path) -> Result<String> {
    let mut input = BufReader::new(File::open(path).map_err(|e| crate::error::io(path, e))?);
    let mut digest = Sha256::new();
    std::io::copy(&mut input, &mut digest_writer(&mut digest))
        .map_err(|e| crate::error::io(path, e))?;
    Ok(format!("{:x}", digest.finalize()))
}

fn digest_writer<'a>(digest: &'a mut Sha256) -> impl Write + 'a {
    struct DigestWriter<'a>(&'a mut Sha256);
    impl Write for DigestWriter<'_> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.update(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    DigestWriter(digest)
}

fn index_text(path: &Path, text_len: u64) -> Result<(Vec<Section>, Vec<CachedHeading>)> {
    let mut input = BufReader::new(File::open(path).map_err(|e| crate::error::io(path, e))?);
    let chinese =
        Regex::new(r"^第[零〇一二三四五六七八九十百千万两0-9]+[章节卷部篇回](?:\s|[:：]|$)")
            .expect("constant chapter pattern");
    let english = Regex::new(r"(?i)^chapter\s+(?:\d+|[ivxlcdm]+)(?:\s|[:.]|$)")
        .expect("constant chapter pattern");
    let mut starts = vec![0u64];
    let mut headings = Vec::<(u64, String)>::new();
    let mut offset = 0u64;
    let mut line_start = 0u64;
    let mut prefix = Vec::<u8>::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let len = input
            .read(&mut buffer)
            .map_err(|e| crate::error::io(path, e))?;
        if len == 0 {
            break;
        }
        for &byte in &buffer[..len] {
            if offset - starts.last().copied().unwrap_or(0) >= SECTION_BYTES && byte & 0xc0 != 0x80
            {
                starts.push(offset);
            }
            if prefix.len() < 256 {
                prefix.push(byte);
            }
            offset += 1;
            if byte == b'\n' {
                if let Ok(line) = std::str::from_utf8(&prefix) {
                    let title = line.trim();
                    if title.chars().count() <= 120
                        && (chinese.is_match(title) || english.is_match(title))
                    {
                        if line_start > *starts.last().unwrap() {
                            starts.push(line_start);
                        }
                        headings.push((line_start, title.to_owned()));
                    }
                }
                line_start = offset;
                prefix.clear();
            }
        }
    }
    if !prefix.is_empty() {
        if let Ok(line) = std::str::from_utf8(&prefix) {
            let title = line.trim();
            if title.chars().count() <= 120 && (chinese.is_match(title) || english.is_match(title))
            {
                if line_start > *starts.last().unwrap() {
                    starts.push(line_start);
                }
                headings.push((line_start, title.to_owned()));
            }
        }
    }
    starts.retain(|start| *start < text_len);
    if starts.is_empty() {
        starts.push(0);
    }
    let sections = starts
        .iter()
        .enumerate()
        .map(|(i, start)| Section {
            start: *start,
            end: starts.get(i + 1).copied().unwrap_or(text_len),
        })
        .collect::<Vec<_>>();
    let toc = headings
        .into_iter()
        .map(|(offset, label)| CachedHeading {
            label,
            section: starts
                .partition_point(|start| *start <= offset)
                .saturating_sub(1),
        })
        .collect();
    Ok((sections, toc))
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detects_utf8_sections_and_escapes_markup() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("小说.txt");
        fs::write(&path, "前言\n第一章 开始\n<&script>你好\n第二章 结尾\n完").unwrap();
        let adapter = TxtAdapter;
        let options = ReaderOptions {
            cache_dir: Some(dir.path().join("cache")),
            ..Default::default()
        };
        let metadata = adapter.inspect(&path).unwrap();
        assert_eq!(metadata.title, "小说");
        assert_eq!(metadata.text_encoding.as_deref(), Some("utf-8"));
        let nav = adapter.navigation_with_options(&path, &options).unwrap();
        assert_eq!(nav.toc.len(), 2);
        let body = adapter
            .read_with_options(&path, Some(&nav.toc[0].locator), &options)
            .unwrap();
        let ReaderContent::Html { body, .. } = body else {
            panic!("expected HTML")
        };
        assert!(body.contains("&lt;&amp;script&gt;"));
        assert!(!body.contains("<script>"));
    }

    #[test]
    fn bounds_long_unbroken_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("long.txt");
        fs::write(&path, "字".repeat(300_000)).unwrap();
        let options = ReaderOptions {
            cache_dir: Some(dir.path().join("cache")),
            ..Default::default()
        };
        let nav = TxtAdapter.navigation_with_options(&path, &options).unwrap();
        assert!(nav.sections.len() >= 3);
        let (index, _) = ensure_cache(&path, &options).unwrap();
        assert!(
            index
                .sections
                .iter()
                .all(|s| s.end - s.start <= SECTION_BYTES + 3)
        );
    }

    #[test]
    fn rejects_binary_and_invalid_manual_decode() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.txt");
        fs::write(&path, [0u8, 1, 2, 3, 4]).unwrap();
        assert!(TxtAdapter.inspect(&path).is_err());
        fs::write(&path, [0x81u8]).unwrap();
        let options = ReaderOptions {
            text_encoding: Some("utf-8".into()),
            ..Default::default()
        };
        assert!(TxtAdapter.inspect_with_options(&path, &options).is_err());
    }

    #[test]
    fn supports_bom_utf16_and_legacy_cjk_encodings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("encoded.txt");
        let text = "Chapter 1\n你好，世界。\nChapter 2\n第二段。";
        for (label, encoder) in [("gb18030", GBK), ("big5", BIG5)] {
            let (encoded, _, malformed) = encoder.encode(text);
            assert!(!malformed);
            fs::write(&path, encoded.as_ref()).unwrap();
            let options = ReaderOptions {
                text_encoding: Some(label.into()),
                cache_dir: Some(dir.path().join("cache")),
                ..Default::default()
            };
            assert_eq!(
                TxtAdapter
                    .inspect_with_options(&path, &options)
                    .unwrap()
                    .text_encoding
                    .as_deref(),
                Some(label)
            );
            let nav = TxtAdapter.navigation_with_options(&path, &options).unwrap();
            assert_eq!(nav.toc.len(), 2);
            let ReaderContent::Html { body, .. } = TxtAdapter
                .read_with_options(&path, Some(&nav.sections[0]), &options)
                .unwrap()
            else {
                panic!()
            };
            assert!(body.contains("你好，世界。"));
        }
        let japanese = "Chapter 1\nこんにちは世界。\nChapter 2\n続きです。";
        let (encoded, _, malformed) = SHIFT_JIS.encode(japanese);
        assert!(!malformed);
        fs::write(&path, encoded.as_ref()).unwrap();
        let options = ReaderOptions {
            text_encoding: Some("shift_jis".into()),
            cache_dir: Some(dir.path().join("cache")),
            ..Default::default()
        };
        assert_eq!(
            TxtAdapter
                .inspect_with_options(&path, &options)
                .unwrap()
                .text_encoding
                .as_deref(),
            Some("shift_jis")
        );
        let ReaderContent::Html { body, .. } =
            TxtAdapter.read_with_options(&path, None, &options).unwrap()
        else {
            panic!()
        };
        assert!(body.contains("こんにちは世界。"));

        for (label, little) in [("utf-16le", true), ("utf-16be", false)] {
            let mut encoded = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for code in text.encode_utf16() {
                let bytes = if little {
                    code.to_le_bytes()
                } else {
                    code.to_be_bytes()
                };
                encoded.extend_from_slice(&bytes);
            }
            fs::write(&path, encoded).unwrap();
            assert_eq!(
                TxtAdapter.inspect(&path).unwrap().text_encoding.as_deref(),
                Some(label)
            );
            let options = ReaderOptions {
                cache_dir: Some(dir.path().join("cache")),
                ..Default::default()
            };
            assert_eq!(
                TxtAdapter
                    .navigation_with_options(&path, &options)
                    .unwrap()
                    .toc
                    .len(),
                2
            );
        }
    }
}
