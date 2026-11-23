//! Everything that touches the Kobo itself: finding it, reading its library
//! database, and adding or removing books.
//!
//! Reads always go through a copy of `KoboReader.sqlite`. Writes to the real
//! database all go through [`write`], one transaction each, and the app takes
//! a backup before the first one each time the Kobo is connected.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{params, Connection};

use crate::{epub, kepub};

/// How the Kobo refers to its own storage in content IDs.
pub const ONBOARD: &str = "file:///mnt/onboard/";
/// File types the Kobo can open. `.kepub.epub` is covered by `.epub`.
pub const BOOK_EXTS: &[&str] = &[".epub", ".pdf", ".mobi", ".cbz", ".cbr", ".txt", ".html", ".rtf"];
const COVER_SUFFIXES: &[&str] = &[" - N3_LIBRARY_FULL.parsed", " - N3_FULL.parsed", " - N3_LIBRARY_GRID.parsed"];

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub mount: PathBuf,
    pub serial: String,
    pub firmware: String,
}

impl Device {
    pub fn db_path(&self) -> PathBuf {
        self.mount.join(".kobo/KoboReader.sqlite")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Added over USB (by this app, Calibre or Finder).
    Sideloaded,
    /// Bought from Kobo; managed by the Kobo account.
    Store,
    /// A sample Kobo loaded at the factory under its own demo account.
    Preview,
    /// Copied over but not imported yet. The Kobo imports it after ejecting.
    Pending,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Unread,
    Reading(f32),
    Finished,
}

#[derive(Clone, Debug)]
pub struct Book {
    pub content_id: String,
    pub title: String,
    pub author: String,
    pub status: Status,
    /// ISO timestamp, or empty.
    pub last_read: String,
    pub image_id: String,
    pub kind: Kind,
    pub path: Option<PathBuf>,
    /// Empty when the book isn't in a series.
    pub series: String,
    pub series_number: String,
    /// Seconds, as tracked by the Kobo.
    pub time_spent: u64,
    /// The Kobo's estimate of reading time left, in seconds.
    pub time_left: u64,
}

impl Book {
    pub fn progress(&self) -> f32 {
        match self.status {
            Status::Unread => 0.0,
            Status::Reading(p) => p,
            Status::Finished => 1.0,
        }
    }

    pub fn is_epub(&self) -> bool {
        self.path.as_ref().is_some_and(|p| p.to_string_lossy().to_lowercase().ends_with(".epub"))
    }

    /// Whether the Kobo knows about it yet, so it can have a read status,
    /// series and collections.
    pub fn in_db(&self) -> bool {
        self.kind != Kind::Pending
    }
}

/// A word saved to My Words on the Kobo.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    /// Dictionary language, e.g. "en" or "ja".
    pub lang: String,
    /// Title of the book it was looked up in.
    pub book: String,
    pub saved: String,
}

/// A collection (the database calls them shelves).
#[derive(Clone, Debug, PartialEq)]
pub struct Shelf {
    /// What `ShelfContent` refers to it by.
    pub key: String,
    pub name: String,
    pub books: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Library {
    pub books: Vec<Book>,
    pub words: Vec<Word>,
    pub shelves: Vec<Shelf>,
}

/// The first mounted volume that looks like a Kobo.
pub fn find_device() -> Option<Device> {
    let mut vols: Vec<PathBuf> = fs::read_dir("/Volumes").ok()?.flatten().map(|e| e.path()).collect();
    vols.sort();
    vols.into_iter().find(|v| v.join(".kobo/KoboReader.sqlite").is_file()).map(|mount| device_at(&mount))
}

pub fn device_at(mount: &Path) -> Device {
    // .kobo/version: serial,?,firmware,...
    let version = fs::read_to_string(mount.join(".kobo/version")).unwrap_or_default();
    let fields: Vec<&str> = version.trim().split(',').collect();
    Device {
        mount: mount.to_path_buf(),
        serial: fields.first().unwrap_or(&"").to_string(),
        firmware: fields.get(2).unwrap_or(&"").to_string(),
    }
}

/// Read the library from a copy of the database, plus any book files waiting
/// to be imported.
pub fn load_library(dev: &Device, work_dir: &Path) -> io::Result<Library> {
    fs::create_dir_all(work_dir)?;
    let copy = work_dir.join("KoboReader.sqlite");
    for ext in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(with_suffix(&copy, ext));
    }
    fs::copy(dev.db_path(), &copy)?;
    let wal = with_suffix(&dev.db_path(), "-wal");
    if wal.is_file() {
        fs::copy(&wal, with_suffix(&copy, "-wal"))?;
    }
    let conn = Connection::open(&copy).map_err(io::Error::other)?;
    let mut books = query_books(&conn, &dev.mount).map_err(io::Error::other)?;
    let known: HashSet<String> = conn
        .prepare("select ContentID from content where BookID is null")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .map_err(io::Error::other)?;
    // Older firmware may lack these tables; treat that as empty.
    let words = query_words(&conn).unwrap_or_default();
    let shelves = query_shelves(&conn).unwrap_or_default();
    drop(conn);

