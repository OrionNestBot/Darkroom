//! Shared dialog building blocks: modal window (header, body, footer button row), cards, label rows, segmented control, primary/secondary buttons, side navigation.
//! Keeps spacing, alignment and colors consistent across all settings and export dialogs.

use super::theme::*;
use egui::{Align2, Color32, CornerRadius, FontId, Margin, Rect, Sense, Stroke, Ui, pos2, vec2};

/// Label column width (aligns where controls start on every row)
pub const LABEL_W: f32 = 118.0;

/// Modal dialog. `body` is a scroll area, `footer` a right-aligned button row.
/// Returns (body result, footer result, close requested via Esc, outside click or the × button)
pub fn modal<B, F>(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    subtitle: &str,
    size: egui::Vec2,
    body: impl FnOnce(&mut Ui) -> B,
    footer: impl FnOnce(&mut Ui) -> F,
) -> (B, F, bool) {
    let screen = ctx.content_rect();
    let w = size.x.min(screen.width() - 40.0);
    let h = size.y.min(screen.height() - 40.0);
    let mut close = false;
    let resp = egui::Modal::new(egui::Id::new(("modal", id)))
        .backdrop_color(Color32::from_black_alpha(if current_theme().is_light() { 70 } else { 140 }))
        .frame(egui::Frame::new().fill(PANEL2()).stroke(Stroke::new(1.0, BORDER())).corner_radius(CornerRadius::same(10)).inner_margin(Margin::ZERO))
        .show(ctx, |ui| {
            ui.set_width(w);
            ui.set_max_height(h);
            // Header
            let (hr, _) = ui.allocate_exact_size(vec2(w, if subtitle.is_empty() { 46.0 } else { 56.0 }), Sense::hover());
            let p = ui.painter();
            let ty = if subtitle.is_empty() { hr.center().y } else { hr.top() + 20.0 };
            p.text(pos2(hr.left() + 20.0, ty), Align2::LEFT_CENTER, title, FontId::proportional(15.5), STRONG());
            if !subtitle.is_empty() {
                p.text(pos2(hr.left() + 20.0, hr.top() + 39.0), Align2::LEFT_CENTER, subtitle, FontId::proportional(11.5), TEXT_WEAK());
            }
            let xr = Rect::from_center_size(pos2(hr.right() - 26.0, hr.top() + 23.0), vec2(26.0, 26.0));
            let xresp = ui.interact(xr, egui::Id::new(("modal_x", id)), Sense::click()).on_hover_text(tr!("닫기 (Esc)", "Close (Esc)"));
            if xresp.hovered() {
                ui.painter().rect_filled(xr, 6.0, WIDGET());
            }
            ui.painter().text(xr.center(), Align2::CENTER_CENTER, "×", FontId::proportional(13.0), if xresp.hovered() { STRONG() } else { TEXT_WEAK() });
            if xresp.clicked() {
                close = true;
            }
            hline(ui, w);
            // Body (leaving room for the footer)
            let body_h = (h - hr.height() - 58.0).max(120.0);
            let b = egui::Frame::new().inner_margin(Margin { left: 20, right: 20, top: 14, bottom: 10 }).show(ui, |ui| {
                ui.set_width(w - 40.0);
                ui.set_max_height(body_h - 24.0);
                body(ui)
            });
            // Self-test: report body content wider than the dialog (it would break the header and footer lines)
            if std::env::var_os("DARKROOM_AUTOTEST").is_some() && b.response.rect.width() > w + 1.0 {
                eprintln!("[layout] modal \"{id}\" body {:.0}px wider than {w:.0}px", b.response.rect.width());
            }
            hline(ui, w);
            // Footer button row
            let f = egui::Frame::new().fill(PANEL()).corner_radius(CornerRadius { nw: 0, ne: 0, sw: 10, se: 10 }).inner_margin(Margin::symmetric(20, 12)).show(ui, |ui| {
                ui.set_width(w - 40.0);
                ui.horizontal(|ui| footer(ui)).inner
            });
            (b.inner, f.inner)
        });
    if resp.should_close() {
        close = true;
    }
    let (b, f) = resp.inner;
    (b, f, close)
}

