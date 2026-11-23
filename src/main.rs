//! Kobo Shelf: see and manage what's on a Kobo e-reader, without keeping a
//! library of your own.

mod dialogs;
mod edits;
mod epub;
mod kepub;
mod kobo;
mod shelf;
mod views;
mod words;

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Align, Color32, ColorImage, FontFamily, FontId, Layout, RichText, TextureHandle};

use dialogs::Dialog;
use edits::Edit;
use kobo::{Book, Device, Kind, Library};

const ADD_EXTS: &[&str] = &["epub", "pdf", "mobi", "cbz", "cbr", "txt", "html", "rtf"];

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kobo Shelf")
            .with_inner_size([1120.0, 780.0])
            .with_min_inner_size([640.0, 440.0])
            .with_drag_and_drop(true)
            // An empty icon stops eframe swapping in its own; the Dock then uses AppIcon.icns.
            .with_icon(egui::IconData::default()),
        ..Default::default()
    };
    eframe::run_native("Kobo Shelf", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

enum Msg {
    Library(u64, io::Result<Library>),
    Cover(u64, String, ColorImage),
    Added(Vec<(String, io::Result<kobo::Added>)>),
    /// A change to the Kobo finished. `Ok` carries the message to show.
    Done { backup: Option<io::Result<PathBuf>>, result: io::Result<String> },
    Ejected(io::Result<usize>),
    Definitions(Vec<(String, Option<String>)>),
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Books,
    Words,
    Stats,
}

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Title,
    Author,
    RecentlyRead,
    Progress,
}

impl Sort {
    const ALL: [Sort; 4] = [Sort::Title, Sort::Author, Sort::RecentlyRead, Sort::Progress];
    fn label(self) -> &'static str {
        match self {
            Sort::Title => "Title",
            Sort::Author => "Author",
            Sort::RecentlyRead => "Recently read",
            Sort::Progress => "Progress",
        }
    }
}