    for path in book_files(&dev.mount) {
        let id = content_id(&dev.mount, &path);
        if known.contains(&id) {
            continue;
        }
        let meta = epub::read_meta(&path, false).unwrap_or_default();
        let stem = file_stem(&path);
        books.push(Book {
            content_id: id,
            title: meta.title.unwrap_or(stem),
            author: meta.author.unwrap_or_default(),
            status: Status::Unread,
            last_read: String::new(),
            image_id: String::new(),
            kind: Kind::Pending,
            path: Some(path),
            series: String::new(),
            series_number: String::new(),
            time_spent: 0,
            time_left: 0,
        });
    }
    Ok(Library { books, words, shelves })
}

fn query_words(conn: &Connection) -> rusqlite::Result<Vec<Word>> {
    let mut stmt = conn.prepare(
        "select w.Text, coalesce(w.DictSuffix, ''), coalesce(w.DateCreated, ''), coalesce(c.Title, '')
         from WordList w left join content c on c.ContentID = w.VolumeId and c.BookID is null
         order by w.DateCreated desc",
    )?;
    let rows = stmt.query_map([], |r| {
        let suffix: String = r.get(1)?;
        Ok(Word { text: r.get(0)?, lang: suffix.trim_start_matches('-').to_string(), saved: r.get(2)?, book: r.get(3)? })
    })?;
    rows.collect()
}

fn query_shelves(conn: &Connection) -> rusqlite::Result<Vec<Shelf>> {
    let mut stmt = conn.prepare(
        "select coalesce(InternalName, Name), coalesce(Name, InternalName) from Shelf
         where coalesce(_IsDeleted, 'false') not in ('true', 1) order by Name collate nocase",
    )?;
    let mut shelves: Vec<Shelf> = stmt
        .query_map([], |r| Ok(Shelf { key: r.get(0)?, name: r.get(1)?, books: Vec::new() }))?
        .collect::<rusqlite::Result<_>>()?;
    let mut stmt = conn.prepare(
        "select ShelfName, ContentId from ShelfContent where coalesce(_IsDeleted, 'false') not in ('true', 1)",
    )?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (shelf, id) = row?;
        if let Some(s) = shelves.iter_mut().find(|s| s.key == shelf) {
            s.books.push(id);
        }
    }
    Ok(shelves)
}

fn query_books(conn: &Connection, mount: &Path) -> rusqlite::Result<Vec<Book>> {
    let mut stmt = conn.prepare(
        "select ContentID, coalesce(Title, ''), coalesce(Attribution, ''), coalesce(ReadStatus, 0),
                coalesce(___PercentRead, 0), coalesce(DateLastRead, ''), coalesce(ImageId, ''),
                coalesce(Accessibility, 0), coalesce(Series, ''), coalesce(SeriesNumber, ''),
                coalesce(TimeSpentReading, 0), coalesce(RestOfBookEstimate, 0)
         from content
         where ContentType = 6 and BookID is null and IsDownloaded in ('true', 1)",
    )?;
    let rows = stmt.query_map([], |r| {
        let id: String = r.get(0)?;
        let read_status: i64 = r.get(3)?;
        let percent: i64 = r.get(4)?;
        let accessibility: i64 = r.get(7)?;
        let (kind, path) = match id.strip_prefix(ONBOARD) {
            Some(rel) => (Kind::Sideloaded, Some(mount.join(rel))),
            None if id.starts_with("file://") => (Kind::Sideloaded, None),
            None => (
                if accessibility == 6 { Kind::Preview } else { Kind::Store },
                Some(mount.join(".kobo/kepub").join(&id)),
            ),
        };
        Ok(Book {
            title: r.get(1)?,
            author: r.get(2)?,
            status: match read_status {
                2 => Status::Finished,
                1 if percent > 0 => Status::Reading(percent as f32 / 100.0),
                _ => Status::Unread,
            },
            last_read: r.get(5)?,
            image_id: r.get(6)?,
            kind,
            path,
            series: r.get(8)?,
            series_number: r.get(9)?,
            time_spent: r.get::<_, i64>(10)?.max(0) as u64,
            time_left: r.get::<_, i64>(11)?.max(0) as u64,
            content_id: id,
        })
    })?;
    let mut books = Vec::new();
    for b in rows {
        let b = b?;
        // A sideloaded file that's gone was removed; the Kobo drops its entry
        // when it next imports.
        if b.kind == Kind::Sideloaded && !b.path.as_ref().is_some_and(|p| p.is_file()) {
            continue;
        }
        books.push(b);
    }
    Ok(books)
}

