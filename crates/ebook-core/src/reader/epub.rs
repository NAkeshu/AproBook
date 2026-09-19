use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
};

use quick_xml::{Reader, events::Event};
use regex::{Captures, Regex};
use zip::ZipArchive;

use super::{
    MAX_EBOOK_BYTES, MAX_RESOURCE_BYTES, MAX_TEXT_BYTES, ReaderAdapter, ReaderContent,
    ReaderNavigation, ReaderResource, TocEntry, fallback_title,
};
use crate::{BookFormat, CoreError, ExtractedMetadata, PartialDate, ReaderLocator, Result};

pub struct EpubAdapter;

#[derive(Default)]
struct Package {
    fields: HashMap<String, Vec<String>>,
    manifest: HashMap<String, ManifestItem>,
    spine: Vec<String>,
    cover_id: Option<String>,
    spine_toc: Option<String>,
}

#[derive(Clone)]
struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

impl ReaderAdapter for EpubAdapter {
    fn format(&self) -> BookFormat {
        BookFormat::Epub
    }

    fn inspect(&self, path: &Path) -> Result<ExtractedMetadata> {
        let mut archive = open(path)?;
        let opf = package_path(&mut archive)?;
        let package = parse_package(&read_text(&mut archive, &opf)?)?;
        let get = |key: &str| package.fields.get(key).and_then(|v| v.first()).cloned();
        let title = get("title")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| fallback_title(path));
        let cover_item = package
            .cover_id
            .as_ref()
            .and_then(|id| package.manifest.get(id))
            .or_else(|| {
                package.manifest.values().find(|item| {
                    item.properties
                        .split_whitespace()
                        .any(|p| p == "cover-image")
                })
            });
        let cover = cover_item
            .map(|item| resolve_archive_path(&opf, &item.href))
            .transpose()?
            .map(|name| read_bytes(&mut archive, &name))
            .transpose()?;
        let publication_date = get("date").and_then(|date| parse_date_prefix(&date));
        Ok(ExtractedMetadata {
            title,
            authors: package.fields.get("creator").cloned().unwrap_or_default(),
            abstract_text: get("description"),
            publication_date,
            edition: None,
            publisher: get("publisher"),
            cover,
            text_encoding: None,
            raw: serde_json::json!({
                "format": "epub",
                "opf": opf,
                "metadata": package.fields,
            }),
        })
    }

    fn navigation(&self, path: &Path) -> Result<ReaderNavigation> {
        let mut archive = open(path)?;
        let opf = package_path(&mut archive)?;
        let package = parse_package(&read_text(&mut archive, &opf)?)?;
        let spine = readable_spine(&package, &opf, &mut archive);
        if spine.is_empty() {
            return Err(CoreError::Reader(
                "EPUB has no readable spine entries".into(),
            ));
        }
        let sections = spine
            .iter()
            .map(|resource| epub_locator(resource, None))
            .collect();
        let allowed = spine.iter().cloned().collect::<HashSet<_>>();
        let mut toc = Vec::new();
        if let Some(item) = package.manifest.values().find(|item| {
            item.properties
                .split_whitespace()
                .any(|property| property == "nav")
        }) && let Ok(resource) = resolve_archive_path(&opf, &item.href)
            && let Ok(xml) = read_text(&mut archive, &resource)
        {
            toc = parse_epub3_nav(&xml, &resource, &allowed).unwrap_or_default();
        }
        if toc.is_empty() {
            let ncx = package
                .spine_toc
                .as_ref()
                .and_then(|id| package.manifest.get(id))
                .or_else(|| {
                    package
                        .manifest
                        .values()
                        .find(|item| item.media_type == "application/x-dtbncx+xml")
                });
            if let Some(item) = ncx
                && let Ok(resource) = resolve_archive_path(&opf, &item.href)
                && let Ok(xml) = read_text(&mut archive, &resource)
            {
                toc = parse_epub2_ncx(&xml, &resource, &allowed).unwrap_or_default();
            }
        }
        if toc.is_empty() {
            toc = spine
                .iter()
                .enumerate()
                .map(|(index, resource)| TocEntry {
                    label: format!("Chapter {}", index + 1),
                    locator: epub_locator(resource, None),
                    depth: 0,
                })
                .collect();
        }
        Ok(ReaderNavigation { sections, toc })
    }

    fn read(&self, path: &Path, locator: Option<&ReaderLocator>) -> Result<ReaderContent> {
        let mut archive = open(path)?;
        let opf = package_path(&mut archive)?;
        let package = parse_package(&read_text(&mut archive, &opf)?)?;
        if let Some(locator) = locator {
            locator.validate_for(BookFormat::Epub)?;
        }
        let requested = locator
            .and_then(|value| value.location.get("resource"))
            .and_then(|value| value.as_str())
            .map(validate_archive_path)
            .transpose()?;
        let spine_resources = readable_spine(&package, &opf, &mut archive);
        let resource = match requested {
            Some(resource) => resource,
            None => spine_resources
                .first()
                .cloned()
                .ok_or_else(|| CoreError::Reader("EPUB has no readable spine entries".into()))?,
        };
        if !spine_resources.contains(&resource) {
            return Err(CoreError::Reader(
                "EPUB locator does not reference a declared spine item".into(),
            ));
        }
        let html = read_text(&mut archive, &resource)?;
        let mut builder = ammonia::Builder::default();
        builder.url_schemes(HashSet::new());
        builder.add_generic_attributes(["id"]);
        builder
            .add_tags(["link"])
            .add_tag_attributes("link", ["href", "rel", "type"]);
        let allowed = manifest_resources(&package, &opf)?;
        let body = rewrite_markup_urls(&builder.clean(&html).to_string(), &resource, &allowed)?;
        Ok(ReaderContent::Html {
            body,
            base_resource: Some(resource),
        })
    }

    fn resource(&self, path: &Path, resource: &str) -> Result<ReaderResource> {
        let resource = decode_protocol_resource(resource)?;
        let mut archive = open(path)?;
        let opf = package_path(&mut archive)?;
        let package = parse_package(&read_text(&mut archive, &opf)?)?;
        let allowed = manifest_resources(&package, &opf)?;
        let item = allowed.get(&resource).ok_or_else(|| {
            CoreError::Reader(format!(
                "EPUB resource is not declared in manifest: {resource}"
            ))
        })?;
        let mut bytes = read_bytes(&mut archive, &resource)?;
        if item.media_type == "text/css" {
            let css = String::from_utf8(bytes)
                .map_err(|_| CoreError::Reader("EPUB stylesheet is not UTF-8".into()))?;
            bytes = rewrite_css_urls(&css, &resource, &allowed)?.into_bytes();
        } else if matches!(
            item.media_type.as_str(),
            "application/xhtml+xml" | "text/html"
        ) {
            let html = String::from_utf8(bytes)
                .map_err(|_| CoreError::Reader("EPUB document is not UTF-8".into()))?;
            let mut builder = ammonia::Builder::default();
            builder.url_schemes(HashSet::new());
            builder.add_generic_attributes(["id"]);
            builder
                .add_tags(["link"])
                .add_tag_attributes("link", ["href", "rel", "type"]);
            bytes = rewrite_markup_urls(&builder.clean(&html).to_string(), &resource, &allowed)?
                .into_bytes();
        }
        Ok(ReaderResource {
            media_type: item.media_type.clone(),
            bytes,
        })
    }
}

