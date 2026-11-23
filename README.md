# Kobo Shelf

A Mac app for managing the books on a Kobo e-reader, without Calibre.

- Cover grid of everything on the Kobo, with reading progress
- Drag in EPUBs (converted to kepub automatically), PDFs or comics
- Remove books; the Kobo's database is backed up first
- Collections and series, editable in batches
- Search, sort, and filter by unread, reading or finished
- **My Words:** words you looked up on the Kobo, exported to CSV or Anki
- **Stats:** reading time and books finished
- Clears out the factory preview books Kobo won't let you delete

Store-bought books are shown but can't be removed here, since they'd sync back.

## Build

Needs macOS 13+, [Rust](https://rustup.rs/), and the Xcode Command Line Tools.

    ./build.sh    # installs ~/Applications/Kobo Shelf.app
    cargo test

Tests that run against real data (`cargo test -- --ignored`) modify their target: point `KOBO_SHELF_MOUNT` at a copy of the Kobo, never the device.

See the [changelog](CHANGELOG.md) for what's new.

## License

[GPL v3](LICENSE) or later.
