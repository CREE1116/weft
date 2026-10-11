//! Native, bounded document extraction. Source text and images stay out of the DB.
use anyhow::{Context, Result, bail};
use quick_xml::{Reader, events::Event};
use serde::Serialize;
use std::{
    collections::HashMap,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
};

pub const INPUT_LIMIT: usize = 32 * 1024 * 1024;
pub const TEXT_LIMIT: usize = 2 * 1024 * 1024;
const PART_LIMIT: usize = 8 * 1024 * 1024;

pub fn format(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "pdf" => Some("pdf"),
        "docx" | "docm" => Some("docx"),
        "pptx" | "pptm" => Some("pptx"),
        "xlsx" | "xlsm" => Some("xlsx"),
        "odt" => Some("odt"),
        "odp" => Some("odp"),
        "ods" => Some("ods"),
        _ => None,
    }
}

pub fn line_kind(path: &str, kind: &str) -> &'static str {
    if kind != "text" {
        "none"
    } else if format(Path::new(path)).is_some() {
        "extracted_text"
    } else {
        "source"
    }
}

#[derive(Clone, Serialize)]
pub struct Location {
    pub kind: String,
    pub label: String,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Serialize)]
pub struct Image {
    pub id: String,
    pub format: String,
    pub location: String,
    pub status: String,
    pub bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<PathBuf>,
    #[serde(skip)]
    data: Vec<u8>,
}

#[derive(Serialize)]
pub struct Extracted {
    pub format: String,
    pub text: String,
    pub locations: Vec<Location>,
    pub images: Vec<Image>,
    pub warnings: Vec<String>,
}

impl Extracted {
    fn new(format: &str) -> Self {
        Self {
            format: format.into(),
            text: String::new(),
            locations: Vec::new(),
            images: Vec::new(),
            warnings: Vec::new(),
        }
    }
    fn part(&mut self, kind: &str, label: &str, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Ok(());
        }
        if self.text.len() + text.len() + label.len() + 4 > TEXT_LIMIT {
            bail!("extracted text exceeds 2 MiB");
        }
        let start = self.text.lines().count() + 1;
        self.text.push_str(&format!("[{label}]\n{text}\n"));
        self.locations.push(Location {
            kind: kind.into(),
            label: label.into(),
            start_line: start,
            end_line: self.text.lines().count(),
        });
        Ok(())
    }
    pub fn export_images(&mut self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        for image in &mut self.images {
            if image.status != "available" {
                continue;
            }
            // Content-derived names avoid ZIP path traversal and reuse identical
            // original images across documents without overwriting other files.
            let hash = blake3::hash(&image.data).to_hex().to_string();
            let path = dir.join(format!("{hash}.{}", image.format));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(&image.data) {
                        let _ = std::fs::remove_file(&path);
                        return Err(error.into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = std::fs::File::open(&path)?;
                    let mut bytes = Vec::new();
                    existing
                        .take(INPUT_LIMIT as u64 + 1)
                        .read_to_end(&mut bytes)?;
                    if bytes != image.data {
                        bail!(
                            "image output already exists with different contents: {}",
                            path.display()
                        );
                    }
                }
                Err(e) => return Err(e.into()),
            }
            image.output_path = Some(path.canonicalize()?);
        }
        Ok(())
    }
}