fn open(path: &Path) -> Result<ZipArchive<File>> {
    let length = path
        .metadata()
        .map_err(|error| crate::error::io(path, error))?
        .len();
    if length > MAX_EBOOK_BYTES {
        return Err(CoreError::Reader(format!(
            "EPUB exceeds {MAX_EBOOK_BYTES} byte limit"
        )));
    }
    let file = File::open(path).map_err(|error| crate::error::io(path, error))?;
    Ok(ZipArchive::new(file)?)
}

fn read_text(archive: &mut ZipArchive<File>, name: &str) -> Result<String> {
    let bytes = read_bytes(archive, name)?;
    if bytes.len() > MAX_TEXT_BYTES {
        return Err(CoreError::Reader(format!(
            "EPUB text resource exceeds {MAX_TEXT_BYTES} byte limit"
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| CoreError::Reader(format!("EPUB resource is not UTF-8: {name}")))
}

fn read_bytes(archive: &mut ZipArchive<File>, name: &str) -> Result<Vec<u8>> {
    let safe = validate_archive_path(name)?;
    let mut file = archive.by_name(&safe)?;
    if file.size() > MAX_RESOURCE_BYTES {
        return Err(CoreError::Reader(format!(
            "EPUB resource exceeds {MAX_RESOURCE_BYTES} byte limit: {name}"
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| crate::error::io(PathBuf::from(name), error))?;
    Ok(bytes)
}

fn package_path(archive: &mut ZipArchive<File>) -> Result<String> {
    let container = read_text(archive, "META-INF/container.xml")?;
    let mut reader = Reader::from_str(&container);
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) | Ok(Event::Empty(event))
                if event.local_name().as_ref() == b"rootfile" =>
            {
                for attr in event.attributes().flatten() {
                    if attr.key.local_name().as_ref() == b"full-path" {
                        return validate_archive_path(&String::from_utf8_lossy(&attr.value));
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => {
                return Err(CoreError::Reader(format!(
                    "invalid EPUB container.xml: {error}"
                )));
            }
            _ => {}
        }
    }
    Err(CoreError::Reader("EPUB container has no rootfile".into()))
}

fn parse_package(xml: &str) -> Result<Package> {
    let mut package = Package::default();
    let mut reader = Reader::from_str(xml);
    let mut active_field: Option<String> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                let name = String::from_utf8_lossy(event.local_name().as_ref()).to_string();
                if matches!(
                    name.as_str(),
                    "title" | "creator" | "description" | "date" | "publisher"
                ) {
                    active_field = Some(name);
                } else if name == "item" {
                    add_manifest(&mut package, &event);
                } else if name == "spine" {
                    package.spine_toc = attribute(&event, b"toc");
                } else if name == "itemref" {
                    if let Some(idref) = attribute(&event, b"idref") {
                        package.spine.push(idref);
                    }
                } else if name == "meta" && attribute(&event, b"name").as_deref() == Some("cover") {
                    package.cover_id = attribute(&event, b"content");
                }
            }
            Ok(Event::Empty(event)) => {
                let name = event.local_name();
                if name.as_ref() == b"item" {
                    add_manifest(&mut package, &event);
                }
                if name.as_ref() == b"itemref" {
                    if let Some(idref) = attribute(&event, b"idref") {
                        package.spine.push(idref);
                    }
                }
                if name.as_ref() == b"meta"
                    && attribute(&event, b"name").as_deref() == Some("cover")
                {
                    package.cover_id = attribute(&event, b"content");
                }
            }
            Ok(Event::Text(text)) => {
                if let Some(field) = active_field.as_ref() {
                    let value = text
                        .decode()
                        .map_err(|error| CoreError::Reader(error.to_string()))?
                        .trim()
                        .to_owned();
                    if !value.is_empty() {
                        package.fields.entry(field.clone()).or_default().push(value);
                    }
                }
            }
            Ok(Event::End(_)) => active_field = None,
            Ok(Event::Eof) => break,
            Err(error) => return Err(CoreError::Reader(format!("invalid EPUB package: {error}"))),
            _ => {}
        }
    }
    Ok(package)
}

fn add_manifest(package: &mut Package, event: &quick_xml::events::BytesStart<'_>) {
    if let (Some(id), Some(href)) = (attribute(event, b"id"), attribute(event, b"href")) {
        package.manifest.insert(
            id,
            ManifestItem {
                href,
                media_type: attribute(event, b"media-type").unwrap_or_default(),
                properties: attribute(event, b"properties").unwrap_or_default(),
            },
        );
    }
}

fn attribute(event: &quick_xml::events::BytesStart<'_>, key: &[u8]) -> Option<String> {
    event
        .attributes()
        .flatten()
        .find(|attr| attr.key.local_name().as_ref() == key)
        .and_then(|attr| attr.unescape_value().ok().map(|value| value.into_owned()))
}

fn epub_locator(resource: &str, fragment: Option<&str>) -> ReaderLocator {
    let location = if let Some(fragment) = fragment {
        serde_json::json!({ "resource": resource, "fragment": fragment })
    } else {
        serde_json::json!({ "resource": resource })
    };
    ReaderLocator::new(BookFormat::Epub, location)
}

fn readable_spine(package: &Package, opf: &str, archive: &mut ZipArchive<File>) -> Vec<String> {
    package
        .spine
        .iter()
        .filter_map(|id| package.manifest.get(id))
        .filter(|item| {
            matches!(
                item.media_type.as_str(),
                "application/xhtml+xml" | "text/html"
            )
        })
        .filter_map(|item| resolve_archive_path(opf, &item.href).ok())
        .filter(|resource| {
            archive
                .by_name(resource)
                .is_ok_and(|entry| entry.size() <= MAX_TEXT_BYTES as u64)
        })
        .collect()
}

fn toc_target(source: &str, href: &str, spine: &HashSet<String>) -> Option<ReaderLocator> {
    let href = href.trim();
    if href.is_empty() || href.starts_with("//") || href.contains(['?', '\\', ':']) {
        return None;
    }
    let (path, fragment) = href
        .split_once('#')
        .map_or((href, None), |(path, fragment)| (path, Some(fragment)));
    let path = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    let resource = if path.is_empty() {
        source.to_owned()
    } else {
        resolve_archive_path(source, &path).ok()?
    };
    if !spine.contains(&resource) {
        return None;
    }
    let fragment = if let Some(fragment) = fragment {
        let decoded = percent_encoding::percent_decode_str(fragment)
            .decode_utf8()
            .ok()?;
        if decoded.is_empty()
            || decoded.len() > 512
            || decoded
                .chars()
                .any(|value| value.is_control() || value.is_whitespace() || value == '#')
        {
            return None;
        }
        Some(decoded.into_owned())
    } else {
        None
    };
    Some(epub_locator(&resource, fragment.as_deref()))
}

fn xml_text(text: &quick_xml::events::BytesText<'_>) -> Result<String> {
    let decoded = text
        .decode()
        .map_err(|error| CoreError::Reader(error.to_string()))?;
    Ok(quick_xml::escape::unescape(&decoded)
        .map_err(|error| CoreError::Reader(error.to_string()))?
        .into_owned())
}

fn xml_reference(reference: &quick_xml::events::BytesRef<'_>) -> Result<String> {
    let encoded = format!(
        "&{};",
        reference
            .decode()
            .map_err(|error| CoreError::Reader(error.to_string()))?
    );
    Ok(quick_xml::escape::unescape(&encoded)
        .map_err(|error| CoreError::Reader(error.to_string()))?
        .into_owned())
}

fn clean_label(label: &str) -> Option<String> {
    let normalized = label.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()).then(|| normalized.chars().take(200).collect())
}

const MAX_TOC_ENTRIES: usize = 2_000;
const MAX_TOC_DEPTH: u8 = 8;
const MAX_NCX_NESTING: usize = 64;

fn parse_epub3_nav(xml: &str, source: &str, spine: &HashSet<String>) -> Result<Vec<TocEntry>> {
    let mut reader = Reader::from_str(xml);
    let mut entries = Vec::new();
    let mut document_depth = 0usize;
    let mut toc_nav_depth = None;
    let mut list_depth = 0usize;
    let mut link: Option<(String, String, u8)> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => {
                document_depth += 1;
                let name = event.local_name();
                if name.as_ref() == b"nav" && toc_nav_depth.is_none() {
                    let kind = attribute(&event, b"type").unwrap_or_default();
                    let role = attribute(&event, b"role").unwrap_or_default();
                    if kind.split_whitespace().any(|value| value == "toc")
                        || role.split_whitespace().any(|value| value == "doc-toc")
                    {
                        toc_nav_depth = Some(document_depth);
                    }
                } else if toc_nav_depth.is_some() {
                    if name.as_ref() == b"ol" {
                        list_depth += 1;
                    } else if name.as_ref() == b"a" && link.is_none() && list_depth > 0 {
                        if let Some(href) = attribute(&event, b"href") {
                            link = Some((
                                href,
                                String::new(),
                                list_depth.saturating_sub(1).min(MAX_TOC_DEPTH as usize) as u8,
                            ));
                        }
                    }
                }
            }
            Ok(Event::Text(text)) if link.is_some() => {
                if let Some((_, label, _)) = link.as_mut() {
                    label.push_str(&xml_text(&text)?);
                }
            }
            Ok(Event::GeneralRef(reference)) if link.is_some() => {
                if let Some((_, label, _)) = link.as_mut() {
                    label.push_str(&xml_reference(&reference)?);
                }
            }
            Ok(Event::CData(text)) if link.is_some() => {
                if let Some((_, label, _)) = link.as_mut() {
                    label.push_str(
                        &text
                            .decode()
                            .map_err(|error| CoreError::Reader(error.to_string()))?,
                    );
                }
            }
            Ok(Event::End(event)) => {
                let name = event.local_name();
                if toc_nav_depth.is_some() {
                    if name.as_ref() == b"a" {
                        if let Some((href, label, depth)) = link.take()
                            && entries.len() < MAX_TOC_ENTRIES
                            && let Some(label) = clean_label(&label)
                            && let Some(locator) = toc_target(source, &href, spine)
                        {
                            entries.push(TocEntry {
                                label,
                                locator,
                                depth,
                            });
                        }
                    } else if name.as_ref() == b"ol" {
                        list_depth = list_depth.saturating_sub(1);
                    } else if name.as_ref() == b"nav" && toc_nav_depth == Some(document_depth) {
                        toc_nav_depth = None;
                        if !entries.is_empty() {
                            break;
                        }
                    }
                }
                document_depth = document_depth.saturating_sub(1);
            }
            Ok(Event::Eof) => break,
            Err(error) => {
                return Err(CoreError::Reader(format!(
                    "invalid EPUB navigation: {error}"
                )));
            }
            _ => {}
        }
    }
    Ok(entries)
}

