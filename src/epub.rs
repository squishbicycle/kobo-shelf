//! Reading metadata and covers out of EPUB files.

use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::Path;

use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::Reader;
use zip::ZipArchive;

#[derive(Default, Debug)]
pub struct Meta {
    pub title: Option<String>,
    pub author: Option<String>,
    /// `opf:file-as` on the first creator, e.g. "Moore, Lane".
    pub author_sort: Option<String>,
    pub cover: Option<Vec<u8>>,
}

#[derive(Debug)]
pub struct Item {
    pub id: String,
    /// Full path inside the zip.
    pub path: String,
    pub media_type: String,
    pub properties: String,
}

#[derive(Default, Debug)]
pub struct Opf {
    pub title: Option<String>,
    pub author: Option<String>,
    pub author_sort: Option<String>,
    pub cover_id: Option<String>,
    pub items: Vec<Item>,
}

impl Opf {
    /// EPUB 3 `cover-image`, then EPUB 2 `<meta name="cover">`, then any image
    /// whose id or path mentions "cover".
    pub fn cover_item(&self) -> Option<&Item> {
        let is_image = |i: &&Item| i.media_type.starts_with("image/");
        self.items
            .iter()
            .filter(is_image)
            .find(|i| i.properties.split_whitespace().any(|p| p == "cover-image"))
            .or_else(|| {
                let id = self.cover_id.as_deref()?;
                self.items.iter().filter(is_image).find(|i| i.id == id)
            })
            .or_else(|| {
                self.items.iter().filter(is_image).find(|i| {
                    i.id.to_lowercase().contains("cover") || i.path.to_lowercase().contains("cover")
                })
            })
    }
}

pub fn read_meta(path: &Path, want_cover: bool) -> io::Result<Meta> {
    let mut zip = ZipArchive::new(File::open(path)?).map_err(io::Error::other)?;
    let opf_path = rootfile(&mut zip).ok_or_else(|| io::Error::other("no OPF in container.xml"))?;
    let opf_xml = read_string(&mut zip, &opf_path).ok_or_else(|| io::Error::other("missing OPF"))?;
    let opf = parse_opf(&opf_xml, &opf_path);
    let cover = if want_cover {
        opf.cover_item().and_then(|i| read_entry(&mut zip, &i.path))
    } else {
        None
    };
    Ok(Meta { title: opf.title, author: opf.author, author_sort: opf.author_sort, cover })
}

pub fn read_entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Option<Vec<u8>> {
    let mut f = zip.by_name(name).ok()?;
    let mut buf = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

pub fn read_string<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str) -> Option<String> {
    let bytes = read_entry(zip, name)?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Path of the OPF package document, from META-INF/container.xml.
pub fn rootfile<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Option<String> {
    let xml = read_string(zip, "META-INF/container.xml")?;
    let mut r = Reader::from_str(&xml);
    loop {
        match r.read_event().ok()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == "rootfile" => {
                if let Some(p) = attr(&e, "full-path") {
                    return Some(p);
                }
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

pub fn parse_opf(xml: &str, opf_path: &str) -> Opf {
    let dir = match opf_path.rfind('/') {
        Some(i) => &opf_path[..=i],
        None => "",
    };
    let mut opf = Opf::default();
    let mut r = Reader::from_str(xml);
    // Which metadata element we're collecting text for.
    let mut collecting: Option<&'static str> = None;
    let mut text = String::new();
    loop {
        let ev = match r.read_event() {
            Ok(ev) => ev,
            Err(_) => break,
        };
        match ev {
            Event::Start(e) => match e.local_name().as_ref() {
                "title" if opf.title.is_none() => {
                    collecting = Some("title");
                    text.clear();
                }
                "creator" if opf.author.is_none() => {
                    collecting = Some("creator");
                    text.clear();
                    opf.author_sort = attr(&e, "file-as").filter(|s| !s.trim().is_empty());
                }
                "item" => push_item(&mut opf, &e, dir),
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                "item" => push_item(&mut opf, &e, dir),
                "meta" if attr(&e, "name").as_deref() == Some("cover") => {
                    opf.cover_id = attr(&e, "content");
                }
                _ => {}
            },
            Event::Text(t) if collecting.is_some() => text.push_str(&t),
            Event::CData(t) if collecting.is_some() => text.push_str(&t),
            Event::GeneralRef(g) if collecting.is_some() => {
                if let Some(c) = resolve_ref(&g) {
                    text.push(c);
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                let value = Some(collapse_ws(&text)).filter(|s| !s.is_empty());
                match (collecting, name.as_ref()) {
                    (Some("title"), "title") => {
                        opf.title = value;
                        collecting = None;
                    }
                    (Some("creator"), "creator") => {
                        opf.author = value;
                        collecting = None;
                    }
                    _ => {}
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    opf
}

fn push_item(opf: &mut Opf, e: &BytesStart, dir: &str) {
    let (Some(id), Some(href)) = (attr(e, "id"), attr(e, "href")) else { return };
    opf.items.push(Item {
        id,
        path: resolve_href(dir, &href),
        media_type: attr(e, "media-type").unwrap_or_default(),
        properties: attr(e, "properties").unwrap_or_default(),
    });
}

/// An attribute's unescaped value, matched on its local name (so `opf:file-as`
/// matches "file-as").
pub fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .map(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map(|v| v.into_owned()).unwrap_or_else(|_| a.value.into_owned()))
}

pub fn resolve_ref(g: &BytesRef) -> Option<char> {
    if g.is_char_ref() {
        return g.resolve_char_ref().ok().flatten();
    }
    match &**g {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        _ => None,
    }
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Resolve an OPF-relative href to a zip entry path: strip any fragment,
/// percent-decode, and fold `.` and `..` segments.
pub fn resolve_href(dir: &str, href: &str) -> String {
    let href = href.split('#').next().unwrap_or(href);
    let joined = format!("{dir}{}", percent_decode(href));
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPF: &str = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
  <metadata>
    <dc:title>Devils, The &amp; Friends</dc:title>
    <dc:creator opf:file-as="Abercrombie, Joe" opf:role="aut">Joe  Abercrombie</dc:creator>
    <dc:creator>Someone Else</dc:creator>
    <meta name="cover" content="cov"/>
  </metadata>
  <manifest>
    <item id="cov" href="images/My%20Cover.jpg" media-type="image/jpeg"/>
    <item id="c1" href="../text/ch1.xhtml#top" media-type="application/xhtml+xml"/>
  </manifest>
</package>"#;

    #[test]
    fn parses_opf_metadata_and_cover() {
        let opf = parse_opf(OPF, "OEBPS/content.opf");
        assert_eq!(opf.title.as_deref(), Some("Devils, The & Friends"));
        assert_eq!(opf.author.as_deref(), Some("Joe Abercrombie"));
        assert_eq!(opf.author_sort.as_deref(), Some("Abercrombie, Joe"));
        assert_eq!(opf.cover_item().unwrap().path, "OEBPS/images/My Cover.jpg");
        assert_eq!(opf.items[1].path, "text/ch1.xhtml");
    }

    #[test]
    fn prefers_epub3_cover_image() {
        let xml = r#"<package><manifest>
            <item id="a" href="a.png" media-type="image/png"/>
            <item id="b" href="b.png" media-type="image/png" properties="cover-image"/>
        </manifest></package>"#;
        assert_eq!(parse_opf(xml, "content.opf").cover_item().unwrap().id, "b");
    }
}