struct App {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    device: Option<Device>,
    lib: Library,
    covers: HashMap<String, TextureHandle>,
    /// Bumped on every reload so stale results get dropped.
    generation: u64,
    loading: bool,
    busy: Option<String>,
    status: String,
    tab: Tab,
    search: String,
    sort: Sort,
    scope: shelf::Scope,
    convert: bool,
    /// Content IDs of selected books, and where a shift-click range starts.
    selected: HashSet<String>,
    anchor: Option<String>,
    dialog: Option<Dialog>,
    /// Whether the database has been backed up since the Kobo was connected.
    backed_up: bool,
    last_poll: f64,
    backup_dir: PathBuf,
    work_dir: PathBuf,
    /// Word → definition, filled in when My Words is first opened.
    definitions: HashMap<String, Option<String>>,
    defining: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext) -> Self {
        install_fonts(&cc.egui_ctx);
        let (tx, rx) = channel();
        let mut app = App {
            tx,
            rx,
            device: None,
            lib: Library::default(),
            covers: HashMap::new(),
            generation: 0,
            loading: false,
            busy: None,
            status: String::new(),
            tab: Tab::Books,
            search: String::new(),
            sort: Sort::Title,
            scope: shelf::Scope::All,
            convert: true,
            selected: HashSet::new(),
            anchor: None,
            dialog: None,
            backed_up: false,
            last_poll: f64::NEG_INFINITY,
            backup_dir: home().join("Library/Application Support/Kobo Shelf/backups"),
            work_dir: home().join("Library/Caches/Kobo Shelf"),
            definitions: HashMap::new(),
            defining: false,
        };
        app.poll_device(&cc.egui_ctx);
        app
    }

    fn poll_device(&mut self, ctx: &egui::Context) {
        let found = kobo::find_device();
        if found.as_ref().map(|d| &d.mount) == self.device.as_ref().map(|d| &d.mount) {
            return;
        }
        let connected = found.is_some();
        self.device = found;
        self.lib = Library::default();
        self.covers.clear();
        self.selected.clear();
        self.backed_up = false;
        self.dialog = None;
        if connected {
            self.status = "Kobo connected.".into();
            self.reload(ctx);
        } else {
            self.generation += 1;
            self.loading = false;
        }
    }

    fn reload(&mut self, ctx: &egui::Context) {
        let Some(dev) = self.device.clone() else { return };
        self.generation += 1;
        self.loading = true;
        let (generation, tx, ctx, work) = (self.generation, self.tx.clone(), ctx.clone(), self.work_dir.clone());
        let have: HashSet<String> = self.covers.keys().cloned().collect();
        std::thread::spawn(move || {
            let result = kobo::load_library(&dev, &work);
            let books = result.as_ref().map(|l| l.books.clone()).unwrap_or_default();
            let _ = tx.send(Msg::Library(generation, result));
            ctx.request_repaint();
            for b in books.iter().filter(|b| !have.contains(&b.content_id)) {
                if let Some(img) = kobo::cover_bytes(&dev, b).and_then(|bytes| decode_cover(&bytes)) {
                    if tx.send(Msg::Cover(generation, b.content_id.clone(), img)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            }
        });
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Library(g, result) if g == self.generation => {
                    self.loading = false;
                    match result {
                        Ok(lib) => {
                            // Keep covers for books that are still there.
                            self.covers.retain(|id, _| lib.books.iter().any(|b| &b.content_id == id));
                            self.selected.retain(|id| lib.books.iter().any(|b| &b.content_id == id));
                            if !self.scope.exists(&lib) {
                                self.scope = shelf::Scope::All;
                            }
                            self.lib = lib;
                        }
                        Err(e) => self.status = format!("Couldn't read the Kobo's library: {e}"),
                    }
                }
                Msg::Cover(g, id, img) if g == self.generation => {
                    let tex = ctx.load_texture(format!("cover:{id}"), img, egui::TextureOptions::LINEAR);
                    self.covers.insert(id, tex);
                }
                Msg::Library(..) | Msg::Cover(..) => {}
                Msg::Added(results) => {
                    self.busy = None;
                    let ok = results.iter().filter(|(_, r)| r.is_ok()).count();
                    let mut parts = vec![match ok {
                        0 => "Nothing was added.".to_string(),
                        1 => "Added 1 book. The Kobo will import it when you eject.".to_string(),
                        n => format!("Added {n} books. The Kobo will import them when you eject."),
                    }];
                    for (name, r) in &results {
                        match r {
                            Ok(a) => {
                                if let Some(note) = &a.note {
                                    parts.push(format!("{name}: {note}."));
                                }
                            }
                            Err(e) => parts.push(format!("{name}: {e}.")),
                        }
                    }
                    self.status = parts.join("  ");
                    self.reload(ctx);
                }
                Msg::Done { backup, result } => {
                    self.busy = None;
                    let mut s = match result {
                        Ok(s) => s,
                        Err(e) => format!("That didn't work: {e}"),
                    };
                    match backup {
                        Some(Ok(path)) => {
                            self.backed_up = true;
                            s.push_str(&format!("  Database backed up to {}.", path.display()));
                        }
                        Some(Err(e)) => s = format!("Nothing was changed, because the database backup failed: {e}"),
                        None => {}
                    }
                    self.status = s;
                    self.reload(ctx);
                }
                Msg::Ejected(result) => {
                    self.busy = None;
                    match result {
                        Ok(cleaned) => {
                            self.status = if cleaned > 0 {
                                format!("Ejected. Cleaned up {cleaned} macOS ._ files first. Safe to unplug.")
                            } else {
                                "Ejected. Safe to unplug.".into()
                            };
                            self.poll_device(ctx);
                        }
                        Err(e) => self.status = format!("Couldn't eject: {e}"),
                    }
                }
                Msg::Definitions(defs) => {
                    self.defining = false;
                    self.definitions.extend(defs);
                }
            }
        }
    }

    /// Run a change to the Kobo in the background, backing up its database
    /// first if that hasn't happened since it was connected.
    fn run(
        &mut self,
        ctx: &egui::Context,
        busy: String,
        job: impl FnOnce(&Device) -> io::Result<String> + Send + 'static,
    ) {
        let Some(dev) = self.device.clone() else { return };
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(busy);
        let need_backup = !self.backed_up;
        let (tx, ctx, backup_dir) = (self.tx.clone(), ctx.clone(), self.backup_dir.clone());
        std::thread::spawn(move || {
            let backup = need_backup.then(|| kobo::backup_db(&dev, &backup_dir));
            let result = match &backup {
                Some(Err(_)) => Err(io::Error::other("backup failed")),
                _ => job(&dev),
            };
            let _ = tx.send(Msg::Done { backup, result });
            ctx.request_repaint();
        });
    }

    fn edit(&mut self, ctx: &egui::Context, edit: Edit, done: String) {
        self.run(ctx, "Saving…".into(), move |dev| {
            edits::apply(&dev.db_path(), &edit).map_err(io::Error::other)?;
            Ok(done)
        });
    }

    fn remove(&mut self, ctx: &egui::Context, books: Vec<Book>) {
        let books: Vec<Book> = books.into_iter().filter(|b| b.kind != Kind::Store).collect();
        let what = describe(&books);
        let stash = self.backup_dir.join(format!("previews-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        self.run(ctx, format!("Removing {what}…"), move |dev| {
            let (previews, others): (Vec<Book>, Vec<Book>) = books.into_iter().partition(|b| b.kind == Kind::Preview);
            kobo::remove_previews(dev, &previews, &stash)?;
            for b in &others {
                kobo::remove_book(dev, b)?;
            }
            Ok(format!("Removed {what}."))
        });
    }

    fn add_files(&mut self, ctx: &egui::Context, files: Vec<PathBuf>) {
        let Some(dev) = self.device.clone() else {
            self.status = "Plug in your Kobo first.".into();
            return;
        };
        if files.is_empty() || self.busy.is_some() {
            return;
        }
        let n = files.len();
        self.busy = Some(if n == 1 { "Adding 1 book…".into() } else { format!("Adding {n} books…") });
        let (tx, ctx, convert) = (self.tx.clone(), ctx.clone(), self.convert);
        std::thread::spawn(move || {
            let results = files
                .into_iter()
                .map(|f| {
                    let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    (name, kobo::add_book(&dev, &f, convert))
                })
                .collect();
            let _ = tx.send(Msg::Added(results));
            ctx.request_repaint();
        });
    }

    fn eject(&mut self, ctx: &egui::Context) {
        let Some(dev) = self.device.clone() else { return };
        self.busy = Some("Ejecting…".into());
        self.generation += 1; // stop loading covers from a drive that's going away
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let cleaned = kobo::clean_mac_clutter(&dev.mount);
            let _ = tx.send(Msg::Ejected(kobo::eject(&dev).map(|_| cleaned)));
            ctx.request_repaint();
        });
    }

    fn fetch_definitions(&mut self, ctx: &egui::Context) {
        let missing: Vec<kobo::Word> =
            self.lib.words.iter().filter(|w| !self.definitions.contains_key(&w.text)).cloned().collect();
        if missing.is_empty() || self.defining {
            return;
        }
        self.defining = true;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let defs = missing.into_iter().map(|w| (w.text.clone(), words::define(&w))).collect();
            let _ = tx.send(Msg::Definitions(defs));
            ctx.request_repaint();
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading("Kobo Shelf");
            ui.add_space(8.0);
            match &self.device {
                Some(dev) => {
                    let n = self.lib.books.len();
                    let mut s = format!("Kobo connected · {n} book{}", if n == 1 { "" } else { "s" });
                    if !dev.firmware.is_empty() {
                        s.push_str(&format!(" · firmware {}", dev.firmware));
                    }
                    ui.label(RichText::new(s).weak());
                }
                None => {
                    ui.label(RichText::new("No Kobo connected").weak());
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let idle = self.device.is_some() && self.busy.is_none();
                if ui.add_enabled(idle, egui::Button::new("Eject")).on_hover_text("Clean up macOS clutter, then eject").clicked() {
                    self.eject(&ctx);
                }
                if ui.add_enabled(idle, egui::Button::new("Add books…")).clicked() {
                    let files = rfd::FileDialog::new().add_filter("Books", ADD_EXTS).pick_files().unwrap_or_default();
                    self.add_files(&ctx, files);
                }
                let previews: Vec<Book> = self.lib.books.iter().filter(|b| b.kind == Kind::Preview).cloned().collect();
                if !previews.is_empty()
                    && ui
                        .add_enabled(idle, egui::Button::new(format!("Remove {} previews…", previews.len())))
                        .on_hover_text("Samples Kobo loaded at the factory, and the series they add")
                        .clicked()
                {
                    self.dialog = Some(Dialog::Remove(previews));
                }
                if ui.add_enabled(idle, egui::Button::new("Refresh")).clicked() {
                    self.reload(&ctx);
                }
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let words = format!("My Words ({})", self.lib.words.len());
            for (tab, label) in [(Tab::Books, "Books"), (Tab::Words, words.as_str()), (Tab::Stats, "Stats")] {
                if ui.selectable_label(self.tab == tab, label).clicked() {
                    self.tab = tab;
                }
            }
            if self.tab != Tab::Books {
                return;
            }
            ui.separator();
            ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Search title or author").desired_width(220.0));
            if !self.search.is_empty() && ui.small_button("✕").clicked() {
                self.search.clear();
            }
            ui.add_space(8.0);
            if matches!(self.scope, shelf::Scope::Series(_)) {
                ui.add_enabled(false, egui::Button::new("Sort: Series order"));
            } else {
                egui::ComboBox::new("sort", "").selected_text(format!("Sort: {}", self.sort.label())).show_ui(ui, |ui| {
                    for s in Sort::ALL {
                        ui.selectable_value(&mut self.sort, s, s.label());
                    }
                });
            }
            ui.add_space(8.0);
            ui.checkbox(&mut self.convert, "Convert EPUBs to kepub")
                .on_hover_text("Kobo's own format: page and chapter stats, smoother page turns, better highlights");
        });
        ui.add_space(6.0);
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages(ctx);
        let now = ctx.input(|i| i.time);
        if now - self.last_poll > 2.0 && self.busy.is_none() {
            self.last_poll = now;
            self.poll_device(ctx);
        }
        ctx.request_repaint_after(Duration::from_secs(2));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let (hovering, dropped) = ctx.input(|i| {
            (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect::<Vec<_>>())
        });
        if !dropped.is_empty() {
            self.add_files(&ctx, dropped);
        }
        if self.tab == Tab::Words {
            self.fetch_definitions(&ctx);
        }

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(busy) = &self.busy {
                    ui.spinner();
                    ui.label(busy);
                } else {
                    ui.add(egui::Label::new(RichText::new(&self.status).weak()).truncate());
                }
            });
        });
        if self.device.is_none() {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new("Plug in your Kobo. It'll show up here.").size(18.0).weak());
                });
            });
        } else if self.loading && self.lib.books.is_empty() {
            egui::CentralPanel::default_margins().show(ui, |ui| {
                ui.centered_and_justified(|ui| ui.spinner());
            });
        } else {
            match self.tab {
                Tab::Books => self.books_view(ui),
                Tab::Words => egui::CentralPanel::default_margins().show(ui, |ui| self.words_view(ui)).inner,
                Tab::Stats => egui::CentralPanel::default_margins().show(ui, |ui| self.stats_view(ui)).inner,
            }
        }

        if hovering && self.device.is_some() {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop")));
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(160));
            painter.text(screen.center(), egui::Align2::CENTER_CENTER, "Drop to add to your Kobo", FontId::proportional(24.0), Color32::WHITE);
        }
        self.show_dialog(&ctx);
    }
}

