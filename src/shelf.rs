//! The Books tab: sidebar, cover grid, selection and the actions that work on
//! a selection.

use std::collections::HashSet;
use std::process::Command;

use eframe::egui::{self, text::LayoutJob, Align, Color32, FontId, Modifiers, Rect, RichText, Sense, TextFormat, Vec2};

use crate::dialogs::Dialog;
use crate::edits::Edit;
use crate::kobo::{Book, Kind, Library, Status};
use crate::{describe, App, Sort};

const CARD_W: f32 = 132.0;
const COVER_H: f32 = 198.0;
const CARD_H: f32 = COVER_H + 90.0;
const GAP: f32 = 18.0;

/// What the grid is showing, picked in the sidebar.
#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    All,
    Unread,
    Reading,
    Finished,
    Pending,
    FromKobo,
    /// By collection key.
    Collection(String),
    Series(String),
}

impl Scope {
    fn matches(&self, b: &Book, lib: &Library) -> bool {
        match self {
            Scope::All => true,
            Scope::Unread => b.status == Status::Unread && b.in_db(),
            Scope::Reading => matches!(b.status, Status::Reading(_)),
            Scope::Finished => b.status == Status::Finished,
            Scope::Pending => b.kind == Kind::Pending,
            Scope::FromKobo => matches!(b.kind, Kind::Store | Kind::Preview),
            Scope::Collection(key) => lib.shelves.iter().any(|s| &s.key == key && s.books.contains(&b.content_id)),
            Scope::Series(name) => &b.series == name,
        }
    }

    /// Whether this still points at something after a reload.
    pub fn exists(&self, lib: &Library) -> bool {
        match self {
            Scope::Collection(key) => lib.shelves.iter().any(|s| &s.key == key),
            Scope::Series(name) => lib.books.iter().any(|b| &b.series == name),
            _ => true,
        }
    }
}

/// Series names on the Kobo, with how many books each has.
pub fn series_list(lib: &Library) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = Vec::new();
    for b in lib.books.iter().filter(|b| !b.series.is_empty()) {
        match v.iter_mut().find(|(n, _)| n == &b.series) {
            Some((_, c)) => *c += 1,
            None => v.push((b.series.clone(), 1)),
        }
    }
    v.sort_by_key(|(n, _)| n.to_lowercase());
    v
}

impl App {
    pub fn visible_books(&self) -> Vec<Book> {
        let q = self.search.trim().to_lowercase();
        let mut v: Vec<Book> = self
            .lib
            .books
            .iter()
            .filter(|b| self.scope.matches(b, &self.lib))
            .filter(|b| q.is_empty() || b.title.to_lowercase().contains(&q) || b.author.to_lowercase().contains(&q))
            .cloned()
            .collect();
        if matches!(self.scope, Scope::Series(_)) {
            v.sort_by(|a, b| series_number(a).total_cmp(&series_number(b)).then_with(|| title_key(&a.title).cmp(&title_key(&b.title))));
            return v;
        }
        match self.sort {
            Sort::Title => v.sort_by_cached_key(|b| title_key(&b.title)),
            Sort::Author => v.sort_by_cached_key(|b| (author_key(&b.author), title_key(&b.title))),
            Sort::RecentlyRead => v.sort_by(|a, b| b.last_read.cmp(&a.last_read)),
            Sort::Progress => v.sort_by(|a, b| b.progress().total_cmp(&a.progress())),
        }
        v
    }

    /// Selected books, in the order they're shown.
    fn selection(&self, visible: &[Book]) -> Vec<Book> {
        visible.iter().filter(|b| self.selected.contains(&b.content_id)).cloned().collect()
    }