pub fn extract(path: &Path, bytes: &[u8], images: bool) -> Result<Extracted> {
    if bytes.len() > INPUT_LIMIT {
        bail!("document exceeds 32 MiB input limit");
    }
    let Some(format) = format(path) else {
        let text = std::str::from_utf8(bytes).context("not a supported document or UTF-8 text")?;
        if text.len() > TEXT_LIMIT || text.contains('\0') {
            bail!("invalid or oversized source text");
        }
        let mut result = Extracted::new("text");
        result.text = text.into();
        return Ok(result);
    };
    if format == "pdf" {
        return pdf(bytes, images);
    }
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).context("opening document ZIP container")?;
    if archive.len() > 8192 {
        bail!("too many document parts");
    }
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        expanded = expanded
            .checked_add(archive.by_index(index)?.size())
            .context("ZIP size overflow")?;
        if expanded > 64 * 1024 * 1024 {
            bail!("document ZIP exceeds 64 MiB declared expansion budget");
        }
    }
    let names: Vec<_> = archive.file_names().map(str::to_owned).collect();
    let mut result = Extracted::new(format);
    match format {
        "docx" => {
            result.part(
                "part",
                "word/document.xml",
                &xml_text(&read_part(&mut archive, "word/document.xml")?, false)?,
            )?;
            let mut extras: Vec<_> = names
                .iter()
                .filter(|n| {
                    n.starts_with("word/")
                        && n.ends_with(".xml")
                        && (n.starts_with("word/header")
                            || n.starts_with("word/footer")
                            || n.as_str() == "word/footnotes.xml"
                            || n.as_str() == "word/endnotes.xml")
                })
                .collect();
            extras.sort();
            for name in extras {
                result.part(
                    "part",
                    name,
                    &xml_text(&read_part(&mut archive, name)?, false)?,
                )?;
            }
        }
        "pptx" => {
            let presentation = read_part(&mut archive, "ppt/presentation.xml")?;
            let rels = relationships(&read_part(&mut archive, "ppt/_rels/presentation.xml.rels")?)?;
            let ids = attributes(&presentation, b"sldId", b"r:id")?;
            for (number, id) in ids.iter().enumerate() {
                let target = rels.get(id).context("missing slide relationship")?;
                let part = resolve_part("ppt", target)?;
                let text = xml_text(&read_part(&mut archive, &part)?, false)?;
                result.part("slide", &format!("Slide {}", number + 1), &text)?;
            }
        }
        "xlsx" => {
            let shared = if names.iter().any(|n| n == "xl/sharedStrings.xml") {
                shared_strings(&read_part(&mut archive, "xl/sharedStrings.xml")?)?
            } else {
                Vec::new()
            };
            let workbook = read_part(&mut archive, "xl/workbook.xml")?;
            let rels = relationships(&read_part(&mut archive, "xl/_rels/workbook.xml.rels")?)?;
            let ids = attributes(&workbook, b"sheet", b"r:id")?;
            let labels = attributes(&workbook, b"sheet", b"name")?;
            for (number, id) in ids.iter().enumerate() {
                let target = rels.get(id).context("missing worksheet relationship")?;
                let part = resolve_part("xl", target)?;
                result.part(
                    "sheet",
                    labels.get(number).context("missing sheet name")?,
                    &sheet_text(&read_part(&mut archive, &part)?, &shared)?,
                )?;
            }
        }
        _ => result.part(
            "part",
            "content.xml",
            &xml_text(&read_part(&mut archive, "content.xml")?, true)?,
        )?,
    }
    if images {
        let mut image_bytes = 0;
        for name in names.iter().filter(|n| {
            n.starts_with("word/media/")
                || n.starts_with("ppt/media/")
                || n.starts_with("xl/media/")
                || n.starts_with("Pictures/")
        }) {
            if name.ends_with('/') {
                continue;
            }
            let extension = Path::new(name)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !matches!(
                extension.as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "bmp"
                    | "tif"
                    | "tiff"
                    | "webp"
                    | "svg"
                    | "emf"
                    | "wmf"
                    | "jp2"
                    | "j2k"
                    | "wdp"
                    | "ico"
                    | "avif"
                    | "heic"
            ) {
                result
                    .warnings
                    .push(format!("non-image media part skipped: {name}"));
                continue;
            }
            let data = read_part(&mut archive, name)?;
            image_bytes += data.len();
            if image_bytes > INPUT_LIMIT {
                bail!("embedded images exceed 32 MiB budget");
            }
            let ext = Path::new(name)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("bin")
                .to_ascii_lowercase();
            let ext = if ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                ext
            } else {
                "bin".into()
            };
            result.images.push(Image {
                id: name.clone(),
                format: ext,
                location: name.clone(),
                status: "available".into(),
                bytes: data.len(),
                output_path: None,
                data,
            });
        }
    }
    if result.text.trim().is_empty() {
        result
            .warnings
            .push("no_extractable_text; OCR is not implemented".into());
    }
    Ok(result)
}

fn read_part(archive: &mut zip::ZipArchive<Cursor<&[u8]>>, name: &str) -> Result<Vec<u8>> {
    let file = archive
        .by_name(name)
        .with_context(|| format!("reading {name}"))?;
    if file.size() > PART_LIMIT as u64 {
        bail!("document part exceeds 8 MiB: {name}");
    }
    let mut data = Vec::new();
    file.take(PART_LIMIT as u64 + 1).read_to_end(&mut data)?;
    if data.len() > PART_LIMIT {
        bail!("document part exceeds 8 MiB: {name}");
    }
    Ok(data)
}
fn resolve_part(base: &str, target: &str) -> Result<String> {
    if target.contains(':') || target.contains('\\') {
        bail!("external or invalid document relationship");
    }
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    bail!("relationship escapes archive");
                }
            }
            _ => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}