#[derive(Default)]
struct NcxNode {
    order: usize,
    depth: u8,
    label: String,
    href: Option<String>,
}

fn parse_epub2_ncx(xml: &str, source: &str, spine: &HashSet<String>) -> Result<Vec<TocEntry>> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<NcxNode> = Vec::new();
    let mut collected = Vec::new();
    let mut order = 0usize;
    let mut in_text = false;
    let mut ignored_depth = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => match event.local_name().as_ref() {
                b"navPoint" => {
                    if ignored_depth > 0 || stack.len() >= MAX_NCX_NESTING {
                        ignored_depth += 1;
                        in_text = false;
                    } else {
                        stack.push(NcxNode {
                            order,
                            depth: stack.len().min(MAX_TOC_DEPTH as usize) as u8,
                            ..Default::default()
                        });
                        order += 1;
                    }
                }
                b"text" if !stack.is_empty() && ignored_depth == 0 => in_text = true,
                b"content" if !stack.is_empty() && ignored_depth == 0 => {
                    if let Some(node) = stack.last_mut() {
                        node.href = attribute(&event, b"src");
                    }
                }
                _ => {}
            },
            Ok(Event::Empty(event))
                if event.local_name().as_ref() == b"content"
                    && !stack.is_empty()
                    && ignored_depth == 0 =>
            {
                if let Some(node) = stack.last_mut() {
                    node.href = attribute(&event, b"src");
                }
            }
            Ok(Event::Text(text)) if in_text => {
                if let Some(node) = stack.last_mut() {
                    node.label.push_str(&xml_text(&text)?);
                }
            }
            Ok(Event::GeneralRef(reference)) if in_text => {
                if let Some(node) = stack.last_mut() {
                    node.label.push_str(&xml_reference(&reference)?);
                }
            }
            Ok(Event::CData(text)) if in_text => {
                if let Some(node) = stack.last_mut() {
                    node.label.push_str(
                        &text
                            .decode()
                            .map_err(|error| CoreError::Reader(error.to_string()))?,
                    );
                }
            }
            Ok(Event::End(event)) => match event.local_name().as_ref() {
                b"text" => in_text = false,
                b"navPoint" => {
                    if ignored_depth > 0 {
                        ignored_depth -= 1;
                        continue;
                    }
                    if let Some(node) = stack.pop()
                        && collected.len() < MAX_TOC_ENTRIES
                        && let Some(label) = clean_label(&node.label)
                        && let Some(href) = node.href
                        && let Some(locator) = toc_target(source, &href, spine)
                    {
                        collected.push((
                            node.order,
                            TocEntry {
                                label,
                                locator,
                                depth: node.depth,
                            },
                        ));
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(CoreError::Reader(format!("invalid EPUB NCX: {error}"))),
            _ => {}
        }
    }
    collected.sort_by_key(|(order, _)| *order);
    Ok(collected
        .into_iter()
        .take(MAX_TOC_ENTRIES)
        .map(|(_, entry)| entry)
        .collect())
}

