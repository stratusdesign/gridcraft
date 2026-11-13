//! Android's Open and Save As pages: one folder browser for both, over the locations the user let
//! GridCraft use ([`crate::Folders`]). ChromeOS shows its own picker to Android apps only for
//! Open, so GridCraft browses folders itself and both pages look and work the same: locations and
//! recent folders on the left, the folder's contents on the right, and on Save As a name box.

use egui::{Align2, Rect, Sense, Ui, pos2, vec2};
use serde_json::json;

use super::{CONTENT_W, Outcome, dim, section};
use crate::icons::{self, Icon};
use crate::theme::{self, Tokens};
use crate::{FolderItem, SheetApp};

/// Width of the locations column.
const SIDE_W: f32 = 230.0;

/// Files the browser shows: what GridCraft opens.
const SHOWN: [&str; 9] = ["xlsx", "xlsm", "xls", "xlsb", "ods", "csv", "tsv", "txt", "json"];

/// The browser's place on the page; a page opened anew starts afresh.
#[derive(Clone, Debug, Default)]
pub struct State {
    /// The folders from the starting location down to the one shown: (id, name).
    trail: Vec<(String, String)>,
    /// The shown folder's contents, folders first; `None` until asked.
    listing: Option<Result<Vec<FolderItem>, String>>,
    /// Save As: the file name being typed.
    name: Option<String>,
    /// Save As: the existing file the user has been asked to confirm replacing.
    replace: Option<String>,
}

impl State {
    fn go(&mut self, trail: Vec<(String, String)>) {
        self.trail = trail;
        self.listing = None;
        self.replace = None;
    }
}

fn shown(item: &FolderItem) -> bool {
    item.folder
        || std::path::Path::new(&item.name)
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|e| SHOWN.iter().any(|s| e.eq_ignore_ascii_case(s)))
}

/// One clickable row: icon, name and (when given) a dimmer line under it.
fn row(ui: &mut Ui, icon: Icon, name: &str, detail: &str, selected: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let height = if detail.is_empty() { 34.0 } else { 44.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, name));
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 4.0, t.accent_soft);
    } else if resp.hovered() {
        p.rect_filled(rect, 4.0, t.hover);
    }
    icons::paint(p, Rect::from_center_size(pos2(rect.left() + 18.0, rect.center().y), vec2(18.0, 18.0)), icon, t.text);
    let text = p.with_clip_rect(rect.intersect(p.clip_rect()));
    if detail.is_empty() {
        text.text(pos2(rect.left() + 38.0, rect.center().y), Align2::LEFT_CENTER, name, theme::ui_font(13.5), t.text);
    } else {
        text.text(pos2(rect.left() + 38.0, rect.center().y - 8.0), Align2::LEFT_CENTER, name, theme::ui_font(13.5), t.text);
        text.text(pos2(rect.left() + 38.0, rect.center().y + 9.0), Align2::LEFT_CENTER, detail, theme::ui_font(11.5), t.text_dim);
    }
    resp
}

/// The Open page (`save` false) or the Save As page's browser (`save` true, writing `fmt`).
pub fn show(app: &mut SheetApp, ui: &mut Ui, state: &mut State, fmt: &str, out: &mut Outcome, save: bool) {
    let lang = app.ui.language;
    let Some(folders) = app.services.folders.as_ref() else { return };
    let places = folders.places();
    let recent = folders.recent();
    if state.trail.is_empty()
        && let Some(start) = recent.first().or(places.first())
    {
        state.go(vec![(start.id.clone(), start.name.clone())]);
    }
    if state.listing.is_none()
        && let Some((id, _)) = state.trail.last()
    {
        state.listing = Some(folders.list(id).map(|mut items| {
            items.retain(shown);
            items.sort_by(|a, b| b.folder.cmp(&a.folder).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
            items
        }));
    }

    if save {
        save_bar(app, ui, state, fmt, out);
        ui.add_space(16.0);
    }

    let mut add_place = false;
    let mut go = None;
    let mut picked = None;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(SIDE_W);
            section(ui, lang, "Locations");
            let at = state.trail.first().map(|(id, _)| id.as_str());
            for p in &places {
                if row(ui, Icon::Folder, &p.name, "", at == Some(p.id.as_str())).clicked() {
                    go = Some(p.clone());
                }
            }
            if row(ui, Icon::Plus, lang.tr("Add a location…"), "", false).clicked() {
                add_place = true;
            }
            if !recent.is_empty() {
                ui.add_space(14.0);
                section(ui, lang, "Recent folders");
                for r in &recent {
                    if row(ui, Icon::Clock, &r.name, &r.detail, false).clicked() {
                        go = Some(r.clone());
                    }
                }
            }
        });
        ui.add_space(24.0);
        ui.vertical(|ui| {
            ui.set_width((CONTENT_W - SIDE_W - 24.0).max(200.0));
            if state.trail.is_empty() {
                dim(ui, lang.tr("Choose Add a location… and pick a folder GridCraft may use, such as My files."));
                return;
            }
            // Where we are: each folder on the way down can be clicked to go back up to it.
            let mut up = None;
            ui.horizontal_wrapped(|ui| {
                let last = state.trail.len() - 1;
                for (i, (_, name)) in state.trail.iter().enumerate() {
                    if i > 0 {
                        ui.label("›");
                    }
                    if i == last {
                        ui.label(egui::RichText::new(name.as_str()).strong());
                    } else if ui.link(name.as_str()).clicked() {
                        up = Some(i);
                    }
                }
            });
            ui.add_space(8.0);
            if let Some(i) = up {
                let trail = state.trail[..=i].to_vec();
                state.go(trail);
                return;
            }
            match &state.listing {
                Some(Ok(items)) if items.is_empty() => dim(ui, lang.tr("This folder has no spreadsheets.")),
                Some(Ok(items)) => {
                    for item in items {
                        let icon = if item.folder { Icon::Folder } else { Icon::Book };
                        let selected = !item.folder && state.name.as_deref() == Some(item.name.as_str());
                        if row(ui, icon, &item.name, "", selected).clicked() {
                            picked = Some(item.clone());
                        }
                    }
                }
                Some(Err(e)) => dim(ui, e),
                None => {}
            }
        });
    });

    if add_place && let Some(f) = app.services.folders.as_ref() {
        f.add_place();
    }
    if let Some(place) = go {
        state.go(vec![(place.id, place.name)]);
    }
    let Some(item) = picked else { return };
    if item.folder {
        let mut trail = state.trail.clone();
        trail.push((item.id, item.name));
        state.go(trail);
    } else if save {
        // A file clicked on Save As gives its name, to save over it.
        state.name = Some(item.name);
        state.replace = None;
    } else {
        let (folder, folder_name) = state.trail.last().cloned().unwrap_or_default();
        let opened = app.services.folders.as_ref().map(|f| f.open(&folder, &folder_name, &item));
        match opened {
            Some(Ok(path)) => {
                app.open_path(&path);
                if app.session.active().and_then(|d| d.path.as_deref()) == Some(path.as_str()) {
                    out.close = true;
                }
            }
            Some(Err(e)) => app.message = Some(("GridCraft".into(), format!("{}: {e}", item.name))),
            None => {}
        }
    }
}