fn hline(ui: &mut Ui, w: f32) {
    let (r, _) = ui.allocate_exact_size(vec2(w, 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, BORDER());
}

/// Group card: title (+ description) above the content, visually grouping related items.
pub fn card<R>(ui: &mut Ui, title: &str, desc: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let r = egui::Frame::new()
        .fill(PANEL())
        .stroke(Stroke::new(1.0, BORDER()))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.label(egui::RichText::new(title).size(13.0).color(STRONG()));
                if !desc.is_empty() {
                    ui.label(egui::RichText::new(desc).size(11.0).color(TEXT_WEAK()));
                }
                ui.add_space(6.0);
            }
            ui.spacing_mut().item_spacing.y = 7.0;
            add(ui)
        })
        .inner;
    ui.add_space(10.0);
    r
}

/// Label + control on one row (fixed label column)
pub fn row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(LABEL_W, 22.0), Sense::hover());
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(12.5), TEXT_WEAK());
        add(ui)
    })
    .inner
}

/// Help text aligned to the control column
pub fn hint(ui: &mut Ui, text: &str) {
    ui.horizontal_top(|ui| {
        ui.add_space(LABEL_W + ui.spacing().item_spacing.x);
        // Wrap within the remaining width (a horizontal layout has no width limit otherwise)
        let w = ui.available_width();
        ui.allocate_ui(egui::vec2(w, 0.0), |ui| {
            ui.set_max_width(w);
            ui.add(egui::Label::new(egui::RichText::new(text).size(11.0).color(TEXT_DIM())).wrap());
        });
    });
}

/// Switch row: name in the label column, switch + description on the right
pub fn switch_row(ui: &mut Ui, label: &str, on: &mut bool, desc: &str) -> bool {
    ui.horizontal_top(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(LABEL_W, 22.0), Sense::hover());
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(12.5), TEXT_WEAK());
        let ch = super::widgets::toggle(ui, on).changed();
        if !desc.is_empty() {
            // Wrap within the remaining width so a long description never widens the dialog
            let w = ui.available_width();
            ui.allocate_ui(vec2(w, 0.0), |ui| {
                ui.set_max_width(w);
                ui.add_space(3.0);
                ui.add(egui::Label::new(egui::RichText::new(desc).size(11.5).color(if *on { TEXT() } else { TEXT_WEAK() })).wrap());
            });
        }
        ch
    })
    .inner
}

/// Segmented control (joined buttons). Returns true when changed
pub fn seg<T: PartialEq + Copy>(ui: &mut Ui, v: &mut T, opts: &[(T, &str)]) -> bool {
    let mut changed = false;
    let font = FontId::proportional(12.0);
    let widths: Vec<f32> = opts.iter().map(|(_, n)| ui.fonts_mut(|f| f.layout_no_wrap(n.to_string(), font.clone(), Color32::WHITE).size().x) + 22.0).collect();
    let total: f32 = widths.iter().sum::<f32>() + 4.0;
    let (rect, _) = ui.allocate_exact_size(vec2(total, 24.0), Sense::hover());
    ui.painter().rect_filled(rect, 6.0, WIDGET());
    let mut x = rect.left() + 2.0;
    for (i, (val, name)) in opts.iter().enumerate() {
        let r = Rect::from_min_size(pos2(x, rect.top() + 2.0), vec2(widths[i], rect.height() - 4.0));
        let resp = ui.interact(r, ui.id().with(("seg", name, i)), Sense::click());
        let sel = *v == *val;
        // Selected = inverted brightness (same rule as tool panel toggles)
        if sel {
            ui.painter().rect_filled(r, 5.0, STRONG());
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 5.0, HOVER());
        }
        ui.painter().text(r.center(), Align2::CENTER_CENTER, *name, if sel { FontId::new(12.0, egui::FontFamily::Proportional) } else { font.clone() }, if sel { PANEL() } else { TEXT_WEAK() });
        if resp.clicked() && !sel {
            *v = *val;
            changed = true;
        }
        x += widths[i];
    }
    changed
}