fn attributes(xml: &[u8], tag: &[u8], attribute: &[u8]) -> Result<Vec<String>> {
    let mut reader = Reader::from_reader(xml);
    let mut out = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == tag => {
                for a in e.attributes() {
                    let a = a?;
                    if (attribute == b"r:id"
                        && a.key.local_name().as_ref() == b"id"
                        && a.key.as_ref().contains(&b':'))
                        || a.key.as_ref() == attribute
                    {
                        out.push(a.decode_and_unescape_value(reader.decoder())?.into_owned());
                    }
                }
            }
            Event::DocType(_) => bail!("DTD is not supported"),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}
fn relationships(xml: &[u8]) -> Result<HashMap<String, String>> {
    let mut reader = Reader::from_reader(xml);
    let mut out = HashMap::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"Relationship" => {
                let (mut id, mut target, mut external) = (None, None, false);
                for a in e.attributes() {
                    let a = a?;
                    let v = a.decode_and_unescape_value(reader.decoder())?.into_owned();
                    match a.key.as_ref() {
                        b"Id" => id = Some(v),
                        b"Target" => target = Some(v),
                        b"TargetMode" if v == "External" => external = true,
                        _ => {}
                    }
                }
                if !external {
                    out.insert(
                        id.context("relationship missing Id")?,
                        target.context("relationship missing Target")?,
                    );
                }
            }
            Event::DocType(_) => bail!("DTD is not supported"),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}
fn xml_text(xml: &[u8], odf: bool) -> Result<String> {
    let mut reader = Reader::from_reader(xml);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.local_name().as_ref() == b"t" => in_text = true,
            Event::End(e) => match e.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" | b"tr" | b"table-row" | b"h" => out.push('\n'),
                b"tc" | b"table-cell" => out.push('\t'),
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                b"br" | b"line-break" => out.push('\n'),
                b"tab" => out.push('\t'),
                _ => {}
            },
            Event::Text(e) if in_text || odf => out.push_str(&e.decode()?),
            Event::CData(e) if in_text || odf => out.push_str(&e.decode()?),
            Event::GeneralRef(e) if in_text || odf => {
                out.push_str(&quick_xml::escape::unescape(&format!("&{};", e.decode()?))?)
            }
            Event::DocType(_) => bail!("DTD is not supported"),
            Event::Eof => break,
            _ => {}
        }
        if out.len() > TEXT_LIMIT {
            bail!("XML text exceeds 2 MiB");
        }
    }
    Ok(out)
}
fn shared_strings(xml: &[u8]) -> Result<Vec<String>> {
    let mut reader = Reader::from_reader(xml);
    let mut strings = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.local_name().as_ref() == b"si" => {
                let inner = reader.read_text(e.name())?;
                strings.push(xml_text(inner.as_bytes(), false)?);
            }
            Event::DocType(_) => bail!("DTD is not supported"),
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(strings)
}
fn sheet_text(xml: &[u8], shared: &[String]) -> Result<String> {
    let mut reader = Reader::from_reader(xml);
    let mut out = String::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.local_name().as_ref() == b"c" => {
                let mut shared_type = false;
                for a in e.attributes() {
                    let a = a?;
                    if a.key.as_ref() == b"t" && a.value.as_ref() == b"s" {
                        shared_type = true;
                    }
                }
                let inner = reader.read_text(e.name())?;
                let mut cell_reader = Reader::from_str(&inner);
                let mut value = String::new();
                loop {
                    match cell_reader.read_event()? {
                        Event::Start(v)
                            if v.local_name().as_ref() == b"v"
                                || v.local_name().as_ref() == b"t" =>
                        {
                            value.push_str(&quick_xml::escape::unescape(
                                &cell_reader.read_text(v.name())?,
                            )?)
                        }
                        Event::Eof => break,
                        _ => {}
                    }
                }
                if shared_type {
                    value = shared
                        .get(value.parse::<usize>()?)
                        .context("invalid shared string index")?
                        .clone();
                }
                out.push_str(&value);
                out.push('\t');
            }
            Event::End(e) if e.local_name().as_ref() == b"row" => out.push('\n'),
            Event::DocType(_) => bail!("DTD is not supported"),
            Event::Eof => break,
            _ => {}
        }
        if out.len() > TEXT_LIMIT {
            bail!("worksheet text exceeds 2 MiB");
        }
    }
    Ok(out)
}