fn resolve_archive_path(base_file: &str, relative: &str) -> Result<String> {
    let base = Path::new(base_file)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    validate_archive_path(&base.join(relative).to_string_lossy())
}

pub fn validate_archive_path(value: &str) -> Result<String> {
    if value.contains('\\') || value.contains('\0') || value.contains("://") {
        return Err(CoreError::UnsafeArchivePath(value.to_owned()));
    }
    let mut safe = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !safe.pop() {
                    return Err(CoreError::UnsafeArchivePath(value.to_owned()));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(CoreError::UnsafeArchivePath(value.to_owned()));
            }
        }
    }
    if safe.as_os_str().is_empty() {
        return Err(CoreError::UnsafeArchivePath(value.to_owned()));
    }
    Ok(safe.to_string_lossy().replace('\\', "/"))
}

fn manifest_resources<'a>(
    package: &'a Package,
    opf: &str,
) -> Result<HashMap<String, &'a ManifestItem>> {
    package
        .manifest
        .values()
        .map(|item| Ok((resolve_archive_path(opf, &item.href)?, item)))
        .collect()
}

fn protocol_url(resource: &str) -> String {
    use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
    format!(
        "ebook-resource://book/{}",
        utf8_percent_encode(resource, NON_ALPHANUMERIC)
    )
}