/// Primary button (accent color); gray when disabled
pub fn primary(ui: &mut Ui, text: &str, enabled: bool) -> egui::Response {
    let b = egui::Button::new(egui::RichText::new(text).size(12.5).strong().color(if enabled { Color32::WHITE } else { TEXT_DIM() }))
        .fill(if enabled { ACCENT } else { WIDGET() })
        .corner_radius(6.0)
        .min_size(vec2(96.0, 30.0));
    ui.add_enabled(enabled, b)
}

/// Secondary button
pub fn secondary(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(egui::RichText::new(text).size(12.5)).corner_radius(6.0).min_size(vec2(72.0, 30.0)))
}

/// Small secondary button (inside cards)
pub fn small(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(egui::RichText::new(text).size(12.0)).corner_radius(5.0).min_size(vec2(0.0, 24.0)))
}

/// Side navigation list (settings window etc.). Items: (name, description). Returns true when changed
pub fn nav(ui: &mut Ui, items: &[(&str, &str)], sel: &mut usize) -> bool {
    let mut changed = false;
    let w = ui.available_width();
    for (i, (name, desc)) in items.iter().enumerate() {
        let (r, resp) = ui.allocate_exact_size(vec2(w, if desc.is_empty() { 30.0 } else { 40.0 }), Sense::click());
        let on = *sel == i;
        if on {
            ui.painter().rect_filled(r, 6.0, WIDGET());
            ui.painter().rect_filled(Rect::from_min_size(r.left_top() + vec2(0.0, 8.0), vec2(3.0, r.height() - 16.0)), 1.5, ACCENT);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 6.0, HOVER());
        }
        let ty = if desc.is_empty() { r.center().y } else { r.top() + 14.0 };
        ui.painter().text(pos2(r.left() + 14.0, ty), Align2::LEFT_CENTER, *name, FontId::proportional(12.5), if on { STRONG() } else { TEXT() });
        if !desc.is_empty() {
            ui.painter().text(pos2(r.left() + 14.0, r.top() + 29.0), Align2::LEFT_CENTER, *desc, FontId::proportional(10.5), TEXT_DIM());
        }
        if resp.clicked() && !on {
            *sel = i;
            changed = true;
        }
    }
    changed
}

/// Folder path field + browse button
pub fn folder_row(ui: &mut Ui, label: &str, path: &mut String) -> bool {
    row(ui, label, |ui| {
        let w = (ui.available_width() - 84.0).max(120.0);
        let mut ch = ui.add(egui::TextEdit::singleline(path).desired_width(w)).changed();
        if small(ui, tr!("찾아보기…", "Browse…")).clicked() {
            let mut d = rfd::FileDialog::new();
            if std::path::Path::new(path.as_str()).is_dir() {
                d = d.set_directory(path.as_str());
            }
            if let Some(f) = d.pick_folder() {
                *path = f.to_string_lossy().to_string();
                ch = true;
            }
        }
        ch
    })
}

/// List row (watermarks, presets, etc.): title/subtitle + buttons on the right. Returns (row clicked, right-side result)
pub fn list_item<R>(ui: &mut Ui, title: &str, sub: &str, selected: bool, right: impl FnOnce(&mut Ui) -> R) -> (bool, R) {
    let w = ui.available_width();
    let h = if sub.is_empty() { 34.0 } else { 44.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 6.0, WIDGET());
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 6.0, HOVER());
    }
    let ty = if sub.is_empty() { rect.center().y } else { rect.top() + 15.0 };
    ui.painter().text(pos2(rect.left() + 12.0, ty), Align2::LEFT_CENTER, title, FontId::proportional(12.5), if selected { STRONG() } else { TEXT() });
    if !sub.is_empty() {
        ui.painter().text(pos2(rect.left() + 12.0, rect.top() + 31.0), Align2::LEFT_CENTER, sub, FontId::proportional(10.5), TEXT_DIM());
    }
    // Child UI that doesn't move the parent cursor (keeps the row height)
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 0.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
    let inner = right(&mut child);
    (resp.clicked(), inner)
}

/// Summary text on the left of the footer
pub fn footer_note(ui: &mut Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(11.5).color(TEXT_WEAK()));
}