/// “Title” for one book, otherwise "3 books" or "49 previews".
fn describe(books: &[Book]) -> String {
    match books {
        [b] => format!("“{}”", b.title),
        _ if books.iter().all(|b| b.kind == Kind::Preview) => format!("{} previews", books.len()),
        _ => format!("{} books", books.len()),
    }
}

/// "28 h 37 m", "12 m", "under a minute".
fn duration(secs: u64) -> String {
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    match (h, m) {
        (0, 0) => "under a minute".into(),
        (0, m) => format!("{m} m"),
        (h, m) => format!("{h} h {m} m"),
    }
}

/// "20 Oct 2026" from a Kobo timestamp.
fn date(ts: &str) -> String {
    ts.get(..10)
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .map(|d| d.format("%-d %b %Y").to_string())
        .unwrap_or_default()
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// Decode a cover and shrink it to a sharp-enough thumbnail.
fn decode_cover(bytes: &[u8]) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = if img.height() > 400 { img.thumbnail(400 * img.width() / img.height(), 400) } else { img };
    let rgba = img.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

/// egui's built-in fonts don't cover Japanese or Chinese titles, so fall back
/// to the system's.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let candidates = [
        ("hiragino", "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc"),
        ("hiragino-gb", "/System/Library/Fonts/Hiragino Sans GB.ttc"),
        ("arial-unicode", "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
    ];
    for (name, path) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(bytes)));
            for fam in [FontFamily::Proportional, FontFamily::Monospace] {
                fonts.families.entry(fam).or_default().push(name.into());
            }
        }
    }
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations_and_dates() {
        assert_eq!(duration(103_019), "28 h 36 m");
        assert_eq!(duration(1977), "32 m");
        assert_eq!(duration(20), "under a minute");
        assert_eq!(date("2026-10-20T06:26:50.646"), "20 Oct 2026");
        assert_eq!(date(""), "");
    }
}