/// Book files on the device, skipping hidden folders and macOS `._` files.
pub fn book_files(mount: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![mount.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                dirs.push(e.path());
            } else if ft.is_file() && BOOK_EXTS.iter().any(|x| name.ends_with(x)) {
                out.push(e.path());
            }
        }
    }
    out
}

pub fn content_id(mount: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(mount).unwrap_or(path);
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    format!("{ONBOARD}{}", parts.join("/"))
}

/// The cover image to show: the EPUB's own (full colour) for books on the
/// drive, falling back to the Kobo's cached thumbnail (greyscale).
pub fn cover_bytes(dev: &Device, book: &Book) -> Option<Vec<u8>> {
    if book.kind != Kind::Store && book.is_epub() {
        if let Some(c) = book.path.as_ref().and_then(|p| epub::read_meta(p, true).ok()).and_then(|m| m.cover) {
            return Some(c);
        }
    }
    cached_cover_paths(&dev.mount, &book.image_id).into_iter().find_map(|p| fs::read(p).ok())
}

/// Kobo's cover cache lives in `.kobo-images/<a>/<b>/`, where a and b come
/// from a Qt-style hash of the ImageId (same scheme Calibre uses).
pub fn cached_cover_paths(mount: &Path, image_id: &str) -> Vec<PathBuf> {
    if image_id.is_empty() {
        return Vec::new();
    }
    let h = qhash(image_id);
    let dir = mount.join(".kobo-images").join((h & 0xff).to_string()).join(((h & 0xff00) >> 8).to_string());
    COVER_SUFFIXES.iter().map(|s| dir.join(format!("{image_id}{s}"))).collect()
}

fn qhash(s: &str) -> u32 {
    let mut h: u32 = 0;
    for &b in s.as_bytes() {
        h = (h << 4).wrapping_add(b as u32);
        h ^= (h & 0xf000_0000) >> 23;
        h &= 0x0fff_ffff;
    }
    h
}

/// Copy the database (and its WAL, if any) into `dir`, keeping the newest 20.
pub fn backup_db(dev: &Device, dir: &Path) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let dest = dir.join(format!("KoboReader-{}-{stamp}.sqlite", dev.serial));
    fs::copy(dev.db_path(), &dest)?;
    let wal = with_suffix(&dev.db_path(), "-wal");
    if wal.is_file() {
        fs::copy(&wal, with_suffix(&dest, "-wal"))?;
    }
    let mut old: Vec<PathBuf> = fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sqlite"))
        .collect();
    old.sort();
    while old.len() > 20 {
        let p = old.remove(0);
        let _ = fs::remove_file(with_suffix(&p, "-wal"));
        let _ = fs::remove_file(p);
    }
    Ok(dest)
}

/// Remove a sideloaded or pending book: its file, its database rows and its
/// cached covers. Store books are refused; they belong to the Kobo account and
/// would sync straight back. Previews go through `remove_previews`.
pub fn remove_book(dev: &Device, book: &Book) -> io::Result<()> {
    match book.kind {
        Kind::Store => return Err(io::Error::other("Kobo Store books have to be removed on the Kobo itself")),
        Kind::Preview => return Err(io::Error::other("previews are removed with remove_previews")),
        Kind::Sideloaded | Kind::Pending => {}
    }
    if let Some(path) = &book.path {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let _ = fs::remove_file(apple_double(path));
        remove_empty_parent(&dev.mount, path);
    }
    if book.kind == Kind::Sideloaded {
        remove_rows(&dev.db_path(), &book.content_id).map_err(io::Error::other)?;
        for p in cached_cover_paths(&dev.mount, &book.image_id) {
            let _ = fs::remove_file(&p);
            let _ = fs::remove_file(apple_double(&p));
        }
    }
    Ok(())
}

