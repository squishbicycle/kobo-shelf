//! Confirmations and small forms shown over the window.

use eframe::egui::{self, Color32, RichText};

use crate::edits::Edit;
use crate::kobo::{Book, Kind, Library, Status};
use crate::shelf::series_list;
use crate::{describe, App};

pub enum Dialog {
    Remove(Vec<Book>),
    MarkUnread(Vec<Book>),
    Series {
        books: Vec<Book>,
        name: String,
        /// The number, for a single book.
        number: String,
        /// For several books: number them in order from `start`.
        in_order: bool,
        start: String,
        existing: Vec<String>,
    },
    NewCollection { books: Vec<Book>, name: String },
    RenameCollection { key: String, name: String },
    DeleteCollection { key: String, name: String },
}

impl Dialog {
    /// Series form prefilled from the books' current series, if they share one.
    pub fn series(books: Vec<Book>, lib: &Library) -> Dialog {
        let first = books.first().map(|b| b.series.clone()).unwrap_or_default();
        let shared = if books.iter().all(|b| b.series == first) { first } else { String::new() };
        let number = match books.as_slice() {
            [b] => b.series_number.clone(),
            _ => String::new(),
        };
        let existing = series_list(lib).into_iter().map(|(n, _)| n).collect();
        Dialog::Series { books, name: shared, number, in_order: true, start: "1".into(), existing }
    }
}

const DANGER: Color32 = Color32::from_rgb(210, 70, 60);

/// What the dialog decided, acted on once the modal is drawn.
enum Outcome {
    Open,
    Close,
    Edit(Edit, String),
    Remove(Vec<Book>),
}

impl App {
    pub fn show_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else { return };
        let mut outcome = Outcome::Open;
        let modal = egui::Modal::new(egui::Id::unique("dialog")).show(ctx, |ui| {
            ui.set_max_width(400.0);
            outcome = draw(ui, &mut dialog, &self.lib);
        });
        if matches!(outcome, Outcome::Open) && modal.should_close() {
            outcome = Outcome::Close;
        }
        match outcome {
            Outcome::Open => self.dialog = Some(dialog),
            Outcome::Close => {}
            Outcome::Edit(edit, done) => self.edit(ctx, edit, done),
            Outcome::Remove(books) => self.remove(ctx, books),
        }
    }
}

/// Cancel plus a primary button; returns (cancelled, confirmed).
fn buttons(ui: &mut egui::Ui, label: &str, enabled: bool, danger: bool) -> (bool, bool) {
    ui.add_space(10.0);
    let mut out = (false, false);
    ui.horizontal(|ui| {
        out.0 = ui.button("Cancel").clicked();
        let text = if danger { RichText::new(label).color(DANGER) } else { RichText::new(label) };
        out.1 = ui.add_enabled(enabled, egui::Button::new(text)).clicked();
    });
    out
}

