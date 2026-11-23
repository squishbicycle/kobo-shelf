//! My Words: definitions and export.
//!
//! Definitions come from things already on this Mac: the system dictionary
//! (Dictionary.app's) for English, and `jdict` (JMdict) for Japanese.

use std::ffi::c_void;
use std::path::PathBuf;
use std::process::Command;

use crate::kobo::Word;

pub fn define(word: &Word) -> Option<String> {
    if word.lang.starts_with("ja") || word.text.chars().any(is_japanese) {
        jdict(&word.text)
    } else {
        // Words saved from the start of a sentence keep their capital.
        let lower = word.text.to_lowercase();
        system_dictionary(&word.text)
            .map(|d| tidy_system_definition(&word.text, &d))
            .or_else(|| (lower != word.text).then(|| system_dictionary(&lower).map(|d| tidy_system_definition(&lower, &d))).flatten())
    }
}

fn is_japanese(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}

/// "たべる — to eat; to live on (e.g. a salary)" from `jdict --json`.
fn jdict(text: &str) -> Option<String> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let bin = home.join(".local/bin/jdict");
    let out = Command::new(&bin).arg("--json").arg(text).output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let entry = v["entries"].get(0)?;
    let glosses: Vec<&str> = entry["senses"].as_array()?.iter().filter_map(|s| s["gloss"].as_str()).take(3).collect();
    if glosses.is_empty() {
        return None;
    }
    let word = entry["word"].as_str().unwrap_or(text);
    let reading = entry["reading"].as_str().filter(|r| *r != word);
    let head = match reading {
        Some(r) => format!("{word}【{r}】"),
        None => word.to_string(),
    };
    Some(format!("{head} — {}", glosses.join("; ")))
}

/// Drop the headword and pronunciation the system dictionary starts with, and
/// the etymology and derived forms it ends with.
fn tidy_system_definition(word: &str, d: &str) -> String {
    let mut d = d.trim();
    // "vertiginous ver·tig·i·nous | vərˈtijənəs | adjective causing …"
    if let Some(rest) = d.strip_prefix(word) {
        d = rest.trim_start();
    }
    if d.matches('|').count() >= 2 && d.find('|').is_some_and(|i| i < 40) {
        let second = d.match_indices('|').nth(1).unwrap().0;
        d = d[second + 1..].trim_start();
    }
    for marker in [" ORIGIN ", " DERIVATIVES ", " PHRASES ", " PHRASAL VERBS "] {
        if let Some(i) = d.find(marker) {
            d = &d[..i];
        }
    }
    d.trim().to_string()
}

#[repr(C)]
struct CFRange {
    location: isize,
    length: isize,
}

type CFStringRef = *const c_void;
const UTF8: u32 = 0x0800_0100;

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn DCSCopyTextDefinition(dictionary: *const c_void, text: CFStringRef, range: CFRange) -> CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(alloc: *const c_void, bytes: *const u8, len: isize, encoding: u32, external: u8) -> CFStringRef;
    fn CFStringGetLength(s: CFStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(len: isize, encoding: u32) -> isize;
    fn CFStringGetCString(s: CFStringRef, buf: *mut u8, size: isize, encoding: u32) -> u8;
    fn CFRelease(cf: *const c_void);
}

fn system_dictionary(text: &str) -> Option<String> {
    // SAFETY: plain Core Foundation calls; every object we create or copy is
    // released before returning, and the buffer is sized by CF itself.
    unsafe {
        let s = CFStringCreateWithBytes(std::ptr::null(), text.as_ptr(), text.len() as isize, UTF8, 0);
        if s.is_null() {
            return None;
        }
        let def = DCSCopyTextDefinition(std::ptr::null(), s, CFRange { location: 0, length: CFStringGetLength(s) });
        CFRelease(s);
        if def.is_null() {
            return None;
        }
        let size = CFStringGetMaximumSizeForEncoding(CFStringGetLength(def), UTF8) + 1;
        let mut buf = vec![0u8; size as usize];
        let ok = CFStringGetCString(def, buf.as_mut_ptr(), size, UTF8);
        CFRelease(def);
        if ok == 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        String::from_utf8(buf[..end].to_vec()).ok()
    }
}

/// Spreadsheet-friendly CSV with a header row.
pub fn to_csv(words: &[(Word, Option<String>)]) -> String {
    let field = |s: &str| {
        if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
    };
    let mut out = String::from("Word,Definition,Book,Language,Saved\r\n");
    for (w, def) in words {
        let cols = [&w.text, def.as_deref().unwrap_or(""), &w.book, &w.lang, w.saved.get(..10).unwrap_or(&w.saved)];
        out.push_str(&cols.iter().map(|c| field(c)).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}

/// Tab-separated with Anki's header lines, so File → Import picks up the
/// fields without any setup: front is the word, back the definition and book.
pub fn to_anki(words: &[(Word, Option<String>)]) -> String {
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ").replace('<', "&lt;");
    let mut out = String::from("#separator:tab\n#html:true\n#columns:Word\tDefinition\tBook\n");
    for (w, def) in words {
        let back = def.as_deref().map(clean).unwrap_or_default();
        out.push_str(&format!("{}\t{}\t<i>{}</i>\n", clean(&w.text), back, clean(&w.book)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, lang: &str) -> Word {
        Word { text: text.into(), lang: lang.into(), book: "Our Share of Night".into(), saved: "2026-09-03T23:18:02Z".into() }
    }

    #[test]
    fn tidies_system_definitions() {
        let raw = "vertiginous ver·tig·i·nous | vərˈtijənəs | adjective causing vertigo: drops below. ORIGIN early 17th century";
        assert_eq!(tidy_system_definition("vertiginous", raw), "adjective causing vertigo: drops below.");
    }

    #[test]
    fn looks_up_english_in_the_system_dictionary() {
        let d = define(&word("vertiginous", "en")).expect("system dictionary has it");
        assert!(d.contains("vertigo"), "{d}");
    }

    #[test]
    fn exports() {
        let rows = vec![(word("sangfroid", "en"), Some("noun composure, \"coolness\"".into())), (word("食べる", "ja"), None)];
        let csv = to_csv(&rows);
        assert_eq!(
            csv,
            "Word,Definition,Book,Language,Saved\r\nsangfroid,\"noun composure, \"\"coolness\"\"\",Our Share of Night,en,2026-09-03\r\n食べる,,Our Share of Night,ja,2026-09-03\r\n"
        );
        let anki = to_anki(&rows);
        assert!(anki.starts_with("#separator:tab\n#html:true\n"));
        assert!(anki.contains("sangfroid\tnoun composure, \"coolness\"\t<i>Our Share of Night</i>\n"));
    }
}