/// Run `f` in one transaction on the Kobo's database, then fold the WAL back
/// into the main file so nothing is left half-written when it's ejected.
pub fn write<T>(db: &Path, f: impl FnOnce(&rusqlite::Transaction) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let mut conn = Connection::open(db)?;
    let tx = conn.transaction()?;
    let out = f(&tx)?;
    tx.commit()?;
    conn.query_row("pragma wal_checkpoint(truncate)", [], |_| Ok(()))?;
    conn.close().map_err(|(_, e)| e)?;
    Ok(out)
}

/// The same statements Calibre's Kobo driver runs, plus dropping the book from
/// collections. The book's own row is kept but reset; the Kobo deletes it at
/// its next import once the file is gone.
fn remove_rows(db: &Path, content_id: &str) -> rusqlite::Result<()> {
    write(db, |tx| {
        tx.execute("delete from volume_shortcovers where volumeid = ?1", params![content_id])?;
        tx.execute("delete from content_keys where volumeid = ?1", params![content_id])?;
        tx.execute("delete from content where BookID = ?1", params![content_id])?;
        tx.execute(
            "update content set ReadStatus = 0, FirstTimeReading = 'true', ___PercentRead = 0, ___ExpirationStatus = 3
             where BookID is null and ContentID = ?1",
            params![content_id],
        )?;
        tx.execute("delete from ShelfContent where ContentId = ?1", params![content_id])?;
        Ok(())
    })
}

/// Remove factory-loaded previews. They belong to Kobo's demo account, so no
/// sync will ever tidy up after them: unlike `remove_book`, every row that
/// refers to them goes (the book, its chapters and sections, cover snippets
/// and home-screen tiles). Their files are moved into `stash` rather than
/// deleted, so restoring the database backup is a complete undo.
pub fn remove_previews(dev: &Device, books: &[Book], stash: &Path) -> io::Result<usize> {
    let books: Vec<&Book> = books.iter().filter(|b| b.kind == Kind::Preview).collect();
    if books.is_empty() {
        return Ok(0);
    }
    fs::create_dir_all(stash.join("kepub"))?;
    fs::create_dir_all(stash.join("images"))?;
    for b in &books {
        if let Some(p) = b.path.as_ref().filter(|p| p.is_file()) {
            copy_plain(p, &stash.join("kepub").join(&b.content_id))?;
        }
        for p in cached_cover_paths(&dev.mount, &b.image_id).iter().filter(|p| p.is_file()) {
            copy_plain(p, &stash.join("images").join(p.file_name().unwrap()))?;
        }
    }
    let ids: Vec<&str> = books.iter().map(|b| b.content_id.as_str()).collect();
    delete_preview_rows(&dev.db_path(), &ids).map_err(io::Error::other)?;
    for b in &books {
        if let Some(p) = &b.path {
            let _ = fs::remove_file(p);
            let _ = fs::remove_file(apple_double(p));
        }
        for p in cached_cover_paths(&dev.mount, &b.image_id) {
            let _ = fs::remove_file(&p);
            let _ = fs::remove_file(apple_double(&p));
        }
    }
    Ok(books.len())
}

fn delete_preview_rows(db: &Path, ids: &[&str]) -> rusqlite::Result<()> {
    write(db, |tx| {
        for id in ids {
            tx.execute("delete from content where BookID = ?1", [id])?;
            tx.execute("delete from content where ContentID = ?1", [id])?;
            tx.execute("delete from volume_shortcovers where volumeId = ?1", [id])?;
            tx.execute("delete from content_keys where volumeId = ?1", [id])?;
            tx.execute("delete from Activity where Id = ?1", [id])?;
            tx.execute("delete from ShelfContent where ContentId = ?1", [id])?;
        }
        Ok(())
    })
}