// pdf-extract accepts String or io::Write via adapters; bound each page writer.
struct BoundedBytes(Vec<u8>);
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + bytes.len() > TEXT_LIMIT {
            return Err(std::io::Error::other("PDF page text exceeds 2 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn pdf(bytes: &[u8], images: bool) -> Result<Extracted> {
    let doc = lopdf::Document::load_mem(bytes)?;
    if doc.is_encrypted() {
        bail!("encrypted PDF is not supported");
    }
    let pages = doc.get_pages();
    if pages.len() > 2048 {
        bail!("PDF exceeds 2048 pages");
    }
    let mut result = Extracted::new("pdf");
    let mut labels: HashMap<lopdf::ObjectId, Vec<String>> = HashMap::new();
    for (number, id) in pages {
        let mut text = BoundedBytes(Vec::new());
        let writer: &mut dyn Write = &mut text;
        let mut output = pdf_extract::PlainTextOutput::new(writer);
        pdf_extract::output_doc_page(&doc, &mut output, number)?;
        result.part(
            "page",
            &format!("Page {number}"),
            std::str::from_utf8(&text.0)?,
        )?;
        if !images {
            continue;
        }
        if let Ok(page_images) = doc.get_page_images(id) {
            for image in page_images {
                labels
                    .entry(image.id)
                    .or_default()
                    .push(format!("Page {number}"));
            }
        }
    }
    if images {
        let mut image_bytes = 0;
        // Inventory every image stream, including nested Form resources and
        // image masks; direct page references supply page labels where known.
        for (id, object) in &doc.objects {
            let Ok(stream) = object.as_stream() else {
                continue;
            };
            if stream
                .dict
                .get(b"Subtype")
                .and_then(lopdf::Object::as_name)
                .ok()
                != Some(b"Image")
            {
                continue;
            }
            let dict = &stream.dict;
            let filter = dict.get(b"Filter").ok();
            let filters = match filter {
                Some(lopdf::Object::Name(n)) => Some(vec![String::from_utf8_lossy(n).into_owned()]),
                Some(lopdf::Object::Array(a)) => Some(
                    a.iter()
                        .filter_map(|v| v.as_name().ok())
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .collect(),
                ),
                _ => None,
            };
            let image = lopdf::xobject::PdfImage {
                id: *id,
                width: dict.get(b"Width")?.as_i64()?,
                height: dict.get(b"Height")?.as_i64()?,
                color_space: dict
                    .get(b"ColorSpace")
                    .and_then(lopdf::Object::as_name)
                    .ok()
                    .map(|n| String::from_utf8_lossy(n).into_owned()),
                bits_per_component: dict
                    .get(b"BitsPerComponent")
                    .and_then(lopdf::Object::as_i64)
                    .ok(),
                filters,
                content: &stream.content,
                origin_dict: dict,
            };
            if result.images.len() >= 4096 {
                bail!("too many PDF images");
            }
            let mut format = "unsupported";
            let mut data = Vec::new();
            let filters = image.filters.clone().unwrap_or_default();
            if filters == ["DCTDecode"] {
                format = "jpg";
                data = image.content.to_vec();
            } else if filters == ["JPXDecode"] {
                format = if image.content.starts_with(&[0xff, 0x4f]) {
                    "j2k"
                } else {
                    "jp2"
                };
                data = image.content.to_vec();
            } else if (filters.is_empty() || filters == ["FlateDecode"])
                && image.bits_per_component == Some(8)
            {
                let color = match image.color_space.as_deref() {
                    Some("DeviceRGB") => Some((png::ColorType::Rgb, 3)),
                    Some("DeviceGray") => Some((png::ColorType::Grayscale, 1)),
                    _ => None,
                };
                if let Some((color, channels)) = color {
                    let expected = image
                        .width
                        .checked_mul(image.height)
                        .and_then(|n| n.checked_mul(channels))
                        .unwrap_or(i64::MAX);
                    if image.width > 0
                        && image.height > 0
                        && expected > 0
                        && expected <= PART_LIMIT as i64
                        && !image.origin_dict.has(b"Decode")
                        && !image.origin_dict.has(b"DecodeParms")
                    {
                        let raw = if filters.is_empty() {
                            image.content.to_vec()
                        } else {
                            doc.get_object(image.id)?
                                .as_stream()?
                                .decompressed_content()?
                        };
                        if raw.len() == expected as usize {
                            let mut encoder = png::Encoder::new(
                                &mut data,
                                image.width as u32,
                                image.height as u32,
                            );
                            encoder.set_color(color);
                            encoder.set_depth(png::BitDepth::Eight);
                            encoder.write_header()?.write_image_data(&raw)?;
                            format = "png";
                        }
                    }
                }
            }
            image_bytes += data.len();
            if image_bytes > INPUT_LIMIT {
                bail!("PDF image output exceeds 32 MiB");
            }
            result.images.push(Image {
                id: format!("{}:{}", image.id.0, image.id.1),
                format: format.into(),
                location: labels
                    .get(id)
                    .map(|pages| pages.join(", "))
                    .unwrap_or_else(|| format!("PDF object {}:{}", id.0, id.1)),
                status: if data.is_empty() {
                    "unsupported_encoding"
                } else {
                    "available"
                }
                .into(),
                bytes: data.len(),
                output_path: None,
                data,
            });
        }
    }
    if images {
        result.warnings.push("PDF image streams include nested resources and masks; unknown page locations use object IDs. Inline images, mask compositing and layout rendering are not reconstructed".into());
    }
    if result.text.trim().is_empty() {
        result
            .warnings
            .push("no_extractable_text; OCR is not implemented".into());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn archive(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in parts {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }
    #[test]
    fn word_unicode_entities_tables_and_images() {
        let xml = br#"<w:document xmlns:w="word"><w:body><w:p><w:r><w:t>Hello &amp; world</w:t><w:tab/><w:t>42</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Table cell</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>"#;
        let bytes = archive(&[
            ("word/document.xml", xml),
            ("word/media/image.png", b"original-image"),
        ]);
        let doc = extract(Path::new("a.docx"), &bytes, true).unwrap();
        assert!(doc.text.contains("Hello & world\t42"));
        assert!(doc.text.contains("Table cell"));
        assert_eq!(doc.images[0].data, b"original-image");
        assert_eq!(doc.locations[0].kind, "part");
        assert!(
            extract(Path::new("a.docx"), &bytes, false)
                .unwrap()
                .images
                .is_empty()
        );
        assert_eq!(
            xml_text("<w:p><w:t>한글 문서</w:t></w:p>".as_bytes(), false).unwrap(),
            "한글 문서\n"
        );
        assert!(
            xml_text(
                b"<!DOCTYPE a [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><t>&x;</t>",
                false
            )
            .is_err()
        );
    }
    #[test]
    fn slides_use_presentation_order_not_part_numbers() {
        let bytes = archive(&[
            ("ppt/presentation.xml", br#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst><p:sldId id="12" r:id="r2"/><p:sldId id="13" r:id="r1"/></p:sldIdLst></p:presentation>"#),
            ("ppt/_rels/presentation.xml.rels", br#"<Relationships><Relationship Id="r1" Target="slides/slide1.xml"/><Relationship Id="r2" Target="slides/slide2.xml"/></Relationships>"#),
            ("ppt/slides/slide1.xml", b"<p:sld><a:p><a:t>Second topic</a:t></a:p></p:sld>"),
            ("ppt/slides/slide2.xml", b"<p:sld><a:p><a:t>First topic</a:t></a:p></p:sld>"),
            ("ppt/media/image.jpeg", b"jpeg-image"),
        ]);
        let doc = extract(Path::new("a.pptx"), &bytes, true).unwrap();
        assert!(doc.text.find("First topic").unwrap() < doc.text.find("Second topic").unwrap());
        assert_eq!(doc.locations[0].label, "Slide 1");
        assert_eq!(doc.images.len(), 1);
        assert!(resolve_part("ppt", "../../../outside").is_err());
    }
    #[test]
    fn spreadsheet_shared_inline_and_numeric_values() {
        let bytes = archive(&[
            ("xl/workbook.xml", br#"<workbook xmlns:r="r"><sheets><sheet name="Budget" r:id="r1"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", br#"<Relationships><Relationship Id="r1" Target="worksheets/sheet1.xml"/></Relationships>"#),
            ("xl/sharedStrings.xml", b"<sst><si><t>Kyoto</t></si><si><r><t>Hotel </t></r><r><t>budget</t></r></si></sst>"),
            ("xl/worksheets/sheet1.xml", b"<worksheet><sheetData><row><c t='s'><v>1</v></c><c><v>42</v></c><c t='inlineStr'><is><t>Food &amp; drinks</t></is></c></row></sheetData></worksheet>"),
        ]);
        let doc = extract(Path::new("a.xlsx"), &bytes, false).unwrap();
        assert!(doc.text.contains("Hotel budget\t42\tFood & drinks"));
        assert_eq!(doc.locations[0].label, "Budget");
    }
    #[test]
    fn compressed_part_budget_and_odf_text() {
        let bytes = archive(&[("content.xml", b"<office:document><text:p>Travel <text:span>plan</text:span></text:p></office:document>"), ("Pictures/a.png", b"image")]);
        let doc = extract(Path::new("a.odt"), &bytes, true).unwrap();
        assert!(doc.text.contains("Travel plan"));
        assert_eq!(doc.images.len(), 1);
        let huge = vec![b'a'; PART_LIMIT + 1];
        let bytes = archive(&[("word/document.xml", &huge)]);
        assert!(extract(Path::new("a.docx"), &bytes, false).is_err());
    }
    #[test]
    fn declared_zip_expansion_is_rejected_before_decompression() {
        let mut bytes = archive(&[("word/document.xml", b"<t>safe</t>")]);
        let central = bytes.windows(4).position(|b| b == b"PK\x01\x02").unwrap();
        bytes[central + 24..central + 28].copy_from_slice(&(65u32 * 1024 * 1024).to_le_bytes());
        assert!(extract(Path::new("bomb.docx"), &bytes, false).is_err());
    }
    pub fn sample_pdf() -> Vec<u8> {
        use lopdf::{
            Object, Stream,
            content::{Content, Operation},
            dictionary,
        };
        let mut doc = lopdf::Document::with_version("1.5");
        let pages = doc.new_object_id();
        let font = doc.add_object(
            dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
        );
        let image = doc.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 1, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 }, vec![255,0,0]));
        let mut kids = Vec::new();
        for text in ["Database retry policy", "Kyoto travel budget"] {
            let content = Content {
                operations: vec![
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 12.into()]),
                    Operation::new("Td", vec![50.into(), 700.into()]),
                    Operation::new("Tj", vec![Object::string_literal(text)]),
                    Operation::new("ET", vec![]),
                ],
            }
            .encode()
            .unwrap();
            let content = doc.add_object(Stream::new(dictionary! {}, content));
            let page = doc.add_object(dictionary! { "Type"=>"Page", "Parent"=>pages, "Contents"=>content, "MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()], "Resources"=>dictionary! { "Font"=>dictionary! { "F1"=>font }, "XObject"=>dictionary! { "Im1"=>image } } });
            kids.push(Object::Reference(page));
        }
        doc.objects.insert(
            pages,
            dictionary! { "Type"=>"Pages", "Kids"=>kids, "Count"=>2 }.into(),
        );
        let catalog = doc.add_object(dictionary! { "Type"=>"Catalog", "Pages"=>pages });
        doc.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }
    #[test]
    fn pdf_pages_text_and_lossless_image_export() {
        let bytes = sample_pdf();
        let mut doc = extract(Path::new("a.pdf"), &bytes, true).unwrap();
        assert!(doc.text.contains("Database retry policy"));
        assert!(doc.text.contains("Kyoto travel budget"));
        assert_eq!(doc.locations.len(), 2);
        assert_eq!(doc.locations[1].label, "Page 2");
        assert_eq!(doc.images.len(), 1);
        assert_eq!(doc.images[0].format, "png");
        let dir = std::env::temp_dir().join(format!("weft-images-{}", std::process::id()));
        doc.export_images(&dir).unwrap();
        assert_eq!(doc.images[0].location, "Page 1, Page 2");
        doc.export_images(&dir).unwrap();
        let path = doc.images[0].output_path.as_ref().unwrap();
        let original = std::fs::read(path).unwrap();
        assert_eq!(original, doc.images[0].data);
        let mut decoder = png::Decoder::new(Cursor::new(&original))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; decoder.output_buffer_size().unwrap()];
        let info = decoder.next_frame(&mut pixels).unwrap();
        assert_eq!(&pixels[..info.buffer_size()], &[255, 0, 0]);
        std::fs::write(path, b"unrelated output").unwrap();
        assert!(doc.export_images(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
