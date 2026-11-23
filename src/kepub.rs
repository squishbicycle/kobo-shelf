//! EPUB → kepub conversion.
//!
//! Kobo's kepub renderer wants every sentence wrapped in
//! `<span class="koboSpan" id="kobo.P.S">` (that's what drives page stats,
//! highlights and accurate progress), the body wrapped in `#book-columns` /
//! `#book-inner`, and a small style hack. This mirrors what Calibre and
//! kepubify produce. Everything else in the source is copied byte-for-byte.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::epub;

const STYLE_HACK: &str = r#"<style type="text/css" class="kobostylehacks">div#book-inner { margin-top: 0; margin-bottom: 0; }</style>"#;

/// Elements that start a new paragraph number.
const BLOCKS: &[&str] = &[
    "p", "h1", "h2", "h3", "h4", "h5", "h6", "li", "blockquote", "dd", "dt", "td", "th",
    "figcaption", "caption", "pre", "div",
];
/// Elements whose text is left alone. `rt`/`rp` are furigana.
const SKIP: &[&str] = &["script", "style", "svg", "math", "textarea", "noscript", "rt", "rp"];

#[derive(Debug, Default, PartialEq)]
pub struct Stats {
    pub converted: usize,
    /// Content documents that couldn't be parsed and were copied unchanged.
    pub skipped: usize,
}

/// Write a kepub version of the EPUB at `src` to `dst`.
pub fn convert(src: &Path, dst: &Path) -> io::Result<Stats> {
    let mut zip = ZipArchive::new(File::open(src)?).map_err(io::Error::other)?;
    if let Some(enc) = epub::read_string(&mut zip, "META-INF/encryption.xml") {
        // Font obfuscation is fine; anything using real encryption is DRM.
        if enc.contains("xmlenc#aes") {
            return Err(io::Error::other("this EPUB is DRM-protected"));
        }
    }
    let opf_path = epub::rootfile(&mut zip).ok_or_else(|| io::Error::other("no OPF in container.xml"))?;
    let opf_xml = epub::read_string(&mut zip, &opf_path).ok_or_else(|| io::Error::other("missing OPF"))?;
    let content: HashSet<String> = epub::parse_opf(&opf_xml, &opf_path)
        .items
        .into_iter()
        .filter(|i| i.media_type == "application/xhtml+xml")
        .map(|i| i.path)
        .collect();

    let mut w = ZipWriter::new(File::create(dst)?);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    w.start_file("mimetype", stored)?;
    w.write_all(b"application/epub+zip")?;

    let mut stats = Stats::default();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(io::Error::other)?;
        let name = f.name().map_err(io::Error::other)?.into_owned();
        if name == "mimetype" {
            continue;
        }
        if f.is_dir() || !content.contains(&name) {
            w.raw_copy_file(f)?;
            continue;
        }
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf)?;
        drop(f);
        let out = match std::str::from_utf8(&buf).ok().map(transform) {
            Some(Ok(Some(html))) => {
                stats.converted += 1;
                html.into_bytes()
            }
            Some(Ok(None)) => buf, // already a kepub document
            _ => {
                stats.skipped += 1;
                buf
            }
        };
        w.start_file(name, deflated)?;
        w.write_all(&out)?;
    }
    w.finish()?;
    Ok(stats)
}

