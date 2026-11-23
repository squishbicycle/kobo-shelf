//! Changes to the library: read status, series and collections. Each edit is
//! one transaction on the Kobo's database (see [`kobo::write`]).

use std::path::Path;

use rusqlite::{params, Transaction};

use crate::kobo;

#[derive(Clone, Debug)]
pub enum Edit {
    MarkFinished(Vec<String>),
    /// Also resets progress, so the book opens at the start.
    MarkUnread(Vec<String>),
    /// An empty name takes the books out of any series. `numbers` lines up
    /// with `ids`; `None` leaves the number blank.
    SetSeries { ids: Vec<String>, name: String, numbers: Vec<Option<String>> },
    /// Creates the collection if needed, so an empty `ids` just creates it.
    AddToCollection { name: String, ids: Vec<String> },
    RemoveFromCollection { key: String, ids: Vec<String> },
    RenameCollection { key: String, to: String },
    /// The books stay on the Kobo.
    DeleteCollection { key: String },
}

pub fn apply(db: &Path, edit: &Edit) -> rusqlite::Result<()> {
    kobo::write(db, |tx| apply_in(tx, edit))
}

/// Kobo's timestamp format, in UTC.
fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn apply_in(tx: &Transaction, edit: &Edit) -> rusqlite::Result<()> {
    let now = now();
    match edit {
        Edit::MarkFinished(ids) => {
            for id in ids {
                tx.execute(
                    "update content set ReadStatus = 2, ___PercentRead = 100, FirstTimeReading = 'false',
                         DateLastRead = coalesce(nullif(DateLastRead, ''), ?2)
                     where ContentID = ?1 and BookID is null",
                    params![id, now],
                )?;
            }
        }
        Edit::MarkUnread(ids) => {
            for id in ids {
                tx.execute(
                    "update content set ReadStatus = 0, ___PercentRead = 0, FirstTimeReading = 'true',
                         ChapterIDBookmarked = null
                     where ContentID = ?1 and BookID is null",
                    params![id],
                )?;
            }
        }
        Edit::SetSeries { ids, name, numbers } => {
            let name = name.trim();
            // Join an existing series' ID if there is one (store books have
            // Kobo's own); otherwise the name doubles as the ID, as Calibre does.
            let series_id: Option<String> = if name.is_empty() {
                None
            } else {
                Some(
                    tx.query_row(
                        "select SeriesID from content where Series = ?1 and coalesce(SeriesID, '') <> '' limit 1",
                        [name],
                        |r| r.get(0),
                    )
                    .unwrap_or_else(|_| name.to_string()),
                )
            };
            for (i, id) in ids.iter().enumerate() {
                let number = numbers.get(i).cloned().flatten().filter(|n| !n.trim().is_empty() && !name.is_empty());
                let float = number.as_deref().and_then(|n| n.trim().parse::<f64>().ok());
                tx.execute(
                    "update content set Series = ?2, SeriesID = ?3, SeriesNumber = ?4, SeriesNumberFloat = ?5
                     where ContentID = ?1 and BookID is null",
                    params![id, (!name.is_empty()).then_some(name), series_id, number.map(|n| n.trim().to_string()), float],
                )?;
            }
        }
        Edit::AddToCollection { name, ids } => {
            let name = name.trim();
            let key: String = match tx.query_row(
                "select InternalName from Shelf where Name = ?1 or InternalName = ?1 limit 1",
                [name],
                |r| r.get(0),
            ) {
                Ok(key) => key,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    tx.execute(
                        "insert into Shelf (CreationDate, Id, InternalName, LastModified, Name, Type,
                             _IsDeleted, _IsVisible, _IsSynced, LastAccessed)
                         values (?2, ?1, ?1, ?2, ?1, 'UserTag', 'false', 'true', 'false', ?2)",
                        params![name, now],
                    )?;
                    name.to_string()
                }
                Err(e) => return Err(e),
            };
            tx.execute("update Shelf set _IsDeleted = 'false', LastModified = ?2 where InternalName = ?1", params![key, now])?;
            for id in ids {
                tx.execute(
                    "insert or replace into ShelfContent (ShelfName, ContentId, DateModified, _IsDeleted, _IsSynced)
                     values (?1, ?2, ?3, 'false', 'false')",
                    params![key, id, now],
                )?;
            }
        }
        Edit::RemoveFromCollection { key, ids } => {
            for id in ids {
                tx.execute("delete from ShelfContent where ShelfName = ?1 and ContentId = ?2", params![key, id])?;
            }
            tx.execute("update Shelf set LastModified = ?2 where InternalName = ?1", params![key, now])?;
        }
        Edit::RenameCollection { key, to } => {
            let to = to.trim();
            tx.execute(
                "update Shelf set Name = ?2, InternalName = ?2, Id = case when Id = ?1 then ?2 else Id end,
                     LastModified = ?3
                 where InternalName = ?1",
                params![key, to, now],
            )?;
            tx.execute("update ShelfContent set ShelfName = ?2, DateModified = ?3 where ShelfName = ?1", params![key, to, now])?;
        }
        Edit::DeleteCollection { key } => {
            // No account sync, so there's nothing to tell; delete outright.
            tx.execute("delete from ShelfContent where ShelfName = ?1", [key])?;
            tx.execute("delete from Shelf where InternalName = ?1", [key])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn db() -> (tempdir::Dir, std::path::PathBuf) {
        let dir = tempdir::Dir::new("edits");
        let path = dir.0.join("KoboReader.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "pragma journal_mode = wal;
             create table content (ContentID text, BookID text, Title text, ReadStatus int, ___PercentRead int,
                 FirstTimeReading text, DateLastRead text, ChapterIDBookmarked text,
                 Series text, SeriesID text, SeriesNumber text, SeriesNumberFloat real);
             create table Shelf (CreationDate text, Id text primary key, InternalName text, LastModified text,
                 Name text, Type text, _IsDeleted bool, _IsVisible bool, _IsSynced bool, _SyncTime text,
                 LastAccessed text);
             create table ShelfContent (ShelfName text, ContentId text, DateModified text, _IsDeleted bool,
                 _IsSynced bool, primary key (ShelfName, ContentId));
             insert into content (ContentID, Title, ReadStatus, ___PercentRead, ChapterIDBookmarked)
                 values ('a', 'A', 1, 40, 'ch3'), ('b', 'B', 0, 0, null);
             insert into content (ContentID, BookID) values ('a!ch1', 'a');
             insert into content (ContentID, Title, Series, SeriesID) values ('s', 'Store', 'Kingsbridge', 'kobo-123');",
        )
        .unwrap();
        (dir, path)
    }

    fn rows(path: &Path, sql: &str) -> Vec<Vec<Option<String>>> {
        let conn = Connection::open(path).unwrap();
        let mut stmt = conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        stmt.query_map([], |r| (0..n).map(|i| r.get::<_, rusqlite::types::Value>(i)).collect::<Result<Vec<_>, _>>())
            .unwrap()
            .map(|r| {
                r.unwrap()
                    .into_iter()
                    .map(|v| match v {
                        rusqlite::types::Value::Null => None,
                        rusqlite::types::Value::Integer(i) => Some(i.to_string()),
                        rusqlite::types::Value::Real(f) => Some(f.to_string()),
                        rusqlite::types::Value::Text(t) => Some(t),
                        rusqlite::types::Value::Blob(_) => Some("blob".into()),
                    })
                    .collect()
            })
            .collect()
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn read_status() {
        let (_d, db) = db();
        apply(&db, &Edit::MarkFinished(vec!["a".into(), "b".into()])).unwrap();
        let r = rows(&db, "select ReadStatus, ___PercentRead, DateLastRead is not null from content where Title in ('A','B')");
        assert_eq!(r, vec![vec![s("2"), s("100"), s("1")], vec![s("2"), s("100"), s("1")]]);
        apply(&db, &Edit::MarkUnread(vec!["a".into()])).unwrap();
        let r = rows(&db, "select ReadStatus, ___PercentRead, FirstTimeReading, ChapterIDBookmarked from content where Title = 'A'");
        assert_eq!(r, vec![vec![s("0"), s("0"), s("true"), None]]);
        // Chapter rows are never touched, and the WAL is folded back in.
        assert_eq!(rows(&db, "select ReadStatus from content where BookID = 'a'"), vec![vec![None]]);
        assert!(!db.with_extension("sqlite-wal").exists());
    }

    #[test]
    fn series() {
        let (_d, db) = db();
        let ids = vec!["a".to_string(), "b".to_string()];
        apply(&db, &Edit::SetSeries { ids: ids.clone(), name: " Kingsbridge ".into(), numbers: vec![s("1"), s("2.5")] }).unwrap();
        let r = rows(&db, "select Series, SeriesID, SeriesNumber, SeriesNumberFloat from content where Title in ('A','B')");
        assert_eq!(r[0], vec![s("Kingsbridge"), s("kobo-123"), s("1"), s("1")]);
        assert_eq!(r[1], vec![s("Kingsbridge"), s("kobo-123"), s("2.5"), s("2.5")]);
        apply(&db, &Edit::SetSeries { ids: vec!["a".into()], name: "New One".into(), numbers: vec![None] }).unwrap();
        assert_eq!(rows(&db, "select Series, SeriesID, SeriesNumber from content where Title = 'A'")[0], vec![s("New One"), s("New One"), None]);
        apply(&db, &Edit::SetSeries { ids, name: "".into(), numbers: vec![s("1"), s("2")] }).unwrap();
        let r = rows(&db, "select Series, SeriesID, SeriesNumber, SeriesNumberFloat from content where Title in ('A','B')");
        assert_eq!(r, vec![vec![None, None, None, None]; 2]);
    }

    #[test]
    fn collections() {
        let (_d, db) = db();
        apply(&db, &Edit::AddToCollection { name: "Japanese".into(), ids: vec!["a".into(), "b".into()] }).unwrap();
        apply(&db, &Edit::AddToCollection { name: "Japanese".into(), ids: vec!["a".into()] }).unwrap();
        assert_eq!(rows(&db, "select Id, Name, Type, _IsDeleted, _IsVisible from Shelf"), vec![vec![s("Japanese"), s("Japanese"), s("UserTag"), s("false"), s("true")]]);
        assert_eq!(rows(&db, "select count(*) from ShelfContent where ShelfName = 'Japanese'"), vec![vec![s("2")]]);

        apply(&db, &Edit::RemoveFromCollection { key: "Japanese".into(), ids: vec!["b".into()] }).unwrap();
        assert_eq!(rows(&db, "select ContentId from ShelfContent"), vec![vec![s("a")]]);

        apply(&db, &Edit::RenameCollection { key: "Japanese".into(), to: "日本語".into() }).unwrap();
        assert_eq!(rows(&db, "select Id, InternalName, Name from Shelf"), vec![vec![s("日本語"); 3]]);
        assert_eq!(rows(&db, "select ShelfName from ShelfContent"), vec![vec![s("日本語")]]);

        apply(&db, &Edit::AddToCollection { name: "Empty".into(), ids: vec![] }).unwrap();
        apply(&db, &Edit::DeleteCollection { key: "日本語".into() }).unwrap();
        assert_eq!(rows(&db, "select Name from Shelf"), vec![vec![s("Empty")]]);
        assert_eq!(rows(&db, "select count(*) from ShelfContent"), vec![vec![s("0")]]);
        assert_eq!(rows(&db, "select count(*) from content"), vec![vec![s("4")]], "books are untouched");
    }

    /// Against a copy of a real Kobo: `KOBO_SHELF_MOUNT=/path cargo test real_device_edits -- --ignored`.
    #[test]
    #[ignore]
    fn real_device_edits() {
        use crate::kobo::{self, Status};
        let mount = std::path::PathBuf::from(std::env::var("KOBO_SHELF_MOUNT").unwrap());
        let dev = kobo::device_at(&mount);
        let work = mount.join(".work");
        let lib = kobo::load_library(&dev, &work).unwrap();
        let ids: Vec<String> = lib.books.iter().filter(|b| b.in_db()).take(3).map(|b| b.content_id.clone()).collect();
        let db = dev.db_path();
        apply(&db, &Edit::MarkFinished(ids.clone())).unwrap();
        apply(&db, &Edit::MarkUnread(vec![ids[0].clone()])).unwrap();
        let numbers = vec![Some("1".into()), Some("2".into()), Some("3".into())];
        apply(&db, &Edit::SetSeries { ids: ids.clone(), name: "Test Series".into(), numbers }).unwrap();
        apply(&db, &Edit::AddToCollection { name: "Test".into(), ids: ids.clone() }).unwrap();
        apply(&db, &Edit::RenameCollection { key: "Test".into(), to: "Renamed".into() }).unwrap();
        apply(&db, &Edit::RemoveFromCollection { key: "Renamed".into(), ids: vec![ids[2].clone()] }).unwrap();
        apply(&db, &Edit::AddToCollection { name: "Gone".into(), ids: vec![ids[0].clone()] }).unwrap();
        apply(&db, &Edit::DeleteCollection { key: "Gone".into() }).unwrap();

        let after = kobo::load_library(&dev, &work).unwrap();
        let get = |id: &String| after.books.iter().find(|b| &b.content_id == id).unwrap();
        assert_eq!(get(&ids[0]).status, Status::Unread);
        assert_eq!(get(&ids[1]).status, Status::Finished);
        assert_eq!((get(&ids[2]).series.as_str(), get(&ids[2]).series_number.as_str()), ("Test Series", "3"));
        assert_eq!(after.shelves.len(), 1);
        assert_eq!((after.shelves[0].name.as_str(), after.shelves[0].books.len()), ("Renamed", 2));
        let conn = Connection::open(&db).unwrap();
        let ok: String = conn.query_row("pragma integrity_check", [], |r| r.get(0)).unwrap();
        assert_eq!(ok, "ok");
        drop(conn);
        assert!(!db.with_extension("sqlite-wal").exists());

        let missing: Vec<&str> = after.words.iter().filter(|w| crate::words::define(w).is_none()).map(|w| w.text.as_str()).collect();
        eprintln!("{} words, {} without a definition: {missing:?}", after.words.len(), missing.len());
        for w in after.words.iter().take(3) {
            eprintln!("  {} ({}, {}): {:?}", w.text, w.book, w.saved, crate::words::define(w));
        }
    }

    /// A throwaway folder that cleans itself up.
    mod tempdir {
        pub struct Dir(pub std::path::PathBuf);
        impl Dir {
            pub fn new(name: &str) -> Self {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static N: AtomicUsize = AtomicUsize::new(0);
                let p = std::env::temp_dir().join(format!(
                    "kobo-shelf-{name}-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                let _ = std::fs::remove_dir_all(&p);
                std::fs::create_dir_all(&p).unwrap();
                Dir(p)
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