fn remove_empty_parent(mount: &Path, path: &Path) {
    let Some(dir) = path.parent() else { return };
    if dir == mount || !dir.starts_with(mount) {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    let names: Vec<_> = entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    if names.iter().all(|n| n.starts_with("._") || n == ".DS_Store") {
        for n in names {
            let _ = fs::remove_file(dir.join(n));
        }
        let _ = fs::remove_dir(dir);
        let _ = fs::remove_file(apple_double(dir));
    }
}

#[derive(Debug)]
pub struct Added {
    pub note: Option<String>,
}

/// Copy a book onto the Kobo as `Author Sort/Title - Author.ext`, converting
/// plain EPUBs to kepub when `convert` is set. The Kobo imports it on eject.
pub fn add_book(dev: &Device, src: &Path, convert: bool) -> io::Result<Added> {
    let lower = src.to_string_lossy().to_lowercase();
    let Some(ext) = BOOK_EXTS.iter().find(|x| lower.ends_with(*x)) else {
        return Err(io::Error::other("not a format the Kobo can read"));
    };
    let is_kepub = lower.ends_with(".kepub.epub");
    let plain_epub = *ext == ".epub" && !is_kepub;
    let meta = if *ext == ".epub" { epub::read_meta(src, false).ok() } else { None }.unwrap_or_default();

    let author = meta.author.clone().unwrap_or_default();
    let folder = sanitize(meta.author_sort.as_deref().or(meta.author.as_deref()).unwrap_or("Unknown"));
    let base = match &meta.title {
        Some(t) if !author.is_empty() => sanitize(&format!("{t} - {author}")),
        Some(t) => sanitize(t),
        None => sanitize(file_stem(src).trim_end_matches(".kepub")),
    };
    let dir = dev.mount.join(folder);
    let out_ext = if is_kepub || (plain_epub && convert) { ".kepub.epub" } else { ext };
    let dest = dir.join(format!("{base}{out_ext}"));
    let alt = dir.join(format!("{base}{}", if out_ext == ".kepub.epub" { ".epub" } else { ".kepub.epub" }));
    if dest.exists() || (*ext == ".epub" && alt.exists()) {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "already on your Kobo"));
    }
    fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(".{base}.part"));

    let mut note = None;
    let mut final_dest = dest;
    let result = (|| {
        if plain_epub && convert {
            match kepub::convert(src, &tmp) {
                Ok(stats) if stats.skipped > 0 => {
                    note = Some(format!("{} chapter(s) couldn't be converted and were left as-is", stats.skipped));
                }
                Ok(_) => {}
                Err(e) if e.to_string().contains("DRM") => return Err(e),
                Err(e) => {
                    note = Some(format!("kepub conversion failed ({e}), so it was copied as a regular EPUB"));
                    final_dest = dir.join(format!("{base}.epub"));
                    copy_plain(src, &tmp)?;
                }
            }
        } else {
            copy_plain(src, &tmp)?;
        }
        File::open(&tmp)?.sync_all()?;
        fs::rename(&tmp, &final_dest)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        remove_empty_parent(&dev.mount, &tmp);
        return Err(e);
    }
    Ok(Added { note })
}

/// Copy file contents only. `fs::copy` on macOS also copies extended
/// attributes, which is what litters FAT drives with `._` files.
fn copy_plain(src: &Path, dst: &Path) -> io::Result<()> {
    let mut from = File::open(src)?;
    let mut to = File::create(dst)?;
    io::copy(&mut from, &mut to)?;
    to.sync_all()
}

/// Delete the `._` files macOS leaves on the drive. Returns how many.
pub fn clean_mac_clutter(mount: &Path) -> usize {
    let mut count = 0;
    let mut dirs = vec![mount.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if !matches!(name.as_str(), ".Spotlight-V100" | ".fseventsd" | ".Trashes") {
                    dirs.push(e.path());
                }
            } else if name.starts_with("._") && fs::remove_file(e.path()).is_ok() {
                count += 1;
            }
        }
    }
    count
}

pub fn eject(dev: &Device) -> io::Result<()> {
    let out = Command::new("/usr/sbin/diskutil").arg("eject").arg(&dev.mount).output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(String::from_utf8_lossy(&out.stderr).trim().to_string()))
    }
}

/// Make a string safe as a FAT32 file name, the way Calibre does.
pub fn sanitize(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .take(100)
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim_start_matches('.').trim().to_string();
    if cleaned.is_empty() { "Untitled".into() } else { cleaned }
}