fn draw(ui: &mut egui::Ui, dialog: &mut Dialog, lib: &Library) -> Outcome {
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    match dialog {
        Dialog::Remove(books) => {
            ui.heading("Remove from Kobo?");
            ui.add_space(6.0);
            match books.as_slice() {
                [b] if b.kind == Kind::Preview => {
                    ui.label(format!("The preview of “{}” will be deleted from the Kobo.", b.title));
                }
                [b] => {
                    let by = if b.author.is_empty() { String::new() } else { format!(" by {}", b.author) };
                    ui.label(format!("“{}”{by} will be deleted from the Kobo.", b.title));
                    if let Status::Reading(p) = b.status {
                        ui.label(format!("You're {:.0}% through it, and that progress will be lost.", p * 100.0));
                    }
                }
                _ if books.iter().all(|b| b.kind == Kind::Preview) => {
                    ui.label(format!(
                        "The {} previews Kobo loaded at the factory will be deleted, along with the series they \
                         added. Your own books aren't touched.",
                        books.len()
                    ));
                }
                _ => {
                    ui.label(format!("These {} books will be deleted from the Kobo:", books.len()));
                    title_list(ui, books);
                    let reading = books.iter().filter(|b| matches!(b.status, Status::Reading(_))).count();
                    if reading > 0 {
                        ui.label(format!("You're partway through {reading} of them, and that progress will be lost."));
                    }
                }
            }
            if books.iter().any(|b| matches!(b.kind, Kind::Sideloaded | Kind::Preview)) {
                ui.label(RichText::new("The Kobo's database is backed up first.").weak().small());
            }
            if books.iter().any(|b| b.kind == Kind::Preview) {
                ui.label(
                    RichText::new("Preview files are moved to ~/Library/Application Support/Kobo Shelf/backups, not deleted.").weak().small(),
                );
            }
            match buttons(ui, "Remove", true, true) {
                (true, _) => Outcome::Close,
                (_, true) => Outcome::Remove(std::mem::take(books)),
                _ => Outcome::Open,
            }
        }
        Dialog::MarkUnread(books) => {
            ui.heading("Mark as unread?");
            ui.add_space(6.0);
            ui.label(format!(
                "{} will go back to the start, and {} reading progress will be reset.",
                describe(books),
                if books.len() == 1 { "its" } else { "their" }
            ));
            ui.label(RichText::new("Time-spent-reading stats are kept.").weak().small());
            match buttons(ui, "Mark unread", true, false) {
                (true, _) => Outcome::Close,
                (_, true) => {
                    let what = describe(books);
                    let ids = books.iter().map(|b| b.content_id.clone()).collect();
                    Outcome::Edit(Edit::MarkUnread(ids), format!("Marked {what} as unread."))
                }
                _ => Outcome::Open,
            }
        }
        Dialog::Series { books, name, number, in_order, start, existing } => {
            ui.heading(if books.len() == 1 { "Series" } else { "Set series" });
            ui.add_space(6.0);
            ui.label(match books.as_slice() {
                [b] => format!("For “{}”", b.title),
                _ => format!("For {} books", books.len()),
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Series");
                ui.add(egui::TextEdit::singleline(name).hint_text("e.g. The Expanse").desired_width(260.0));
            });
            let others: Vec<&String> = existing.iter().filter(|e| *e != name).collect();
            if !others.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Existing:").weak().small());
                    for e in others {
                        if ui.small_button(e.as_str()).clicked() {
                            *name = e.clone();
                        }
                    }
                });
            }
            let valid_number = |s: &str| s.trim().is_empty() || s.trim().parse::<f64>().is_ok();
            let ok = match books.as_slice() {
                [_] => {
                    ui.horizontal(|ui| {
                        ui.label("Number");
                        ui.add(egui::TextEdit::singleline(number).hint_text("1").desired_width(60.0));
                    });
                    valid_number(number)
                }
                _ => {
                    ui.horizontal(|ui| {
                        ui.checkbox(in_order, "Number them in this order, starting at");
                        ui.add_enabled(*in_order, egui::TextEdit::singleline(start).desired_width(40.0));
                    });
                    if *in_order {
                        title_list(ui, books);
                    }
                    !*in_order || start.trim().parse::<f64>().is_ok()
                }
            };
            ui.add_space(10.0);
            let mut out = Outcome::Open;
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    out = Outcome::Close;
                }
                if books.iter().any(|b| !b.series.is_empty()) && ui.button("Take out of series").clicked() {
                    let what = describe(books);
                    let ids: Vec<String> = books.iter().map(|b| b.content_id.clone()).collect();
                    let numbers = vec![None; ids.len()];
                    out = Outcome::Edit(Edit::SetSeries { ids, name: String::new(), numbers }, format!("Took {what} out of their series."));
                }
                let can_save = ok && !name.trim().is_empty();
                if ui.add_enabled(can_save, egui::Button::new("Save")).clicked() || (can_save && enter) {
                    let ids: Vec<String> = books.iter().map(|b| b.content_id.clone()).collect();
                    let numbers = match books.as_slice() {
                        [_] => vec![Some(number.trim().to_string()).filter(|n| !n.is_empty())],
                        _ if *in_order => {
                            let first: f64 = start.trim().parse().unwrap_or(1.0);
                            (0..ids.len()).map(|i| Some(format_number(first + i as f64))).collect()
                        }
                        _ => vec![None; ids.len()],
                    };
                    let what = describe(books);
                    let series = name.trim().to_string();
                    out = Outcome::Edit(Edit::SetSeries { ids, name: series.clone(), numbers }, format!("Put {what} in “{series}”."));
                }
            });
            out
        }
        Dialog::NewCollection { books, name } => {
            ui.heading("New collection");
            ui.add_space(6.0);
            let resp = ui.add(egui::TextEdit::singleline(name).hint_text("Name").desired_width(300.0));
            resp.request_focus();
            if !books.is_empty() {
                ui.label(RichText::new(format!("{} will be added to it.", describe(books))).weak());
            }
            let exists = lib.shelves.iter().any(|s| s.name.eq_ignore_ascii_case(name.trim()));
            if exists {
                ui.label(RichText::new("There's already a collection with that name; the books will be added to it.").weak().small());
            }
            let valid = !name.trim().is_empty();
            match buttons(ui, if exists { "Add" } else { "Create" }, valid, false) {
                (true, _) => Outcome::Close,
                (_, true) => create(books, name),
                _ if valid && enter => create(books, name),
                _ => Outcome::Open,
            }
        }
        Dialog::RenameCollection { key, name } => {
            ui.heading("Rename collection");
            ui.add_space(6.0);
            ui.add(egui::TextEdit::singleline(name).desired_width(300.0)).request_focus();
            let taken = lib.shelves.iter().any(|s| &s.key != key && s.name.eq_ignore_ascii_case(name.trim()));
            if taken {
                ui.label(RichText::new("Another collection already has that name.").color(DANGER).small());
            }
            let valid = !name.trim().is_empty() && !taken;
            let go = |key: &String, name: &String| {
                let to = name.trim().to_string();
                Outcome::Edit(Edit::RenameCollection { key: key.clone(), to: to.clone() }, format!("Renamed the collection to “{to}”."))
            };
            match buttons(ui, "Rename", valid, false) {
                (true, _) => Outcome::Close,
                (_, true) => go(key, name),
                _ if valid && enter => go(key, name),
                _ => Outcome::Open,
            }
        }
        Dialog::DeleteCollection { key, name } => {
            ui.heading("Delete collection?");
            ui.add_space(6.0);
            ui.label(format!("The collection “{name}” will be deleted. The books in it stay on your Kobo."));
            match buttons(ui, "Delete", true, true) {
                (true, _) => Outcome::Close,
                (_, true) => Outcome::Edit(Edit::DeleteCollection { key: key.clone() }, format!("Deleted the collection “{name}”.")),
                _ => Outcome::Open,
            }
        }
    }
}

fn create(books: &[Book], name: &str) -> Outcome {
    let name = name.trim().to_string();
    let ids = books.iter().map(|b| b.content_id.clone()).collect();
    let done = if books.is_empty() {
        format!("Created the collection “{name}”.")
    } else {
        format!("Added {} to “{name}”.", describe(books))
    };
    Outcome::Edit(Edit::AddToCollection { name, ids }, done)
}

/// Up to eight titles, then "and N more".
fn title_list(ui: &mut egui::Ui, books: &[Book]) {
    for (i, b) in books.iter().take(8).enumerate() {
        ui.label(RichText::new(format!("{}. {}", i + 1, b.title)).small());
    }
    if books.len() > 8 {
        ui.label(RichText::new(format!("…and {} more", books.len() - 8)).small().weak());
    }
}

/// 3.0 → "3", 2.5 → "2.5".
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 { format!("{}", n as i64) } else { format!("{n}") }
}
