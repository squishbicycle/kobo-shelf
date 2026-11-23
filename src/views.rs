//! The My Words and Stats tabs.

use eframe::egui::{self, RichText};

use crate::kobo::{Kind, Status};
use crate::shelf::title_key;
use crate::{date, duration, home, words, App};

impl App {
    pub fn words_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("My Words");
            ui.label(RichText::new(format!("{} saved on your Kobo", self.lib.words.len())).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = !self.defining && !self.lib.words.is_empty();
                if ui.add_enabled(ready, egui::Button::new("Export for Anki…")).on_hover_text("Tab-separated; in Anki use File → Import").clicked() {
                    self.export_words(true);
                }
                if ui.add_enabled(ready, egui::Button::new("Export CSV…")).on_hover_text("Opens in Numbers or Excel").clicked() {
                    self.export_words(false);
                }
                if self.defining {
                    ui.spinner();
                    ui.label(RichText::new("Looking up definitions…").weak());
                }
            });
        });
        ui.add_space(8.0);
        if self.lib.words.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("No saved words yet. Look up a word on your Kobo and save it to My Words.").weak());
            });
            return;
        }
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.set_max_width(760.0);
            for w in &self.lib.words {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&w.text).strong().size(16.0));
                    let mut from = date(&w.saved);
                    if !w.book.is_empty() {
                        from = format!("{} · {from}", w.book);
                    }
                    ui.label(RichText::new(from).weak().small());
                });
                match self.definitions.get(&w.text) {
                    Some(Some(def)) => {
                        ui.add(egui::Label::new(def.as_str()).wrap());
                    }
                    Some(None) => {
                        ui.label(RichText::new("No definition found on this Mac.").weak().italics());
                    }
                    None => {
                        ui.label(RichText::new("…").weak());
                    }
                }
                ui.add_space(4.0);
                ui.separator();
            }
        });
    }

    fn export_words(&mut self, anki: bool) {
        let rows: Vec<_> = self.lib.words.iter().map(|w| (w.clone(), self.definitions.get(&w.text).cloned().flatten())).collect();
        let (name, ext, filter) = if anki { ("Kobo words (Anki).txt", "txt", "Anki import") } else { ("Kobo words.csv", "csv", "CSV") };
        let Some(path) = rfd::FileDialog::new()
            .set_directory(home().join("Documents"))
            .set_file_name(name)
            .add_filter(filter, &[ext])
            .save_file()
        else {
            return;
        };
        let text = if anki { words::to_anki(&rows) } else { words::to_csv(&rows) };
        self.status = match std::fs::write(&path, text) {
            Ok(()) => format!("Exported {} words to {}.", rows.len(), path.display()),
            Err(e) => format!("Couldn't save the words: {e}"),
        };
    }

    pub fn stats_view(&mut self, ui: &mut egui::Ui) {
        let books: Vec<_> = self.lib.books.iter().filter(|b| b.in_db() && b.kind != Kind::Preview).collect();
        let total: u64 = books.iter().map(|b| b.time_spent).sum();
        let finished = books.iter().filter(|b| b.status == Status::Finished).count();
        let year = chrono::Local::now().format("%Y").to_string();
        let this_year = books.iter().filter(|b| b.status == Status::Finished && b.last_read.starts_with(&year)).count();
        let mut reading: Vec<_> = books.iter().filter(|b| matches!(b.status, Status::Reading(_))).collect();
        reading.sort_by(|a, b| b.last_read.cmp(&a.last_read));

        ui.heading("Reading stats");
        ui.label(RichText::new("From what your Kobo has tracked.").weak());
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            tile(ui, &duration(total), "time reading");
            tile(ui, &finished.to_string(), "books finished");
            tile(ui, &this_year.to_string(), &format!("finished in {year}"));
            tile(ui, &reading.len().to_string(), "reading now");
            tile(ui, &self.lib.words.len().to_string(), "words saved");
        });

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if !reading.is_empty() {
                ui.add_space(14.0);
                ui.label(RichText::new("Reading now").strong());
                for b in &reading {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&b.title).strong());
                        ui.label(RichText::new(&b.author).weak());
                    });
                    let mut line = format!("{:.0}% · read for {}", b.progress() * 100.0, duration(b.time_spent));
                    if b.time_left > 0 {
                        line.push_str(&format!(" · about {} left", duration(b.time_left)));
                    }
                    ui.horizontal(|ui| {
                        ui.add(egui::ProgressBar::new(b.progress()).desired_width(160.0).desired_height(6.0));
                        ui.label(RichText::new(line).weak());
                    });
                }
            }

            ui.add_space(14.0);
            ui.label(RichText::new("All books").strong());
            ui.add_space(4.0);
            let mut all = books.clone();
            all.sort_by(|a, b| b.time_spent.cmp(&a.time_spent).then_with(|| title_key(&a.title).cmp(&title_key(&b.title))));
            egui::Grid::new("stats").striped(true).num_columns(4).min_col_width(90.0).spacing([24.0, 6.0]).show(ui, |ui| {
                for h in ["Book", "Status", "Time read", "Last read"] {
                    ui.label(RichText::new(h).small().weak());
                }
                ui.end_row();
                for b in all {
                    ui.add(egui::Label::new(&b.title).truncate());
                    ui.label(match b.status {
                        Status::Unread => "Unread".to_string(),
                        Status::Reading(p) => format!("{:.0}%", p * 100.0),
                        Status::Finished => "Finished".to_string(),
                    });
                    ui.label(if b.time_spent > 0 { duration(b.time_spent) } else { "—".into() });
                    ui.label(date(&b.last_read));
                    ui.end_row();
                }
            });
        });
    }
}

fn tile(ui: &mut egui::Ui, value: &str, label: &str) {
    egui::Frame::group(ui.style()).inner_margin(12.0).show(ui, |ui| {
        ui.set_min_width(120.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(value).size(22.0).strong());
            ui.label(RichText::new(label).weak());
        });
    });
}