fn file_stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn apple_double(p: &Path) -> PathBuf {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    p.with_file_name(format!("._{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qhash_matches_device_layout() {
        // Checked against .kobo-images on a real Kobo Clara.
        let h = qhash("cfd3abfa-666f-4da8-acc2-62cbf255588f");
        assert_eq!((h & 0xff, (h & 0xff00) >> 8), (166, 6));
        let h = qhash("f80ebb1b-b146-4316-a01b-2422d4db1281");
        assert_eq!((h & 0xff, (h & 0xff00) >> 8), (49, 191));
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize("James: A Novel - Percival Everett"), "James_ A Novel - Percival Everett");
        assert_eq!(sanitize(" ..hidden. "), "hidden");
        assert_eq!(sanitize("???"), "___");
        assert_eq!(sanitize(""), "Untitled");
    }

    /// A fake Kobo in a temp folder: the tables we touch, in WAL mode like the
    /// real thing, with one sideloaded book (plus chapter rows) and one store book.
    fn fake_device(name: &str) -> Device {
        let mount = std::env::temp_dir().join(format!("kobo-shelf-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&mount);
        fs::create_dir_all(mount.join(".kobo/kepub")).unwrap();
        fs::write(mount.join(".kobo/version"), "N123,4.9.77,4.42.23291,4.9.77,4.9.77,00000000").unwrap();
        let conn = Connection::open(mount.join(".kobo/KoboReader.sqlite")).unwrap();
        conn.execute_batch(
            "pragma journal_mode = wal;
             create table content (ContentID text, ContentType int, BookID text, Title text, Attribution text,
                 ReadStatus int, ___PercentRead int, DateLastRead text, ImageId text, IsDownloaded text,
                 FirstTimeReading text, ___ExpirationStatus int, Accessibility int, Series text,
                 SeriesNumber text, SeriesID text, SeriesNumberFloat real, TimeSpentReading int,
                 RestOfBookEstimate int, ChapterIDBookmarked text);
             create table volume_shortcovers (volumeId text, shortcoverId text);
             create table content_keys (volumeId text, elementId text);
             create table Activity (Id text, Type text);
             create table Shelf (CreationDate text, Id text primary key, InternalName text, LastModified text,
                 Name text, Type text, _IsDeleted bool, _IsVisible bool, _IsSynced bool, _SyncTime text,
                 LastAccessed text);
             create table ShelfContent (ShelfName text, ContentId text, DateModified text, _IsDeleted bool,
                 _IsSynced bool, primary key (ShelfName, ContentId));
             create table WordList (Text text primary key, VolumeId text, DictSuffix text, DateCreated text);",
        )
        .unwrap();
        let id = "file:///mnt/onboard/Doe, Jane/Old Book - Jane Doe.kepub.epub";
        conn.execute(
            "insert into content (ContentID, ContentType, BookID, Title, Attribution, ReadStatus, ___PercentRead, DateLastRead, ImageId, IsDownloaded, FirstTimeReading, ___ExpirationStatus, Accessibility, Series) values (?1, 6, null, 'Old Book', 'Jane Doe', 1, 40, '2026-10-01', 'img1', 'true', 'false', 0, -1, null)",
            [id],
        )
        .unwrap();
        for ch in ["c1", "c2"] {
            let cid = format!("{id}!!{ch}");
            conn.execute("insert into content (ContentID, ContentType, BookID) values (?1, 9, ?2)", [&cid, id]).unwrap();
            conn.execute("insert into volume_shortcovers values (?1, ?2)", [id, &cid]).unwrap();
        }
        conn.execute(
            "insert into content (ContentID, ContentType, BookID, Title, Attribution, ReadStatus, ___PercentRead, DateLastRead, ImageId, IsDownloaded, FirstTimeReading, ___ExpirationStatus, Accessibility, Series) values ('store-1', 6, null, 'Bought', 'Kobo Author', 2, 100, '2026-09-01', 'img2', 'true', 'false', 0, 1, null)",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "insert into content (ContentID, ContentType, BookID, Title, Attribution, ReadStatus, ___PercentRead, DateLastRead, ImageId, IsDownloaded, FirstTimeReading, ___ExpirationStatus, Accessibility, Series) values ('preview-1', 6, null, 'Sample', 'Someone', 1, 0, '2018-07-01', 'img3', 'true', 'false', 0, 6, 'Some Series');
             insert into content (ContentID, ContentType, BookID) values ('preview-1!ch1', 9, 'preview-1'), ('preview-1!s1', 899, 'preview-1');
             insert into volume_shortcovers values ('preview-1', 'preview-1!ch1');
             insert into Activity values ('preview-1', 'RecentBook'), ('Library', 'Library');",
        )
        .unwrap();
        conn.execute(
            "insert into WordList values ('sangfroid', ?1, '-en', '2026-09-12T19:33:42Z'), ('gone', 'deleted', '-en', '2026-01-01')",
            [id],
        )
        .unwrap();
        conn.execute_batch(
            "insert into Shelf (Id, InternalName, Name, _IsDeleted) values ('Faves', 'Faves', 'Faves', 'false'), ('Old', 'Old', 'Old', 'true');
             insert into ShelfContent (ShelfName, ContentId, _IsDeleted) values ('Faves', 'store-1', 'false'),
                 ('Faves', 'file:///mnt/onboard/Doe, Jane/Old Book - Jane Doe.kepub.epub', 'false');",
        )
        .unwrap();
        fs::write(mount.join(".kobo/kepub/preview-1"), b"encrypted sample").unwrap();
        drop(conn);
        fs::create_dir_all(mount.join("Doe, Jane")).unwrap();
        make_epub(&mount.join("Doe, Jane/Old Book - Jane Doe.kepub.epub"), "Old Book");
        let img = cached_cover_paths(&mount, "img1");
        fs::create_dir_all(img[0].parent().unwrap()).unwrap();
        fs::write(&img[0], b"jpeg").unwrap();
        let img = cached_cover_paths(&mount, "img3");
        fs::create_dir_all(img[0].parent().unwrap()).unwrap();
        fs::write(&img[0], b"jpeg").unwrap();
        device_at(&mount)
    }

    fn make_epub(path: &Path, title: &str) {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        let files = [
            ("mimetype", "application/epub+zip".to_string()),
            ("META-INF/container.xml", r#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#.to_string()),
            ("OEBPS/content.opf", format!(r#"<?xml version="1.0"?><package xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf"><metadata><dc:title>{title}</dc:title><dc:creator opf:file-as="Rowe, Sam">Sam Rowe</dc:creator></metadata><manifest><item id="ch" href="ch.xhtml" media-type="application/xhtml+xml"/></manifest></package>"#)),
            ("OEBPS/ch.xhtml", r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>x</title></head><body><p>One. Two.</p></body></html>"#.to_string()),
        ];
        for (name, body) in files {
            w.start_file(name, o).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn add_convert_and_remove_round_trip() {
        let dev = fake_device("roundtrip");
        let work = dev.mount.join(".work");
        let lib = load_library(&dev, &work).unwrap();
        let words: Vec<(&str, &str)> = lib.words.iter().map(|w| (w.text.as_str(), w.book.as_str())).collect();
        assert_eq!(words, vec![("sangfroid", "Old Book"), ("gone", "")]);
        assert_eq!(lib.shelves, vec![Shelf { key: "Faves".into(), name: "Faves".into(), books: vec!["store-1".into(), "file:///mnt/onboard/Doe, Jane/Old Book - Jane Doe.kepub.epub".into()] }]);
        let books = lib.books;
        assert_eq!(books.len(), 3);
        assert_eq!(books.iter().find(|b| b.title == "Sample").unwrap().kind, Kind::Preview);
        let old = books.iter().find(|b| b.title == "Old Book").unwrap().clone();
        assert_eq!((old.kind, old.status), (Kind::Sideloaded, Status::Reading(0.4)));
        assert_eq!(books.iter().find(|b| b.title == "Bought").unwrap().kind, Kind::Store);

        // Add: lands in Author Sort/Title - Author.kepub.epub, converted, then shows as pending.
        let src = dev.mount.join(".work/new.epub");
        make_epub(&src, "New: A Book");
        add_book(&dev, &src, true).unwrap();
        let dest = dev.mount.join("Rowe, Sam/New_ A Book - Sam Rowe.kepub.epub");
        let mut zip = zip::ZipArchive::new(File::open(&dest).unwrap()).unwrap();
        assert_eq!(zip.by_index(0).unwrap().name().unwrap(), "mimetype");
        assert!(epub::read_string(&mut zip, "OEBPS/ch.xhtml").unwrap().contains(r#"id="kobo.1.2">Two.</span>"#));
        assert_eq!(add_book(&dev, &src, true).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert!(!dev.mount.join("Rowe, Sam/.New_ A Book - Sam Rowe.part").exists());
        let books = load_library(&dev, &work).unwrap().books;
        let new = books.iter().find(|b| b.kind == Kind::Pending).unwrap();
        assert_eq!((new.title.as_str(), new.author.as_str()), ("New: A Book", "Sam Rowe"));

        // Remove a sideloaded book: file, folder, cover cache and chapter rows go;
        // the book row is reset for the Kobo to drop, and no WAL is left behind.
        let backup = backup_db(&dev, &work.join("backups")).unwrap();
        assert!(backup.is_file());
        remove_book(&dev, &old).unwrap();
        assert!(!dev.mount.join("Doe, Jane").exists());
        assert!(!cached_cover_paths(&dev.mount, "img1")[0].exists());
        assert!(!with_suffix(&dev.db_path(), "-wal").exists());
        let conn = Connection::open(dev.db_path()).unwrap();
        let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count("select count(*) from content where BookID like 'file:%'"), 0);
        assert_eq!(count("select count(*) from volume_shortcovers where volumeId like 'file:%'"), 0);
        assert_eq!(count("select ___ExpirationStatus from content where Title = 'Old Book'"), 3);
        drop(conn);
        let books = load_library(&dev, &work).unwrap().books;
        assert!(books.iter().all(|b| b.title != "Old Book"));

        // Store books are refused; pending ones are just deleted.
        let store = books.iter().find(|b| b.kind == Kind::Store).unwrap();
        assert!(remove_book(&dev, store).is_err());
        let pending = books.iter().find(|b| b.kind == Kind::Pending).unwrap();
        remove_book(&dev, pending).unwrap();
        assert!(!dest.exists());

        // Previews: every reference goes, the file is stashed, other rows stay.
        let stash = work.join("stash");
        assert_eq!(remove_previews(&dev, &books, &stash).unwrap(), 1);
        assert!(!dev.mount.join(".kobo/kepub/preview-1").exists());
        assert!(!cached_cover_paths(&dev.mount, "img3")[0].exists());
        assert_eq!(fs::read(stash.join("kepub/preview-1")).unwrap(), b"encrypted sample");
        assert!(stash.join("images/img3 - N3_LIBRARY_FULL.parsed").is_file());
        assert!(!with_suffix(&dev.db_path(), "-wal").exists());
        let conn = Connection::open(dev.db_path()).unwrap();
        let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count("select count(*) from content where ContentID like 'preview-1%' or BookID = 'preview-1'"), 0);
        assert_eq!(count("select count(*) from volume_shortcovers"), 0);
        assert_eq!(count("select count(*) from Activity"), 1);
        assert_eq!(count("select count(*) from content where Title in ('Old Book', 'Bought')"), 2);
        drop(conn);
        let titles: Vec<String> = load_library(&dev, &work).unwrap().books.into_iter().map(|b| b.title).collect();
        assert_eq!(count_rows(&dev, "select count(*) from ShelfContent where ContentId like 'file:%'"), 0, "dropped from collections");
        assert_eq!(titles, vec!["Bought"]);
        fs::remove_dir_all(&dev.mount).unwrap();
    }

    /// Against a copy of a real Kobo: `KOBO_SHELF_MOUNT=/path cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_device_copy() {
        let mount = PathBuf::from(std::env::var("KOBO_SHELF_MOUNT").unwrap());
        let dev = device_at(&mount);
        let books = load_library(&dev, &mount.join(".work")).unwrap().books;
        let covers = books.iter().filter(|b| cover_bytes(&dev, b).is_some()).count();
        eprintln!("{} books, {covers} with covers, firmware {}", books.len(), dev.firmware);
        for b in books.iter().filter(|b| cover_bytes(&dev, b).is_none()) {
            eprintln!("  no cover: {} ({:?})", b.title, b.kind);
        }
        let victim = books.iter().find(|b| b.kind == Kind::Sideloaded).unwrap();
        eprintln!("removing {}", victim.title);
        remove_book(&dev, victim).unwrap();
        let conn = Connection::open(dev.db_path()).unwrap();
        let ok: String = conn.query_row("pragma integrity_check", [], |r| r.get(0)).unwrap();
        assert_eq!(ok, "ok");
        assert_eq!(load_library(&dev, &mount.join(".work")).unwrap().books.len(), books.len() - 1);
        drop(conn);

        let previews = books.iter().filter(|b| b.kind == Kind::Preview).count();
        let removed = remove_previews(&dev, &books, &mount.join(".work/stash")).unwrap();
        eprintln!("removed {removed} of {previews} previews");
        let conn = Connection::open(dev.db_path()).unwrap();
        let ok: String = conn.query_row("pragma integrity_check", [], |r| r.get(0)).unwrap();
        assert_eq!(ok, "ok");
        let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        eprintln!(
            "left: {} books, {} with a series, {} orphan rows",
            count("select count(*) from content where ContentType = 6"),
            count("select count(*) from content where ContentType = 6 and Series <> ''"),
            count("select count(*) from content where BookID is not null and BookID not in (select ContentID from content)"),
        );
        assert_eq!(count("select count(*) from content where ContentType = 6 and Accessibility = 6"), 0);
    }

    fn count_rows(dev: &Device, sql: &str) -> i64 {
        Connection::open(dev.db_path()).unwrap().query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn content_ids_use_device_paths() {
        let m = Path::new("/Volumes/KOBOeReader");
        assert_eq!(
            content_id(m, &m.join("Moore, Lane/How to Be Alone - Lane Moore.kepub.epub")),
            "file:///mnt/onboard/Moore, Lane/How to Be Alone - Lane Moore.kepub.epub"
        );
    }
}