    pub fn books_view(&mut self, ui: &mut egui::Ui) {
        let visible = self.visible_books();
        // Only what's on screen stays selected, so actions never reach hidden books.
        let shown: HashSet<&String> = visible.iter().map(|b| &b.content_id).collect();
        self.selected.retain(|id| shown.contains(id));

        let ctx = ui.ctx().clone();
        if self.dialog.is_none() && ctx.memory(|m| m.focused().is_none()) {
            if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, egui::Key::A)) {
                self.selected = visible.iter().map(|b| b.content_id.clone()).collect();
            }
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, egui::Key::Escape)) {
                self.selected.clear();
            }
        }

        let selection = self.selection(&visible);
        if !selection.is_empty() {
            egui::Panel::bottom("actions").show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("{} selected", selection.len())).strong());
                    ui.separator();
                    self.actions(ui, &selection, false);
                    ui.separator();
                    if ui.button("Clear selection").clicked() {
                        self.selected.clear();
                    }
                });
                ui.add_space(4.0);
            });
        }
        egui::Panel::left("sidebar").resizable(false).exact_size(200.0).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.sidebar(ui));
        });
        egui::CentralPanel::default_margins().show(ui, |ui| self.grid(ui, &visible));
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let count = |s: &Scope| self.lib.books.iter().filter(|b| s.matches(b, &self.lib)).count();
        ui.add_space(8.0);
        ui.label(RichText::new("LIBRARY").small().weak());
        let mut entries = vec![(Scope::All, "All books"), (Scope::Unread, "Unread"), (Scope::Reading, "Reading"), (Scope::Finished, "Finished")];
        if count(&Scope::Pending) > 0 {
            entries.push((Scope::Pending, "Waiting to import"));
        }
        if count(&Scope::FromKobo) > 0 {
            entries.push((Scope::FromKobo, "From Kobo"));
        }
        for (scope, label) in entries {
            let n = count(&scope);
            if ui.selectable_label(self.scope == scope, format!("{label}  ({n})")).clicked() {
                self.scope = scope;
            }
        }

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("COLLECTIONS").small().weak());
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(self.busy.is_none(), egui::Button::new("+").small()).on_hover_text("New collection").clicked() {
                    self.dialog = Some(Dialog::NewCollection { books: Vec::new(), name: String::new() });
                }
            });
        });
        if self.lib.shelves.is_empty() {
            ui.label(RichText::new("None yet").weak().small());
        }
        for shelf in self.lib.shelves.clone() {
            let scope = Scope::Collection(shelf.key.clone());
            let n = shelf.books.iter().filter(|id| self.lib.books.iter().any(|b| &b.content_id == *id)).count();
            let resp = ui.selectable_label(self.scope == scope, format!("{}  ({n})", shelf.name));
            if resp.clicked() {
                self.scope = scope;
            }
            resp.context_menu(|ui| {
                if ui.button("Rename…").clicked() {
                    self.dialog = Some(Dialog::RenameCollection { key: shelf.key.clone(), name: shelf.name.clone() });
                    ui.close();
                }
                if ui.button("Delete collection…").clicked() {
                    self.dialog = Some(Dialog::DeleteCollection { key: shelf.key.clone(), name: shelf.name.clone() });
                    ui.close();
                }
            });
        }

        let series = series_list(&self.lib);
        if !series.is_empty() {
            ui.add_space(12.0);
            ui.label(RichText::new("SERIES").small().weak());
            for (name, n) in series {
                let scope = Scope::Series(name.clone());
                if ui.selectable_label(self.scope == scope, format!("{name}  ({n})")).clicked() {
                    self.scope = scope;
                }
            }
        }
    }

    fn grid(&mut self, ui: &mut egui::Ui, books: &[Book]) {
        if books.is_empty() {
            ui.centered_and_justified(|ui| {
                let msg = if self.lib.books.is_empty() { "No books on this Kobo yet. Drop some EPUBs here." } else { "No books here." };
                ui.label(RichText::new(msg).weak());
            });
            return;
        }
        let cols = (((ui.available_width() + GAP) / (CARD_W + GAP)).floor() as usize).max(1);
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(8.0);
            for row in books.chunks(cols) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    for b in row {
                        self.card(ui, b, books);
                    }
                });
                ui.add_space(GAP);
            }
        });
    }

    fn click(&mut self, id: &str, visible: &[Book], mods: Modifiers) {
        if mods.command {
            if !self.selected.remove(id) {
                self.selected.insert(id.to_string());
            }
            self.anchor = Some(id.to_string());
        } else if mods.shift {
            let pos = |x: &str| visible.iter().position(|b| b.content_id == x);
            match (self.anchor.as_deref().and_then(pos), pos(id)) {
                (Some(a), Some(b)) => {
                    let (lo, hi) = (a.min(b), a.max(b));
                    self.selected.extend(visible[lo..=hi].iter().map(|b| b.content_id.clone()));
                }
                _ => {
                    self.selected.insert(id.to_string());
                    self.anchor = Some(id.to_string());
                }
            }
        } else {
            self.selected = HashSet::from([id.to_string()]);
            self.anchor = Some(id.to_string());
        }
    }

    fn card(&mut self, ui: &mut egui::Ui, b: &Book, visible: &[Book]) {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(CARD_W, CARD_H), Sense::click());
        let visuals = ui.visuals().clone();
        let selected = self.selected.contains(&b.content_id);
        let painter = ui.painter_at(rect.expand(6.0));
        if selected {
            painter.rect_filled(rect.expand(5.0), 6.0, visuals.selection.bg_fill.gamma_multiply(0.35));
        }
        let cover_area = Rect::from_min_size(rect.min, Vec2::new(CARD_W, COVER_H));

        // Cover, scaled to fit and sitting on the shelf line.
        let cover = match self.covers.get(&b.content_id) {
            Some(tex) => {
                let size = tex.size_vec2();
                let s = size * (CARD_W / size.x).min(COVER_H / size.y);
                let r = Rect::from_min_size(egui::pos2(cover_area.center().x - s.x / 2.0, cover_area.max.y - s.y), s);
                painter.image(tex.id(), r, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                r
            }
            None => {
                let r = cover_area.shrink2(Vec2::new(6.0, 0.0));
                painter.rect_filled(r, 3.0, visuals.faint_bg_color.gamma_multiply(2.0));
                let g = painter.layout_job(wrapped(&b.title, 15.0, visuals.weak_text_color(), r.width() - 16.0, 6));
                painter.galley(r.center() - g.size() / 2.0, g, visuals.text_color());
                r
            }
        };
        if selected {
            painter.rect_stroke(cover, 3.0, egui::Stroke::new(2.5, visuals.selection.stroke.color), egui::StrokeKind::Outside);
            let c = egui::pos2(cover.max.x - 12.0, cover.max.y - 12.0);
            painter.circle_filled(c, 10.0, visuals.selection.stroke.color);
            painter.text(c, egui::Align2::CENTER_CENTER, "✔", FontId::proportional(12.0), Color32::WHITE);
        } else if resp.hovered() {
            painter.rect_stroke(cover, 3.0, visuals.selection.stroke, egui::StrokeKind::Outside);
        }
        let badge = match b.kind {
            Kind::Store => Some(("STORE", Color32::from_rgb(70, 110, 170))),
            Kind::Preview => Some(("PREVIEW", Color32::from_rgb(190, 120, 40))),
            Kind::Pending => Some(("NEW", Color32::from_rgb(60, 140, 90))),
            Kind::Sideloaded => None,
        };
        if let Some((text, color)) = badge {
            let g = painter.layout_no_wrap(text.into(), FontId::proportional(10.0), Color32::WHITE);
            let r = Rect::from_min_size(cover.min + Vec2::splat(4.0), g.size() + Vec2::new(8.0, 2.0));
            painter.rect_filled(r, 3.0, color);
            painter.galley(r.min + Vec2::new(4.0, 1.0), g, Color32::WHITE);
        }

        // Title, author, series and progress under the cover.
        let mut y = cover_area.max.y + 6.0;
        let lines = [
            (b.title.clone(), 13.0, visuals.strong_text_color(), 2),
            (b.author.clone(), 11.5, visuals.weak_text_color(), 1),
            (series_label(b), 11.0, visuals.weak_text_color(), 1),
        ];
        for (text, size, color, rows) in lines {
            let g = painter.layout_job(wrapped(&text, size, color, CARD_W, rows));
            let h = g.size().y.max(size + 3.0);
            painter.galley(egui::pos2(rect.min.x, y), g, visuals.text_color());
            y += h + 1.0;
        }
        y += 2.0;
        let (label, frac) = match (b.kind, b.status) {
            (Kind::Pending, _) => ("Imports on eject".to_string(), None),
            (_, Status::Unread) => ("Unread".to_string(), None),
            (_, Status::Reading(p)) => (format!("{:.0}%", p * 100.0), Some(p)),
            (_, Status::Finished) => ("Finished".to_string(), None),
        };
        let mut text_x = rect.min.x;
        if let Some(p) = frac {
            let bar = Rect::from_min_size(egui::pos2(rect.min.x, y + 4.0), Vec2::new(CARD_W - 40.0, 4.0));
            painter.rect_filled(bar, 2.0, visuals.extreme_bg_color);
            painter.rect_filled(Rect::from_min_size(bar.min, Vec2::new(bar.width() * p, 4.0)), 2.0, visuals.selection.bg_fill);
            text_x = bar.max.x + 6.0;
        }
        painter.text(egui::pos2(text_x, y), egui::Align2::LEFT_TOP, label, FontId::proportional(11.0), visuals.weak_text_color());

        // In a collection or series, ✕ takes the book out of it; elsewhere it
        // removes the book from the Kobo.
        let group = match &self.scope {
            Scope::Collection(key) => self.lib.shelves.iter().find(|s| &s.key == key).map(|s| (s.name.clone(), Some(s.key.clone()))),
            Scope::Series(name) => Some((name.clone(), None)),
            _ => None,
        };
        let removable = self.busy.is_none() && if group.is_some() { b.in_db() } else { b.kind != Kind::Store };
        if removable && (resp.hovered() || ui.rect_contains_pointer(cover)) {
            let r = Rect::from_min_size(egui::pos2(cover.max.x - 26.0, cover.min.y + 4.0), Vec2::splat(22.0));
            let tip = match &group {
                Some((name, _)) => format!("Remove from “{name}” (the book stays on your Kobo)"),
                None => "Remove from Kobo".to_string(),
            };
            if ui.put(r, egui::Button::new("✕").corner_radius(11.0)).on_hover_text(tip).clicked() {
                let ctx = ui.ctx().clone();
                let ids = vec![b.content_id.clone()];
                match group {
                    Some((name, Some(key))) => {
                        let done = format!("Took “{}” out of “{name}”.", b.title);
                        self.edit(&ctx, Edit::RemoveFromCollection { key, ids }, done);
                    }
                    Some((name, None)) => {
                        let done = format!("Took “{}” out of “{name}”.", b.title);
                        self.edit(&ctx, Edit::SetSeries { ids, name: String::new(), numbers: vec![None] }, done);
                    }
                    None => self.dialog = Some(Dialog::Remove(vec![b.clone()])),
                }
            }
        }

        let mods = ui.input(|i| i.modifiers);
        if resp.clicked() {
            self.click(&b.content_id, visible, mods);
        }
        if resp.secondary_clicked() && !selected {
            self.click(&b.content_id, visible, Modifiers::NONE);
        }

        let mut tip = format!("{}\n{}", b.title, b.author);
        if b.time_spent > 0 {
            tip.push_str(&format!("\nRead for {}", crate::duration(b.time_spent)));
        }
        match b.kind {
            Kind::Store => tip.push_str("\nKobo Store book: remove it on the Kobo itself"),
            Kind::Preview => tip.push_str("\nPreview Kobo loaded at the factory"),
            Kind::Pending => tip.push_str("\nThe Kobo imports this when you eject"),
            Kind::Sideloaded => {}
        }
        let resp = resp.on_hover_text(tip);
        resp.context_menu(|ui| {
            let targets = if self.selected.contains(&b.content_id) { self.selection(visible) } else { vec![b.clone()] };
            if let [one] = targets.as_slice() {
                if let Some(path) = one.path.as_ref().filter(|_| matches!(one.kind, Kind::Sideloaded | Kind::Pending)) {
                    if ui.button("Show in Finder").clicked() {
                        let _ = Command::new("/usr/bin/open").arg("-R").arg(path).spawn();
                        ui.close();
                    }
                    ui.separator();
                }
            }
            self.actions(ui, &targets, true);
        });
    }

    /// Buttons for what can be done to `books`. Shared by the action bar and
    /// the right-click menu (`in_menu` stacks them and closes after a click).
    fn actions(&mut self, ui: &mut egui::Ui, books: &[Book], in_menu: bool) {
        let ctx = ui.ctx().clone();
        let idle = self.busy.is_none();
        let in_db: Vec<Book> = books.iter().filter(|b| b.in_db()).cloned().collect();
        let ids: Vec<String> = in_db.iter().map(|b| b.content_id.clone()).collect();
        let waiting = if in_db.len() < books.len() { "\nNew books can't be changed until the Kobo imports them (eject first)" } else { "" };
        let mut clicked = false;

        let finished = ui.add_enabled(idle && !ids.is_empty(), egui::Button::new("Mark finished")).on_hover_text(format!("Mark as finished{waiting}"));
        if finished.clicked() {
            let what = describe(&in_db);
            self.edit(&ctx, Edit::MarkFinished(ids.clone()), format!("Marked {what} as finished."));
            clicked = true;
        }
        let unread = ui.add_enabled(idle && !ids.is_empty(), egui::Button::new("Mark unread")).on_hover_text(format!("Mark as unread and start over{waiting}"));
        if unread.clicked() {
            if in_db.iter().any(|b| matches!(b.status, Status::Reading(_) | Status::Finished)) {
                self.dialog = Some(Dialog::MarkUnread(in_db.clone()));
            } else {
                let what = describe(&in_db);
                self.edit(&ctx, Edit::MarkUnread(ids.clone()), format!("Marked {what} as unread."));
            }
            clicked = true;
        }
        ui.add_enabled_ui(idle && !ids.is_empty(), |ui| {
            ui.menu_button("Series", |ui| {
                let series = series_list(&self.lib);
                // A book has one series (unlike collections), so this moves it.
                if !series.is_empty() {
                    ui.label(RichText::new("Move to").small().weak())
                        .on_hover_text("A book can be in one series; this replaces its current one");
                }
                for (name, _) in series {
                    let joining: Vec<&Book> = in_db.iter().filter(|b| b.series != name).collect();
                    let label = if joining.is_empty() { format!("✔ {name}") } else { name.clone() };
                    if ui.add_enabled(!joining.is_empty(), egui::Button::new(label)).on_hover_text("Numbered after the last book in it. Replaces any series the book is already in.").clicked() {
                        // Carry on from the series' highest number, in the order shown.
                        let last = self
                            .lib
                            .books
                            .iter()
                            .filter(|b| b.series == name)
                            .filter_map(|b| b.series_number.trim().parse::<f64>().ok())
                            .fold(0.0_f64, f64::max)
                            .floor();
                        let numbers = (1..=joining.len()).map(|i| Some(format!("{}", last as i64 + i as i64))).collect();
                        let joined: Vec<Book> = joining.iter().map(|b| (*b).clone()).collect();
                        let ids = joined.iter().map(|b| b.content_id.clone()).collect();
                        let done = format!("Moved {} to “{name}”.", describe(&joined));
                        self.edit(&ctx, Edit::SetSeries { ids, name: name.clone(), numbers }, done);
                        clicked = true;
                        ui.close();
                    }
                }
                if ui.button("Set series…").on_hover_text("Choose a name and numbers").clicked() {
                    self.dialog = Some(Dialog::series(in_db.clone(), &self.lib));
                    clicked = true;
                    ui.close();
                }
                let in_series: Vec<Book> = in_db.iter().filter(|b| !b.series.is_empty()).cloned().collect();
                if !in_series.is_empty() {
                    ui.separator();
                    if ui.button("Take out of series").clicked() {
                        let ids: Vec<String> = in_series.iter().map(|b| b.content_id.clone()).collect();
                        let numbers = vec![None; ids.len()];
                        let done = format!("Took {} out of their series.", describe(&in_series));
                        self.edit(&ctx, Edit::SetSeries { ids, name: String::new(), numbers }, done);
                        clicked = true;
                        ui.close();
                    }
                }
            });
        });
        ui.add_enabled_ui(idle && !ids.is_empty(), |ui| {
            ui.menu_button("Collections", |ui| {
                ui.label(RichText::new("Add to").small().weak());
                for shelf in self.lib.shelves.clone() {
                    let already = ids.iter().all(|id| shelf.books.contains(id));
                    let label = if already { format!("✔ {}", shelf.name) } else { shelf.name.clone() };
                    if ui.add_enabled(!already, egui::Button::new(label)).clicked() {
                        let what = describe(&in_db);
                        let name = shelf.name.clone();
                        self.edit(&ctx, Edit::AddToCollection { name: shelf.name, ids: ids.clone() }, format!("Added {what} to “{name}”."));
                        clicked = true;
                        ui.close();
                    }
                }
                if ui.button("New collection…").clicked() {
                    self.dialog = Some(Dialog::NewCollection { books: in_db.clone(), name: String::new() });
                    clicked = true;
                    ui.close();
                }
                // Only the collections these books are actually in.
                let containing: Vec<_> =
                    self.lib.shelves.iter().filter(|s| ids.iter().any(|id| s.books.contains(id))).cloned().collect();
                if !containing.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("Remove from").small().weak());
                    for shelf in containing {
                        if ui.button(&shelf.name).clicked() {
                            let members: Vec<String> = ids.iter().filter(|id| shelf.books.contains(id)).cloned().collect();
                            let what = describe(&in_db.iter().filter(|b| members.contains(&b.content_id)).cloned().collect::<Vec<_>>());
                            let done = format!("Took {what} out of “{}”.", shelf.name);
                            self.edit(&ctx, Edit::RemoveFromCollection { key: shelf.key, ids: members }, done);
                            clicked = true;
                            ui.close();
                        }
                    }
                }
            });
        });
        if in_menu {
            ui.separator();
        }
        let removable: Vec<Book> = books.iter().filter(|b| b.kind != Kind::Store).cloned().collect();
        let label = RichText::new("Remove from Kobo…").color(Color32::from_rgb(210, 70, 60));
        if ui.add_enabled(idle && !removable.is_empty(), egui::Button::new(label)).clicked() {
            self.dialog = Some(Dialog::Remove(removable));
            clicked = true;
        }
        if in_menu && clicked {
            ui.close();
        }
    }
}