fn decode_protocol_resource(value: &str) -> Result<String> {
    let value = value
        .strip_prefix("ebook-resource://book/")
        .unwrap_or(value);
    let decoded = percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .map_err(|_| CoreError::UnsafeArchivePath(value.to_owned()))?;
    validate_archive_path(&decoded)
}

fn resolve_link(
    current: &str,
    value: &str,
    allowed: &HashMap<String, &ManifestItem>,
) -> Result<Option<String>> {
    let value = value.trim();
    if value.is_empty() || value.starts_with('#') {
        return Ok(None);
    }
    if value.starts_with("//") || value.contains(':') || value.contains('\\') {
        return Ok(Some(String::new()));
    }
    let path_only = value.split(['?', '#']).next().unwrap_or(value);
    let resource = resolve_archive_path(current, path_only)?;
    if !allowed.contains_key(&resource) {
        return Ok(Some(String::new()));
    }
    let suffix = value
        .find(['?', '#'])
        .map(|index| &value[index..])
        .unwrap_or("");
    Ok(Some(format!("{}{suffix}", protocol_url(&resource))))
}

fn rewrite_markup_urls(
    html: &str,
    current: &str,
    allowed: &HashMap<String, &ManifestItem>,
) -> Result<String> {
    let expression = Regex::new(r#"(?i)\b(src|href)\s*=\s*(['\"])([^'\"]*)['\"]"#)
        .map_err(|error| CoreError::Reader(error.to_string()))?;
    let mut failure = None;
    let rewritten = expression.replace_all(html, |capture: &Captures<'_>| {
        let original = capture
            .get(0)
            .map(|value| value.as_str())
            .unwrap_or_default();
        match resolve_link(current, &capture[3], allowed) {
            Ok(None) => original.to_owned(),
            Ok(Some(value)) if value.is_empty() => String::new(),
            Ok(Some(value)) => format!("{}={}{}{}", &capture[1], &capture[2], value, &capture[2]),
            Err(error) => {
                failure = Some(error);
                String::new()
            }
        }
    });
    if let Some(error) = failure {
        Err(error)
    } else {
        Ok(rewritten.into_owned())
    }
}

