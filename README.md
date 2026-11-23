# Kobo Shelf

See and manage what's on a Kobo e-reader, without a separate library like Calibre's.

- Plug in the Kobo: it's detected automatically and every book appears in a cover grid
  with title, author and reading progress.
- **Add:** drag EPUBs (or PDFs, CBZs, …) onto the window, or use *Add books…*. Files go to
  `Author Sort/Title - Author.kepub.epub`. Plain EPUBs are converted to kepub on the way
  (toggle in the toolbar). The Kobo imports new books when you eject.
- **Remove:** hover a cover and click ✕ (or right-click). Before the first removal on each
  connection, the Kobo's database is backed up to
  `~/Library/Application Support/Kobo Shelf/backups` (newest 20 kept).
- **Sidebar:** all books, unread, reading, finished, plus your collections and series.
- **Search and sort:** by title/author; sort by title, author, recently read or progress
  (a series view is always in series order).
- **Multi-select:** click, ⌘-click, ⇧-click, ⌘A; Esc clears. Right-click acts on the selection.
- **Batch actions:** mark finished, mark unread (resets progress; reading-time stats are kept),
  set series (optionally numbering in the order shown), add to / remove from collections, remove.
- **Collections:** create (+ in the sidebar), rename or delete (right-click). Deleting a
  collection keeps its books.
- **My Words:** words saved from the Kobo dictionary, with definitions from this Mac's
  dictionary (English) or `jdict` (Japanese). Export as CSV or as an Anki import file.
- **Stats:** total reading time, books finished, what you're reading now with the Kobo's
  time-left estimate, and time spent per book.
- **Eject:** deletes the `._` files macOS leaves on the drive, then ejects.

**Previews:** Kobos ship with factory-loaded samples belonging to Kobo's own demo account
(Accessibility = 6 in the database). They can't be removed on the device and they fill the Series
tab. *Remove N previews…* deletes every row that refers to them (book, chapters, sections, cover
snippets, home-screen tiles) after backing up the database, and moves their files to
`backups/previews-<time>/` rather than deleting them. To undo: copy the database backup back to
`.kobo/KoboReader.sqlite` and the stashed `kepub/` files back to `.kobo/kepub/`.

Books bought from the Kobo Store are shown but can't be removed here: they belong to a Kobo
account and would sync back, so remove them on the Kobo itself.

## Build

Requirements: macOS 13 or later, the [Rust toolchain](https://rustup.rs/), and the Xcode Command Line Tools (`xcode-select --install`).

    ./build.sh        # release build, installs ~/Applications/Kobo Shelf.app
    cargo test        # unit tests + a fake-Kobo add/convert/remove round trip

Extra checks against real data (both are ignored by default):

    KOBO_SHELF_MOUNT=/copy/of/kobo cargo test real_device_copy -- --ignored
    KOBO_SHELF_MOUNT=/copy/of/kobo cargo test real_device_edits -- --ignored
    KOBO_SHELF_EPUB=/some/book.epub cargo test convert_real_epub -- --ignored

Run the `real_device_*` tests against a *copy* of the Kobo, never the device itself, because they change it.

See the [changelog](CHANGELOG.md) for what's new.

## License

Kobo Shelf is free software under the [GNU General Public License v3.0](LICENSE) or later: you can use, change and share it, and anything you distribute that's built on it must be shared under the same license.