/// Add Kobo spans to one XHTML document. `Ok(None)` means it already has them.
pub fn transform(src: &str) -> Result<Option<String>, quick_xml::Error> {
    if src.contains("koboSpan") {
        return Ok(None);
    }
    let mut r = Reader::from_str(src);
    let cfg = r.config_mut();
    cfg.check_end_names = false;
    cfg.allow_unmatched_ends = true;

    let mut out = String::with_capacity(src.len() + src.len() / 3);
    let mut stack: Vec<String> = Vec::new();
    let mut in_body = false;
    let (mut para, mut seg) = (0u32, 0u32);
    // Start of a run of text/entity events not yet written.
    let mut text_start: Option<usize> = None;

    loop {
        let before = r.buffer_position() as usize;
        let ev = r.read_event()?;
        let after = r.buffer_position() as usize;
        if matches!(ev, Event::Text(_) | Event::GeneralRef(_)) {
            text_start.get_or_insert(before);
            continue;
        }
        if let Some(start) = text_start.take() {
            let text = &src[start..before];
            let skipping = stack.iter().any(|n| SKIP.contains(&n.as_str()));
            if in_body && !skipping && !text.trim().is_empty() {
                if para == 0 {
                    para = 1;
                }
                for sentence in split_sentences(text) {
                    seg += 1;
                    out.push_str(&format!(r#"<span class="koboSpan" id="kobo.{para}.{seg}">{sentence}</span>"#));
                }
            } else {
                out.push_str(text);
            }
        }
        let raw = &src[before..after];
        match ev {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_ascii_lowercase();
                out.push_str(raw);
                if name == "body" {
                    in_body = true;
                    out.push_str(r#"<div id="book-columns"><div id="book-inner">"#);
                } else if in_body && BLOCKS.contains(&name.as_str()) {
                    para += 1;
                    seg = 0;
                }
                stack.push(name);
            }
            Event::End(e) => {
                let name = e.local_name().as_ref().to_ascii_lowercase();
                match name.as_str() {
                    "head" => out.push_str(STYLE_HACK),
                    "body" if in_body => {
                        out.push_str("</div></div>");
                        in_body = false;
                    }
                    _ => {}
                }
                out.push_str(raw);
                if let Some(i) = stack.iter().rposition(|n| *n == name) {
                    stack.truncate(i);
                }
            }
            Event::Empty(e) => {
                let skipping = stack.iter().any(|n| SKIP.contains(&n.as_str()));
                if in_body && !skipping && e.local_name().as_ref().eq_ignore_ascii_case("img") {
                    para += 1;
                    seg = 1;
                    out.push_str(&format!(r#"<span class="koboSpan" id="kobo.{para}.1">{raw}</span>"#));
                } else {
                    out.push_str(raw);
                }
            }
            Event::Eof => break,
            _ => out.push_str(raw),
        }
    }
    Ok(Some(out))
}

/// Split raw (still-escaped) text into sentences. Trailing whitespace stays
/// with the sentence before it, so concatenating the pieces gives back `text`.
fn split_sentences(text: &str) -> Vec<&str> {
    const ENDS: &[char] = &['.', '!', '?', '…'];
    const CJK_ENDS: &[char] = &['。', '！', '？'];
    const CLOSERS: &[char] = &['"', '\'', ')', ']', '»', '”', '’', '」', '』', '）'];

    let mut pieces = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        let cjk = CJK_ENDS.contains(&c);
        if !cjk && !ENDS.contains(&c) {
            continue;
        }
        // Swallow repeated terminators and closing quotes/brackets, including
        // ones written as entities like `&#8221;`.
        loop {
            match chars.peek() {
                Some(&(_, n)) if ENDS.contains(&n) || CJK_ENDS.contains(&n) || CLOSERS.contains(&n) => {
                    chars.next();
                }
                Some(&(i, '&')) => match entity_len(&text[i..]) {
                    Some(len) => {
                        while chars.peek().is_some_and(|&(j, _)| j < i + len) {
                            chars.next();
                        }
                    }
                    None => break,
                },
                _ => break,
            }
        }
        let has_ws = chars.peek().is_some_and(|&(_, n)| n.is_whitespace());
        if !cjk && !has_ws {
            continue;
        }
        while let Some(&(_, n)) = chars.peek() {
            if !n.is_whitespace() {
                break;
            }
            chars.next();
        }
        let end = chars.peek().map_or(text.len(), |&(i, _)| i);
        pieces.push(&text[start..end]);
        start = end;
    }
    if start < text.len() {
        if text[start..].trim().is_empty() && !pieces.is_empty() {
            // Fold trailing whitespace into the last sentence.
            let last = pieces.pop().unwrap();
            let from = last.as_ptr() as usize - text.as_ptr() as usize;
            pieces.push(&text[from..]);
        } else {
            pieces.push(&text[start..]);
        }
    }
    pieces
}

/// Length of an entity like `&amp;` or `&#8221;` at the start of `s`.
fn entity_len(s: &str) -> Option<usize> {
    let semi = s[1..].find(';')? + 1;
    let body = &s[1..semi];
    (semi <= 10 && !body.is_empty() && body.chars().all(|c| c.is_ascii_alphanumeric() || c == '#')).then_some(semi + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sentences_losslessly() {
        let t = "He said “Go.” Then left! Really?  Yes. 3.14 is pi";
        let p = split_sentences(t);
        assert_eq!(p, vec!["He said “Go.” ", "Then left! ", "Really?  ", "Yes. ", "3.14 is pi"]);
        assert_eq!(p.concat(), t);
    }

    #[test]
    fn splits_japanese_and_entities() {
        assert_eq!(split_sentences("銀河だ。「本当？」はい"), vec!["銀河だ。", "「本当？」", "はい"]);
        assert_eq!(split_sentences("&#8220;Hi.&#8221; Bye. "), vec!["&#8220;Hi.&#8221; ", "Bye. "]);
        assert_eq!(split_sentences("Tom &amp; Jerry."), vec!["Tom &amp; Jerry."]);
    }

    #[test]
    fn transforms_document() {
        let src = r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>T. Title</title><style>p{}</style></head>
<body class="b"><h1>One</h1><p>First. Second &amp; third.</p>
<p><img src="a.jpg"/></p><p>Ruby <ruby>漢字<rt>かんじ</rt></ruby></p><br/></body></html>"#;
        let out = transform(src).unwrap().unwrap();
        assert!(out.contains(&format!("{STYLE_HACK}</head>")));
        assert!(out.contains(r#"<body class="b"><div id="book-columns"><div id="book-inner"><h1><span class="koboSpan" id="kobo.1.1">One</span></h1>"#));
        assert!(out.contains(r#"<p><span class="koboSpan" id="kobo.2.1">First. </span><span class="koboSpan" id="kobo.2.2">Second &amp; third.</span></p>"#));
        assert!(out.contains(r#"<span class="koboSpan" id="kobo.4.1"><img src="a.jpg"/></span>"#));
        assert!(out.contains("<rt>かんじ</rt>"), "furigana untouched");
        assert!(out.contains("<title>T. Title</title>"), "head text untouched");
        assert!(out.contains("<br/></div></div></body>"));
        // Whitespace between blocks isn't wrapped, and output is still well-formed.
        assert!(!out.contains(r#"">
</span>"#));
        let mut r = Reader::from_str(&out);
        while !matches!(r.read_event().unwrap(), Event::Eof) {}
        assert_eq!(transform(&out).unwrap(), None, "already converted");
    }

    /// `KOBO_SHELF_EPUB=/path/book.epub cargo test convert_real_epub -- --ignored`
    #[test]
    #[ignore]
    fn convert_real_epub() {
        let src = std::path::PathBuf::from(std::env::var("KOBO_SHELF_EPUB").unwrap());
        let dst = src.with_extension("kepub.epub");
        let stats = convert(&src, &dst).unwrap();
        eprintln!("{stats:?}");
        assert_eq!(stats.skipped, 0);
        let mut zip = ZipArchive::new(File::open(&dst).unwrap()).unwrap();
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).unwrap();
            let name = f.name().unwrap().into_owned();
            if name.ends_with("html") {
                let mut s = String::new();
                f.read_to_string(&mut s).unwrap();
                let mut r = Reader::from_str(&s);
                loop {
                    match r.read_event() {
                        Ok(Event::Eof) => break,
                        Ok(_) => {}
                        Err(e) => panic!("{name}: {e}"),
                    }
                }
            }
        }
    }
}