fn rewrite_css_urls(
    css: &str,
    current: &str,
    allowed: &HashMap<String, &ManifestItem>,
) -> Result<String> {
    if css.len() > MAX_TEXT_BYTES {
        return Err(CoreError::Reader("EPUB stylesheet is too large".into()));
    }
    // Imports are not needed for basic reading and can trigger network loads
    // independently of url() rewriting, so omit the entire rule.
    let imports = Regex::new(r#"(?is)@import\s+[^;]*;"#)
        .map_err(|error| CoreError::Reader(error.to_string()))?;
    let css = imports.replace_all(css, "");
    let expression = Regex::new(r#"(?i)url\(\s*(['\"]?)([^)'\"]+)['\"]?\s*\)"#)
        .map_err(|error| CoreError::Reader(error.to_string()))?;
    let mut failure = None;
    let rewritten = expression.replace_all(&css, |capture: &Captures<'_>| {
        match resolve_link(current, &capture[2], allowed) {
            Ok(None) => capture[0].to_owned(),
            Ok(Some(value)) if value.is_empty() => "url()".to_owned(),
            Ok(Some(value)) => format!("url(\"{value}\")"),
            Err(error) => {
                failure = Some(error);
                "url()".to_owned()
            }
        }
    });
    if let Some(error) = failure {
        Err(error)
    } else {
        Ok(rewritten.into_owned())
    }
}

fn parse_date_prefix(value: &str) -> Option<PartialDate> {
    [10, 7, 4]
        .into_iter()
        .filter(|length| value.len() >= *length)
        .find_map(|length| PartialDate::parse(&value[..length]).ok())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::tempdir;
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;

    fn write_test_epub(path: &Path, opf: &str, resources: &[(&str, &str)]) {
        let file = File::create(path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("META-INF/container.xml", options).unwrap();
        zip.write_all(br#"<container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#).unwrap();
        zip.start_file("OPS/book.opf", options).unwrap();
        zip.write_all(opf.as_bytes()).unwrap();
        for (name, body) in resources {
            zip.start_file(name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn archive_paths_cannot_escape() {
        assert_eq!(
            validate_archive_path("OPS/../images/a.png").unwrap(),
            "images/a.png"
        );
        assert!(validate_archive_path("../../secret").is_err());
        assert!(validate_archive_path("/etc/passwd").is_err());
    }

    #[test]
    fn extracts_metadata_and_sanitizes_content() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("book.epub");
        let file = File::create(&path).unwrap();
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        zip.start_file("META-INF/container.xml", options).unwrap();
        zip.write_all(br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OPS/book.opf"/></rootfiles></container>"#).unwrap();
        zip.start_file("OPS/book.opf", options).unwrap();
        zip.write_all(br#"<package xmlns:dc="x"><metadata><dc:title>Safe Book</dc:title><dc:creator>A. Writer</dc:creator><dc:date>2026-09-16T00:00:00Z</dc:date></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="css" href="style/main.css" media-type="text/css"/><item id="image" href="images/cover.png" media-type="image/png"/></manifest><spine><itemref idref="chapter"/></spine></package>"#).unwrap();
        zip.start_file("OPS/chapter.xhtml", options).unwrap();
        zip.write_all(br#"<html><head><link rel="stylesheet" href="style/main.css"/></head><body onload="evil()"><script>evil()</script><h1>Hello</h1><img src="images/cover.png"/><img src="https://example.invalid/tracker"/></body></html>"#).unwrap();
        zip.start_file("OPS/style/main.css", options).unwrap();
        zip.write_all(br#"body{background:url(../images/cover.png)} x{background:url(https://example.invalid/x)}"#).unwrap();
        zip.start_file("OPS/images/cover.png", options).unwrap();
        zip.write_all(b"not-decoded-by-resource-api").unwrap();
        zip.finish().unwrap();

        let adapter = EpubAdapter;
        let metadata = adapter.inspect(&path).unwrap();
        assert_eq!(metadata.title, "Safe Book");
        assert_eq!(metadata.authors, ["A. Writer"]);
        assert_eq!(metadata.publication_date.unwrap().as_str(), "2026-09-16");
        let ReaderContent::Html { body, .. } = adapter.read(&path, None).unwrap() else {
            panic!()
        };
        assert!(body.contains("Hello"));
        assert!(!body.contains("script"));
        assert!(!body.contains("onload"));
        assert!(!body.contains("https://"));
        assert!(body.contains("ebook-resource://book/OPS%2Fimages%2Fcover%2Epng"));
        let image = adapter.resource(&path, "OPS/images/cover.png").unwrap();
        assert_eq!(image.media_type, "image/png");
        let css = adapter
            .resource(&path, "ebook-resource://book/OPS%2Fstyle%2Fmain%2Ecss")
            .unwrap();
        let css = String::from_utf8(css.bytes).unwrap();
        assert!(css.contains("ebook-resource://book/OPS%2Fimages%2Fcover%2Epng"));
        assert!(!css.contains("https://"));
        assert!(adapter.resource(&path, "OPS/not-in-manifest").is_err());
        assert!(adapter.resource(&path, r"OPS\images\cover.png").is_err());
        let invalid_locator = ReaderLocator::new(
            BookFormat::Epub,
            serde_json::json!({"resource":"OPS/images/cover.png"}),
        );
        assert!(adapter.read(&path, Some(&invalid_locator)).is_err());
    }

    #[test]
    fn epub3_navigation_uses_authored_titles_depth_and_fragments() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("nav3.epub");
        write_test_epub(
            &path,
            r#"<package><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/><item id="c" href="c.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="a"/><itemref idref="b"/><itemref idref="c"/></spine></package>"#,
            &[
                (
                    "OPS/nav.xhtml",
                    r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="a.xhtml#opening">First &amp; Foremost</a><ol><li><a href="b.xhtml#deep">Nested Chapter</a></li></ol></li><li><a href="../../secret.xhtml">Unsafe</a></li><li><a href="https://example.invalid/evil">Remote</a></li><li><a href="missing.xhtml">Undeclared</a></li></ol></nav></body></html>"#,
                ),
                (
                    "OPS/a.xhtml",
                    "<html><body><h1 id='opening'>A</h1></body></html>",
                ),
                (
                    "OPS/b.xhtml",
                    "<html><body><h1 id='deep'>B</h1></body></html>",
                ),
                ("OPS/c.xhtml", "<html><body>C</body></html>"),
            ],
        );
        let navigation = EpubAdapter.navigation(&path).unwrap();
        assert_eq!(navigation.sections.len(), 3);
        assert_eq!(navigation.toc.len(), 2);
        assert_eq!(navigation.toc[0].label, "First & Foremost");
        assert_eq!(navigation.toc[0].depth, 0);
        assert_eq!(
            navigation.toc[0].locator.location["resource"],
            "OPS/a.xhtml"
        );
        assert_eq!(navigation.toc[0].locator.location["fragment"], "opening");
        assert_eq!(navigation.toc[1].depth, 1);
        assert_eq!(navigation.toc[1].locator.location["fragment"], "deep");
        let ReaderContent::Html { body, .. } = EpubAdapter
            .read(&path, Some(&navigation.toc[0].locator))
            .unwrap()
        else {
            panic!()
        };
        assert!(body.contains("id=\"opening\""), "{body:?}");
    }

    #[test]
    fn epub2_ncx_navigation_preserves_order_and_fallback_uses_all_spine_entries() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("nav2.epub");
        write_test_epub(
            &path,
            r#"<package><manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/></manifest><spine toc="ncx"><itemref idref="a"/><itemref idref="b"/></spine></package>"#,
            &[
                (
                    "OPS/toc.ncx",
                    r#"<ncx><navMap><navPoint><navLabel><text>Part One</text></navLabel><content src="a.xhtml"/><navPoint><navLabel><text>Inside Part</text></navLabel><content src="b.xhtml#section"/></navPoint></navPoint></navMap></ncx>"#,
                ),
                ("OPS/a.xhtml", "<html><body>A</body></html>"),
                ("OPS/b.xhtml", "<html><body id='section'>B</body></html>"),
            ],
        );
        let navigation = EpubAdapter.navigation(&path).unwrap();
        assert_eq!(navigation.sections.len(), 2);
        assert_eq!(
            navigation
                .toc
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            ["Part One", "Inside Part"]
        );
        assert_eq!(
            navigation
                .toc
                .iter()
                .map(|entry| entry.depth)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(navigation.toc[1].locator.location["fragment"], "section");

        let fallback = temp.path().join("fallback.epub");
        write_test_epub(
            &fallback,
            r#"<package><manifest><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="a"/><itemref idref="b"/></spine></package>"#,
            &[
                ("OPS/a.xhtml", "<html>A</html>"),
                ("OPS/b.xhtml", "<html>B</html>"),
            ],
        );
        let navigation = EpubAdapter.navigation(&fallback).unwrap();
        assert_eq!(
            navigation
                .toc
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>(),
            ["Chapter 1", "Chapter 2"]
        );
        assert_eq!(navigation.sections.len(), 2);
    }

    #[test]
    fn toc_targets_cannot_escape_or_reference_undeclared_resources() {
        let spine = HashSet::from(["OPS/chapter.xhtml".to_owned()]);
        assert!(toc_target("OPS/nav.xhtml", "../../secret.xhtml", &spine).is_none());
        assert!(toc_target("OPS/nav.xhtml", "%2e%2e/%2e%2e/secret.xhtml", &spine).is_none());
        assert!(
            toc_target(
                "OPS/nav.xhtml",
                "https://example.invalid/chapter.xhtml",
                &spine
            )
            .is_none()
        );
        assert!(toc_target("OPS/nav.xhtml", "missing.xhtml", &spine).is_none());
        assert!(toc_target("OPS/nav.xhtml", "chapter.xhtml#bad%23anchor", &spine).is_none());
        assert_eq!(
            toc_target("OPS/nav.xhtml", "chapter.xhtml#safe", &spine)
                .unwrap()
                .location["fragment"],
            "safe"
        );
    }
}