/// Save As: the name box and the Save button (and the replace question).
fn save_bar(app: &mut SheetApp, ui: &mut Ui, state: &mut State, fmt: &str, out: &mut Outcome) {
    let lang = app.ui.language;
    let stem = {
        let title = app.session.active().map(|d| d.display_title()).unwrap_or_else(|| "Book1".into());
        std::path::Path::new(&title).file_stem().and_then(std::ffi::OsStr::to_str).unwrap_or("Book1").to_string()
    };
    let name = state.name.get_or_insert_with(|| format!("{stem}.{fmt}"));
    // The name follows the chosen type.
    let ext_ok = std::path::Path::new(name.as_str()).extension().and_then(std::ffi::OsStr::to_str).is_some_and(|e| e.eq_ignore_ascii_case(fmt));
    if !ext_ok && !name.trim().is_empty() {
        let base = std::path::Path::new(name.as_str()).file_stem().and_then(std::ffi::OsStr::to_str).unwrap_or("Book1").to_string();
        *name = format!("{base}.{fmt}");
    }
    let mut clicked = false;
    ui.horizontal(|ui| {
        let label = ui.label(lang.tr("File name"));
        let resp = ui.add(egui::TextEdit::singleline(name).desired_width(360.0)).labelled_by(label.id);
        if resp.changed() {
            state.replace = None;
        }
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let ready = !state.trail.is_empty() && !name.trim().is_empty();
        clicked = ui.add_enabled(ready, egui::Button::new(lang.tr("Save"))).clicked() || (enter && ready);
    });
    let name = name.trim().to_string();
    let existing = match &state.listing {
        Some(Ok(items)) => items.iter().find(|i| !i.folder && i.name == name).cloned(),
        _ => None,
    };
    if let Some(e) = &existing
        && state.replace.as_deref() == Some(e.id.as_str())
    {
        ui.add_space(6.0);
        let mut confirmed = false;
        ui.horizontal(|ui| {
            dim(ui, &format!("{name} {}", lang.tr("already exists. Replace it?")));
            confirmed = ui.button(lang.tr("Replace")).clicked();
        });
        if !confirmed {
            return;
        }
    } else if clicked {
        if let Some(e) = &existing {
            state.replace = Some(e.id.clone());
            return;
        }
    } else {
        return;
    }
    let Some((folder, folder_name)) = state.trail.last().cloned() else { return };
    let target = app.services.folders.as_ref().map(|f| f.save(&folder, &folder_name, &name, existing.as_ref()));
    let path = match target {
        Some(Ok(p)) => p,
        Some(Err(e)) => {
            app.message = Some(("GridCraft".into(), format!("{name}: {e}")));
            return;
        }
        None => return,
    };
    let id = match fmt {
        "pdf" => "file.exportPdf",
        "csv" => "file.exportCsv",
        "html" => "file.exportHtml",
        _ => "file.save",
    };
    match app.run(id, json!({"path": path})) {
        Ok(_) => {
            app.after_engine();
            out.close = true;
        }
        Err(e) => app.message = Some(("GridCraft".into(), crate::clean_error(&e))),
    }
}