fn series_label(b: &Book) -> String {
    match (b.series.as_str(), b.series_number.as_str()) {
        ("", _) => String::new(),
        (s, "") => s.to_string(),
        (s, n) => format!("{s} #{n}"),
    }
}

fn series_number(b: &Book) -> f64 {
    b.series_number.trim().parse().unwrap_or(f64::MAX)
}

fn wrapped(text: &str, size: f32, color: Color32, width: f32, rows: usize) -> LayoutJob {
    let mut job = LayoutJob::single_section(text.to_string(), TextFormat { font_id: FontId::proportional(size), color, ..Default::default() });
    job.wrap.max_width = width;
    job.wrap.max_rows = rows;
    job.wrap.break_anywhere = false;
    job.halign = Align::LEFT;
    job
}

/// "The Devils" sorts under D.
pub fn title_key(t: &str) -> String {
    let t = t.trim().to_lowercase();
    for article in ["the ", "a ", "an "] {
        if let Some(rest) = t.strip_prefix(article) {
            return rest.to_string();
        }
    }
    t
}

/// Surname of the first author: "Joe Abercrombie, Someone" → "abercrombie".
fn author_key(a: &str) -> String {
    let first = a.split(',').next().unwrap_or("").trim();
    first.rsplit(' ').next().unwrap_or(first).to_lowercase()
}
