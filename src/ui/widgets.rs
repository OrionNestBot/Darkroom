//! Widgets specific to photo editing.

use super::theme::*;
use crate::develop::color::{hsv_to_rgb, monotone_spline};
use crate::develop::pipeline::Histogram;
use crate::develop::settings::Wheel;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};

#[derive(Default, Clone, Copy)]
pub struct Edit {
    pub changed: bool,
    /// Currently dragging (used to decide on draft rendering).
    pub active: bool,
    /// Editing finished (when the history step is committed).
    pub committed: bool,
}

impl Edit {
    pub fn merge(&mut self, o: Edit) {
        self.changed |= o.changed;
        self.active |= o.active;
        self.committed |= o.committed;
    }
}

/// Fixed-width value box: draws its own text so it never overflows the layout (keeps panels from widening).
/// Drag left/right = change value, click = type a value, Enter/focus loss = commit.
#[allow(clippy::too_many_arguments)]
pub fn value_box(ui: &mut Ui, size: Vec2, v: &mut f32, min: f32, max: f32, speed: f32, decimals: usize, signed: bool, suffix: &str, id: &str) -> Edit {
    let mut e = Edit::default();
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let eid = ui.id().with(("vbox", id));
    let editing: Option<String> = ui.data(|d| d.get_temp(eid));
    let fmt = |n: f32| {
        let s = format!("{:.*}", decimals, n);
        let s = if signed && n > 0.0 { format!("+{s}") } else { s };
        format!("{s}{suffix}")
    };
    if let Some(mut text) = editing {
        let te = egui::TextEdit::singleline(&mut text)
            .id(eid.with("te"))
            .font(FontId::monospace(11.0))
            .horizontal_align(egui::Align::Max)
            .margin(egui::Margin::symmetric(2, 1))
            .desired_width(rect.width() - 4.0);
        let r = ui.put(rect, te);
        r.request_focus();
        let done = r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter));
        if done {
            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                let t = text.trim().trim_end_matches(suffix.trim()).trim().replace(',', ".");
                if let Ok(n) = t.parse::<f32>() {
                    let n = n.clamp(min, max);
                    if n != *v {
                        *v = n;
                        e.changed = true;
                    }
                }
            }
            e.committed = e.changed;
            ui.data_mut(|d| d.remove::<String>(eid));
        } else {
            ui.data_mut(|d| d.insert_temp(eid, text));
        }
        return e;
    }
    let hot = resp.hovered() || resp.dragged();
    if hot {
        ui.painter().rect_filled(rect, 3.0, WIDGET());
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    ui.painter().with_clip_rect(rect).text(rect.right_center() - vec2(3.0, 0.0), Align2::RIGHT_CENTER, fmt(*v), FontId::monospace(11.0), if hot { STRONG() } else { TEXT() });
    if resp.dragged() {
        let fine = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
        let acc: f32 = ui.data(|d| d.get_temp(eid.with("acc"))).unwrap_or(*v);
        let nv = (acc + resp.drag_delta().x * speed * fine).clamp(min, max);
        ui.data_mut(|d| d.insert_temp(eid.with("acc"), nv));
        let step = 10f32.powi(-(decimals as i32));
        let q = (nv / step).round() * step;
        if q != *v {
            *v = q;
            e.changed = true;
        }
        e.active = true;
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<f32>(eid.with("acc")));
        e.committed = true;
    }
    if resp.clicked() {
        ui.data_mut(|d| d.insert_temp(eid, format!("{:.*}", decimals, *v)));
    }
    e
}

/// Shared slider dimensions so every slider, temperature and quick-develop row lines up in the same columns.
pub const SLIDER_LABEL_W: f32 = 86.0;
pub const SLIDER_VALUE_W: f32 = 54.0;

/// Draws a label that fits its column, shrinking the font if needed (minimum 9.5).
pub fn fit_label(ui: &Ui, r: Rect, text: &str, col: Color32) {
    let p = ui.painter();
    let mut size = 12.0;
    loop {
        let g = p.layout_no_wrap(text.to_string(), FontId::proportional(size), col);
        if g.size().x <= r.width() - 6.0 || size <= 9.5 {
            p.galley(pos2(r.left(), r.center().y - g.size().y * 0.5), g, col);
            break;
        }
        size -= 0.5;
    }
}

/// Crop aspect tile: a rectangle with the real aspect on top (dashed for free), text below. Selected = inverted brightness.
pub fn ratio_tile(ui: &mut Ui, w: f32, h: f32, aspect: Option<f32>, label: &str, selected: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let p = ui.painter();
    let (bg, fg) = if selected { (STRONG(), PANEL()) } else if resp.hovered() { (HOVER(), TEXT()) } else { (WIDGET(), TEXT()) };
    p.rect_filled(r, 5.0, bg);
    // Icon area (top 60%)
    let icon_box = Rect::from_min_max(r.min + vec2(6.0, 6.0), pos2(r.max.x - 6.0, r.top() + h * 0.62));
    let a = aspect.unwrap_or(1.3);
    let (bw, bh) = (icon_box.width(), icon_box.height());
    let (iw, ih) = if a >= bw / bh { (bw, bw / a) } else { (bh * a, bh) };
    let ir = Rect::from_center_size(icon_box.center(), vec2(iw.max(4.0), ih.max(4.0)));
    if aspect.is_some() {
        p.rect_stroke(ir, 1.0, Stroke::new(1.4, fg), egui::StrokeKind::Inside);
    } else {
        // Free: dashed rectangle
        let pts = [ir.left_top(), ir.right_top(), ir.right_bottom(), ir.left_bottom(), ir.left_top()];
        for s in pts.windows(2) {
            let (a0, b0) = (s[0], s[1]);
            let len = (b0 - a0).length();
            let n = (len / 4.0).floor() as i32;
            for k in (0..n).step_by(2) {
                let t0 = k as f32 / n as f32;
                let t1 = ((k + 1) as f32 / n as f32).min(1.0);
                p.line_segment([a0 + (b0 - a0) * t0, a0 + (b0 - a0) * t1], Stroke::new(1.3, fg));
            }
        }
    }
    p.text(pos2(r.center().x, r.bottom() - h * 0.2), Align2::CENTER_CENTER, label, FontId::proportional(11.5), fg);
    resp
}

/// Result of the tool panel bottom action bar.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Act {
    None,
    Secondary(usize),
    Primary,
}

/// Tool panel footer: separator + [secondary buttons ...][primary button (accent)], same place and look in every tool.
pub fn action_bar(ui: &mut Ui, secondary: &[(&str, bool, &str)], primary: (&str, bool, &str)) -> Act {
    ui.add_space(8.0);
    let y = ui.cursor().top();
    let (x0, x1) = (ui.max_rect().left(), ui.max_rect().right());
    ui.painter().line_segment([pos2(x0, y), pos2(x1, y)], Stroke::new(1.0, BORDER()));
    ui.add_space(8.0);
    let gap = 6.0;
    let n = secondary.len() as f32;
    let full = ui.available_width();
    // Primary button is wider than secondary ones (full width if there are none).
    let pw = if secondary.is_empty() { full } else { (full * 0.42).max(110.0).min(full) };
    let sw = if secondary.is_empty() { 0.0 } else { ((full - pw - gap * n) / n).floor() };
    let mut act = Act::None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, (label, enabled, tip)) in secondary.iter().enumerate() {
            let mut r = ui.add_enabled(*enabled, egui::Button::new(egui::RichText::new(*label).size(12.0)).min_size(vec2(sw, 28.0)).corner_radius(6.0));
            if !tip.is_empty() {
                r = r.on_hover_text(*tip);
            }
            if r.clicked() {
                act = Act::Secondary(i);
            }
        }
        let (label, enabled, tip) = primary;
        let b = egui::Button::new(egui::RichText::new(label).size(12.5).strong().color(Color32::WHITE)).fill(ACCENT).min_size(vec2(pw, 28.0)).corner_radius(6.0);
        let mut r = ui.add_enabled(enabled, b);
        if !tip.is_empty() {
            r = r.on_hover_text(tip);
        }
        if r.clicked() {
            act = Act::Primary;
        }
    });
    act
}

/// '?' help button: opens a popup next to the button (closes on a second click or a click outside).
pub fn help_box(ui: &mut Ui, id: &str, rows: &[(&str, &str)]) {
    let pid = ui.make_persistent_id(("helpbtn", id));
    let mut open = ui.data(|d| d.get_temp::<bool>(pid)).unwrap_or(false);
    ui.add_space(2.0);
    // Take only one row of height (so right-aligned layout does not consume the remaining height).
    let resp = ui
        .allocate_ui_with_layout(vec2(ui.available_width(), 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(egui::Button::new(egui::RichText::new(tr!("?  조작 방법", "?  Controls")).size(11.0)).selected(open).min_size(vec2(0.0, 20.0)))
        })
        .inner;
    let clicked = resp.clicked();
    if clicked {
        open = !open;
    }
    if open {
        let w = 270.0;
        let pos = pos2((resp.rect.right() - w).max(4.0), resp.rect.bottom() + 4.0);
        let area = egui::Area::new(pid.with("pop")).order(egui::Order::Foreground).fixed_pos(pos).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::same(10)).show(ui, |ui| {
                ui.set_width(w - 20.0);
                ui.label(egui::RichText::new(tr!("조작 방법", "Controls")).size(12.0).strong().color(STRONG()));
                ui.add_space(4.0);
                egui::Grid::new(pid.with("g")).num_columns(2).spacing(vec2(12.0, 4.0)).show(ui, |ui| {
                    for (k, v) in rows {
                        ui.label(egui::RichText::new(*k).size(11.5).color(TEXT()));
                        ui.add(egui::Label::new(egui::RichText::new(*v).size(11.5).color(TEXT_WEAK())).wrap());
                        ui.end_row();
                    }
                });
            });
        });
        // Close on click outside
        if !clicked && ui.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer() && !resp.contains_pointer() {
            open = false;
        }
    }
    ui.data_mut(|d| d.insert_temp(pid, open));
}

/// Icon tile (icon glyph on top, name below). Selected = inverted brightness.
pub fn icon_tile(ui: &mut Ui, w: f32, h: f32, icon: &str, label: &str, selected: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    let p = ui.painter();
    let (bg, fg) = if selected { (STRONG(), PANEL()) } else if resp.hovered() { (HOVER(), TEXT()) } else { (WIDGET(), TEXT()) };
    p.rect_filled(r, 5.0, bg);
    p.text(pos2(r.center().x, r.top() + h * 0.36), Align2::CENTER_CENTER, icon, FontId::proportional(14.0), fg);
    p.text(pos2(r.center().x, r.bottom() - h * 0.22), Align2::CENTER_CENTER, label, FontId::proportional(10.5), fg);
    resp
}

/// Crop row [level][rotate 90° CCW][rotate 90° CW][flip horizontal][flip vertical], with hand-drawn icons. Returns the clicked index.
pub fn geo_button_row(ui: &mut Ui) -> Option<usize> {
    let gap = 4.0;
    let w = tile_w(ui, 5, gap);
    let tips = [tr!("자동 수평: 사진 속 수평·수직선을 찾아 기울기를 바로잡습니다", "Auto straighten: finds horizontal·vertical lines in the photo and fixes the tilt"), tr!("왼쪽으로 90° 회전 (반시계)", "Rotate 90° left (counterclockwise)"), tr!("오른쪽으로 90° 회전 (시계)", "Rotate 90° right (clockwise)"), tr!("좌우 반전", "Flip horizontal"), tr!("상하 반전", "Flip vertical")];
    let mut hit = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, tip) in tips.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
            let p = ui.painter();
            p.rect_filled(r, 4.0, if resp.hovered() { HOVER() } else { WIDGET() });
            let col = TEXT();
            let c = r.center();
            match i {
                0 => {
                    p.text(c, Align2::CENTER_CENTER, tr!("수평", "Horizontal"), FontId::proportional(11.5), col);
                }
                1 | 2 => {
                    // 3/4 arc plus arrowhead: CCW (1) points left at the top end, CW (2) points right.
                    let cw = i == 2;
                    let ic = c - vec2(9.0, 0.0);
                    let rr = 5.5;
                    let n = 16;
                    // Screen angles (down is +): start at -90° (top), sweep 270° in + for CW, - for CCW.
                    let pts: Vec<Pos2> = (0..=n)
                        .map(|k| {
                            let t = (-90.0f32 + if cw { -1.0 } else { 1.0 } * 270.0 * (1.0 - k as f32 / n as f32)).to_radians();
                            ic + vec2(t.cos(), t.sin()) * rr
                        })
                        .collect();
                    p.add(egui::Shape::line(pts.clone(), Stroke::new(1.5, col)));
                    // Arrowhead at the end (top) in the direction of travel
                    let end = pts[n];
                    let dir = (pts[n] - pts[n - 1]).normalized();
                    let nrm = vec2(-dir.y, dir.x);
                    p.add(egui::Shape::convex_polygon(vec![end + dir * 3.2, end - dir * 1.0 + nrm * 3.0, end - dir * 1.0 - nrm * 3.0], col, Stroke::NONE));
                    p.text(c + vec2(6.0, 0.0), Align2::CENTER_CENTER, "90°", FontId::proportional(11.0), col);
                }
                _ => {
                    // Flip: dashed center axis + a triangle on each side (one filled, one outlined)
                    let horiz = i == 3;
                    let s = 6.0;
                    if horiz {
                        p.line_segment([c - vec2(0.0, s + 1.0), c + vec2(0.0, s + 1.0)], Stroke::new(1.0, TEXT_DIM()));
                        p.add(egui::Shape::convex_polygon(vec![c + vec2(-2.0, -s), c + vec2(-2.0, s), c + vec2(-2.0 - s, s)], col, Stroke::NONE));
                        p.add(egui::Shape::closed_line(vec![c + vec2(2.0, -s), c + vec2(2.0, s), c + vec2(2.0 + s, s)], Stroke::new(1.2, col)));
                    } else {
                        p.line_segment([c - vec2(s + 1.0, 0.0), c + vec2(s + 1.0, 0.0)], Stroke::new(1.0, TEXT_DIM()));
                        p.add(egui::Shape::convex_polygon(vec![c + vec2(-s, -2.0), c + vec2(s, -2.0), c + vec2(s, -2.0 - s)], col, Stroke::NONE));
                        p.add(egui::Shape::closed_line(vec![c + vec2(-s, 2.0), c + vec2(s, 2.0), c + vec2(s, 2.0 + s)], Stroke::new(1.2, col)));
                    }
                }
            }
            if resp.on_hover_text(*tip).clicked() {
                hit = Some(i);
            }
        }
    });
    hit
}

/// Tile width when placing n equal-width tiles in one row.
pub fn tile_w(ui: &Ui, n: usize, gap: f32) -> f32 {
    ((ui.available_width() - gap * (n.saturating_sub(1)) as f32) / n.max(1) as f32).floor()
}

/// Label column + switch row (aligned with slider labels).
pub fn toggle_line(ui: &mut Ui, label: &str, on: &mut bool, tip: &str) -> bool {
    let mut ch = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let (lr, _) = ui.allocate_exact_size(vec2(SLIDER_LABEL_W, 20.0), Sense::hover());
        fit_label(ui, lr, label, if *on { TEXT() } else { TEXT_WEAK() });
        let r = toggle(ui, on);
        ch = r.changed();
        if !tip.is_empty() {
            r.on_hover_text(tip);
        }
    });
    ch
}

/// Row of equal-width toggles (selected one highlighted). Returns the clicked index.
pub fn toggle_row(ui: &mut Ui, items: &[&str], selected: Option<usize>, tip: &str) -> Option<usize> {
    toggle_row_ex(ui, items, selected, tip)
}

fn toggle_row_ex(ui: &mut Ui, items: &[&str], selected: Option<usize>, tip: &str) -> Option<usize> {
    let gap = 4.0;
    let n = items.len().max(1) as f32;
    let w = ((ui.available_width() - gap * (n - 1.0)) / n).floor();
    let mut hit = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, label) in items.iter().enumerate() {
            if label.is_empty() {
                ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
                continue;
            }
            let on = selected == Some(i);
            // Selected = inverted brightness so selected/unselected are easy to tell apart
            let b = egui::Button::new(egui::RichText::new(*label).size(12.0).color(if on { PANEL() } else { TEXT() }).strong()).min_size(vec2(w, 22.0)).fill(if on { STRONG() } else { WIDGET() });
            let mut r = ui.add(b);
            if !tip.is_empty() {
                r = r.on_hover_text(tip);
            }
            if r.clicked() {
                hit = Some(i);
            }
        }
    });
    hit
}

/// Row of buttons splitting the panel width equally. (label, enabled, tooltip) -> clicked index.
pub fn button_row(ui: &mut Ui, items: &[(&str, bool, &str)]) -> Option<usize> {
    let gap = 4.0;
    let n = items.len().max(1) as f32;
    let w = ((ui.available_width() - gap * (n - 1.0)) / n).floor();
    let mut hit = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, (label, enabled, tip)) in items.iter().enumerate() {
            let mut r = ui.add_enabled(*enabled, egui::Button::new(egui::RichText::new(*label).size(12.0)).min_size(vec2(w, 22.0)));
            if !tip.is_empty() {
                r = r.on_hover_text(*tip);
            }
            if r.clicked() {
                hit = Some(i);
            }
        }
    });
    hit
}

/// Slider: [label][track][value]. Double-click label = reset to default, drag/type in the value box, Shift = fine adjust.
#[allow(clippy::too_many_arguments)]
pub fn slider(ui: &mut Ui, label: &str, v: &mut f32, min: f32, max: f32, default: f32, decimals: usize, id: &str) -> Edit {
    slider_impl(ui, label, v, min, max, default, decimals, id, None, 1.0)
}

/// Slider for mostly small values (brush size, flow, ...): track position t -> value = min + range * t^gamma.
/// Larger gamma gives more track to small values (at 2.5, 30% of the range takes 62% of the track).
#[allow(clippy::too_many_arguments)]
pub fn slider_pow(ui: &mut Ui, label: &str, v: &mut f32, min: f32, max: f32, default: f32, decimals: usize, id: &str, gamma: f32) -> Edit {
    slider_impl(ui, label, v, min, max, default, decimals, id, None, gamma)
}

/// Slider with a color gradient track (temperature, tint, HSL, ...) so it shows what the value changes.
#[allow(clippy::too_many_arguments)]
pub fn slider_grad(ui: &mut Ui, label: &str, v: &mut f32, min: f32, max: f32, default: f32, decimals: usize, id: &str, grad: &[Color32]) -> Edit {
    slider_impl(ui, label, v, min, max, default, decimals, id, Some(grad), 1.0)
}

/// Horizontal gradient strip.
pub fn grad_bar(p: &egui::Painter, r: Rect, cols: &[Color32]) {
    if cols.len() < 2 {
        return;
    }
    let mut mesh = egui::Mesh::default();
    let n = cols.len() - 1;
    for (i, c) in cols.iter().enumerate() {
        let x = r.left() + i as f32 / n as f32 * r.width();
        mesh.colored_vertex(pos2(x, r.top()), *c);
        mesh.colored_vertex(pos2(x, r.bottom()), *c);
        if i > 0 {
            let b = mesh.vertices.len() as u32;
            mesh.add_triangle(b - 4, b - 3, b - 2);
            mesh.add_triangle(b - 3, b - 2, b - 1);
        }
    }
    p.add(egui::Shape::mesh(mesh));
}

#[allow(clippy::too_many_arguments)]
fn slider_impl(ui: &mut Ui, label: &str, v: &mut f32, min: f32, max: f32, default: f32, decimals: usize, id: &str, grad: Option<&[Color32]>, gamma: f32) -> Edit {
    let mut e = Edit::default();
    let full_w = ui.available_width();
    let label_w = SLIDER_LABEL_W;
    let value_w = SLIDER_VALUE_W;
    let h = 20.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let (lrect, lresp) = ui.allocate_exact_size(vec2(label_w, h), Sense::click());
        let lcol = if (*v - default).abs() > 1e-6 { TEXT() } else { TEXT_WEAK() };
        fit_label(ui, lrect, label, lcol);
        if lresp.double_clicked() {
            *v = default;
            e.changed = true;
            e.committed = true;
        }
        lresp.on_hover_text(tr!("더블클릭: 초기화", "Double-click: reset"));
        let track_w = (full_w - label_w - value_w - 12.0).max(40.0);
        let (trect, tresp) = ui.allocate_exact_size(vec2(track_w, h), Sense::click_and_drag());
        let span = max - min;
        // Value <-> track position (gamma curve)
        let to_t = |val: f32| ((val - min) / span).clamp(0.0, 1.0).powf(1.0 / gamma);
        let from_t = |t: f32| min + span * t.clamp(0.0, 1.0).powf(gamma);
        let to_x = |val: f32| trect.left() + to_t(val) * trect.width();
        let p = ui.painter();
        let cy = trect.center().y;
        if let Some(g) = grad {
            grad_bar(p, Rect::from_min_max(pos2(trect.left(), cy - 1.5), pos2(trect.right(), cy + 1.5)), g);
            if min < 0.0 && max > 0.0 {
                let zx = to_x(0.0);
                p.line_segment([pos2(zx, cy - 4.0), pos2(zx, cy + 4.0)], Stroke::new(1.0, TEXT_DIM()));
            }
        } else {
            p.line_segment([pos2(trect.left(), cy), pos2(trect.right(), cy)], Stroke::new(2.0, WIDGET()));
        }
        if grad.is_some() {
        } else if min < 0.0 && max > 0.0 {
            let zx = to_x(0.0);
            p.line_segment([pos2(zx, cy - 4.0), pos2(zx, cy + 4.0)], Stroke::new(1.0, TEXT_DIM()));
            p.line_segment([pos2(zx, cy), pos2(to_x(*v), cy)], Stroke::new(2.0, TEXT_WEAK()));
        } else {
            p.line_segment([pos2(trect.left(), cy), pos2(to_x(*v), cy)], Stroke::new(2.0, TEXT_WEAK()));
        }
        let hx = to_x(v.clamp(min, max));
        let hot = tresp.hovered() || tresp.dragged();
        p.circle_filled(pos2(hx, cy), if hot { 6.0 } else { 5.0 }, if tresp.dragged() { STRONG() } else { TEXT() });
        if tresp.double_clicked() {
            *v = default;
            e.changed = true;
            e.committed = true;
        } else if tresp.dragged() || tresp.drag_started() {
            let fine = ui.input(|i| i.modifiers.shift);
            if fine {
                let dx = tresp.drag_delta().x;
                let nv = from_t(to_t(*v) + dx / trect.width() * 0.1).clamp(min, max);
                if nv != *v {
                    *v = nv;
                    e.changed = true;
                }
            } else if let Some(pp) = tresp.interact_pointer_pos() {
                let nv = from_t((pp.x - trect.left()) / trect.width()).clamp(min, max);
                let step = 10f32.powi(-(decimals as i32));
                let nv = (nv / step).round() * step;
                if nv != *v {
                    *v = nv;
                    e.changed = true;
                }
            }
            e.active = true;
        } else if tresp.clicked()
            && let Some(pp) = tresp.interact_pointer_pos() {
                let step = 10f32.powi(-(decimals as i32));
                *v = (from_t((pp.x - trect.left()) / trect.width()).clamp(min, max) / step).round() * step;
                e.changed = true;
                e.committed = true;
            }
        if tresp.drag_stopped() {
            e.committed = true;
        }
        let r = value_box(ui, vec2(value_w, h - 2.0), v, min, max, span / 300.0, decimals, min < 0.0, "", id);
        e.merge(r);
        let _ = id;
    });
    if e.changed && !e.active {
        e.committed = true;
    }
    e
}

/// Color temperature (K) slider: logarithmic track (more room at the warm/low end), value box in K.
pub fn slider_kelvin(ui: &mut Ui, label: &str, k: &mut f32, default: f32) -> Edit {
    const KMIN: f32 = 2000.0;
    const KMAX: f32 = 50000.0;
    let to_pos = |k: f32| (k.clamp(KMIN, KMAX) / KMIN).ln() / (KMAX / KMIN).ln() * 100.0;
    let from_pos = |p: f32| KMIN * (KMAX / KMIN).powf(p.clamp(0.0, 100.0) / 100.0);
    let mut pos = to_pos(*k);
    let before = pos;
    let mut e = Edit::default();
    let full_w = ui.available_width();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let (lrect, lresp) = ui.allocate_exact_size(vec2(SLIDER_LABEL_W, 20.0), Sense::click());
        ui.painter().text(lrect.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(12.0), TEXT());
        if lresp.double_clicked() {
            *k = default;
            e.changed = true;
            e.committed = true;
        }
        let track_w = (full_w - SLIDER_LABEL_W - SLIDER_VALUE_W - 12.0).max(40.0);
        let (trect, tresp) = ui.allocate_exact_size(vec2(track_w, 20.0), Sense::click_and_drag());
        // Temperature track: blue -> yellow (data color showing photo color, exempt from the neutral UI palette)
        let p = ui.painter();
        let cy = trect.center().y;
        let mut mesh = egui::Mesh::default();
        let seg = 24;
        for i in 0..=seg {
            let t = i as f32 / seg as f32;
            let col = Color32::from_rgb((90.0 + 140.0 * t) as u8, (120.0 + 80.0 * t) as u8, (220.0 - 160.0 * t) as u8);
            let x = trect.left() + t * trect.width();
            mesh.colored_vertex(pos2(x, cy - 1.5), col);
            mesh.colored_vertex(pos2(x, cy + 1.5), col);
            if i > 0 {
                let b = mesh.vertices.len() as u32;
                mesh.add_triangle(b - 4, b - 3, b - 2);
                mesh.add_triangle(b - 3, b - 2, b - 1);
            }
        }
        p.add(egui::Shape::mesh(mesh));
        let hx = trect.left() + pos / 100.0 * trect.width();
        p.circle_filled(pos2(hx, cy), if tresp.hovered() || tresp.dragged() { 6.0 } else { 5.0 }, TEXT());
        if tresp.double_clicked() {
            *k = default;
            e.changed = true;
            e.committed = true;
        } else if tresp.dragged() || tresp.drag_started() || tresp.clicked() {
            if let Some(pp) = tresp.interact_pointer_pos() {
                if ui.input(|i| i.modifiers.shift) {
                    pos = (pos + tresp.drag_delta().x / trect.width() * 10.0).clamp(0.0, 100.0);
                } else {
                    pos = ((pp.x - trect.left()) / trect.width() * 100.0).clamp(0.0, 100.0);
                }
                if pos != before {
                    *k = (from_pos(pos) / 50.0).round() * 50.0;
                    e.changed = true;
                }
            }
            e.active = tresp.dragged();
        }
        if tresp.drag_stopped() || tresp.clicked() {
            e.committed = true;
        }
        let r = value_box(ui, vec2(SLIDER_VALUE_W, 18.0), k, KMIN, KMAX, 10.0, 0, false, " K", label);
        e.merge(r);
    });
    if e.changed && !e.active {
        e.committed = true;
    }
    e
}

/// Histogram (filled luminance + per-channel max outline) with clipping indicator triangles.
/// Histogram regions: blacks, shadows, exposure, highlights, whites (drag directly to adjust).
pub const HIST_ZONES: [(&str, f32, f32); 5] = [("블랙", 0.0, 0.1), ("섀도", 0.1, 0.33), ("노출", 0.33, 0.67), ("하이라이트", 0.67, 0.9), ("화이트", 0.9, 1.0)];

/// Returns the region being dragged: (region, horizontal drag as a fraction, released).
pub fn histogram(ui: &mut Ui, h: Option<&Histogram>, clip: &mut bool, info: &str) -> Option<(usize, f32, bool)> {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 96.0), Sense::click_and_drag());
    let zone_at = |x: f32| -> usize {
        let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        HIST_ZONES.iter().position(|z| t >= z.1 && t <= z.2).unwrap_or(2)
    };
    let zid = resp.id.with("zone");
    let active_zone: Option<usize> = if resp.dragged() || resp.drag_stopped() { ui.data(|d| d.get_temp(zid)) } else { None };
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, Color32::from_rgb(0x0E, 0x0E, 0x0E));
    for i in 1..4 {
        let x = rect.left() + rect.width() * i as f32 / 4.0;
        p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, Color32::from_rgb(0x1C, 0x1C, 0x1C)));
    }
    if let Some(h) = h {
        let maxv = (0..256).map(|i| h.r[i].max(h.g[i]).max(h.b[i])).skip(2).take(252).max().unwrap_or(1).max(1) as f32;
        let ypos = |c: u32| rect.bottom() - 2.0 - ((c as f32 / maxv).sqrt().min(1.0)) * (rect.height() - 6.0);
        let xpos = |i: usize| rect.left() + i as f32 / 255.0 * rect.width();
        // Filled luminance
        let mut mesh = egui::Mesh::default();
        for i in 0..256 {
            let x = xpos(i);
            let y = ypos(h.l[i]);
            let idx = mesh.vertices.len() as u32;
            mesh.colored_vertex(pos2(x, rect.bottom()), Color32::from_gray(70));
            mesh.colored_vertex(pos2(x, y), Color32::from_gray(110));
            if i > 0 {
                mesh.add_triangle(idx - 2, idx - 1, idx);
                mesh.add_triangle(idx - 1, idx, idx + 1);
            }
        }
        p.add(egui::Shape::mesh(mesh));
        // Per-channel max outline
        let pts: Vec<Pos2> = (0..256).map(|i| pos2(xpos(i), ypos(h.r[i].max(h.g[i]).max(h.b[i])))).collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.0, Color32::from_gray(200))));
        // Clipping triangles
        let total: u32 = h.l.iter().sum::<u32>().max(1);
        let hi_clip = (h.r[255].max(h.g[255]).max(h.b[255])) as f32 / total as f32 > 0.0005;
        let lo_clip = (h.r[0].min(h.g[0]).min(h.b[0])) as f32 / total as f32 > 0.0005;
        let tri = |x: f32, right: bool, on: bool| {
            let y = rect.top() + 4.0;
            let d = if right { -9.0 } else { 9.0 };
            let col = if on { if right { ACCENT } else { STRONG() } } else { TEXT_DIM() };
            p.add(egui::Shape::convex_polygon(vec![pos2(x, y), pos2(x + d, y), pos2(x, y + 9.0)], col, Stroke::NONE));
        };
        tri(rect.left() + 3.0, false, lo_clip || *clip);
        tri(rect.right() - 3.0, true, hi_clip || *clip);
    }
    p.text(rect.left_bottom() + vec2(6.0, -4.0), Align2::LEFT_BOTTOM, info, FontId::monospace(10.0), TEXT_WEAK());
    // Region highlight (hover/drag)
    let hz = active_zone.or_else(|| resp.hover_pos().map(|hp| zone_at(hp.x)));
    if let Some(z) = hz {
        let (name, a, b) = HIST_ZONES[z];
        let zr = Rect::from_min_max(pos2(rect.left() + a * rect.width(), rect.top()), pos2(rect.left() + b * rect.width(), rect.bottom()));
        p.rect_filled(zr, 0.0, Color32::from_white_alpha(14));
        p.text(rect.right_bottom() + vec2(-6.0, -4.0), Align2::RIGHT_BOTTOM, format!("{} ↔", crate::i18n::t(name)), FontId::proportional(10.5), Color32::from_gray(190));
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if resp.drag_started()
        && let Some(pp) = resp.interact_pointer_pos() {
            ui.data_mut(|d| d.insert_temp(zid, zone_at(pp.x)));
        }
    let mut out = None;
    if let Some(z) = active_zone {
        out = Some((z, resp.drag_delta().x / rect.width(), resp.drag_stopped()));
    }
    if resp.clicked() {
        *clip = !*clip;
    }
    resp.on_hover_text(tr!("좌우 드래그: 해당 영역 조정 · 클릭: 클리핑 표시 (J)", "Drag left/right: adjust that region · click: show clipping (J)"));
    out
}

/// Point curve editor graph size (square, shared by all modes).
pub fn curve_box_size(ui: &Ui) -> f32 {
    ui.available_width().clamp(120.0, 320.0)
}

pub fn curve_editor(ui: &mut Ui, pts: &mut Vec<[f32; 2]>, line: Color32, id: &str) -> Edit {
    let mut e = Edit::default();
    let w = curve_box_size(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, w), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, Color32::from_rgb(0x0E, 0x0E, 0x0E));
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let x = rect.left() + rect.width() * t;
        let y = rect.top() + rect.height() * t;
        p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0, Color32::from_gray(30)));
        p.line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0, Color32::from_gray(30)));
    }
    p.line_segment([rect.left_bottom(), rect.right_top()], Stroke::new(1.0, Color32::from_gray(40)));
    let to_s = |q: [f32; 2]| pos2(rect.left() + q[0] * rect.width(), rect.bottom() - q[1] * rect.height());
    let from_s = |s: Pos2| [((s.x - rect.left()) / rect.width()).clamp(0.0, 1.0), ((rect.bottom() - s.y) / rect.height()).clamp(0.0, 1.0)];
    let drag_id = ui.make_persistent_id(("curve_drag", id));
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(drag_id));
    if resp.drag_started() || (resp.clicked() && dragging.is_none()) {
        // Hit-test at the press position (drag starts after a few pixels, which would miss the point), with a generous radius
        if let Some(pp) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
            let near = pts.iter().enumerate().map(|(i, q)| (i, to_s(*q).distance(pp))).filter(|(_, d)| *d < 16.0).min_by(|a, b| a.1.total_cmp(&b.1));
            dragging = match near {
                Some((i, _)) => Some(i),
                None => {
                    let q = from_s(pp);
                    let y = monotone_spline(pts)(q[0]);
                    let np = [q[0], y];
                    let idx = pts.iter().position(|a| a[0] > np[0]).unwrap_or(pts.len());
                    pts.insert(idx, np);
                    e.changed = true;
                    Some(idx)
                }
            };
        }
    }
    if resp.dragged()
        && let (Some(i), Some(pp)) = (dragging, resp.interact_pointer_pos())
            && i < pts.len() {
                let mut q = from_s(pp);
                let lo = if i > 0 { pts[i - 1][0] + 0.01 } else { 0.0 };
                let hi = if i + 1 < pts.len() { pts[i + 1][0] - 0.01 } else { 1.0 };
                q[0] = q[0].clamp(lo, hi.max(lo));
                // Dragging a point outside removes it (except endpoints)
                let outside = !rect.expand(24.0).contains(pp);
                if outside && i > 0 && i + 1 < pts.len() {
                    pts.remove(i);
                    dragging = None;
                } else {
                    pts[i] = q;
                }
                e.changed = true;
                e.active = true;
            }
    if resp.drag_stopped() {
        dragging = None;
        e.committed = true;
    }
    // Double-click or right-click: remove the nearest point (except endpoints)
    if (resp.double_clicked() || resp.secondary_clicked())
        && let Some(pp) = resp.interact_pointer_pos()
            && let Some(i) = pts.iter().position(|q| to_s(*q).distance(pp) < 16.0)
                && i > 0 && i + 1 < pts.len() {
                    pts.remove(i);
                    e.changed = true;
                    e.committed = true;
                }
    ui.data_mut(|d| match dragging {
        Some(v) => {
            d.insert_temp(drag_id, v);
        }
        None => {
            d.remove::<usize>(drag_id);
        }
    });
    let f = monotone_spline(pts);
    let curve: Vec<Pos2> = (0..=128).map(|i| {
        let x = i as f32 / 128.0;
        to_s([x, f(x).clamp(0.0, 1.0)])
    }).collect();
    p.add(egui::Shape::line(curve, Stroke::new(1.5, line)));
    for q in pts.iter() {
        let s = to_s(*q);
        p.circle_filled(s, 4.0, PANEL());
        p.circle_stroke(s, 4.0, Stroke::new(1.5, line));
    }
    if let Some(hp) = resp.hover_pos() {
        let q = from_s(hp);
        p.text(rect.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, format!("{:.0} / {:.0}", q[0] * 255.0, f(q[0]) * 255.0), FontId::monospace(10.0), TEXT_WEAK());
    }
    if e.changed && !e.active {
        e.committed = true;
    }
    e
}

/// Hue disc: bright neutral center, saturation increasing outward (each ring is drawn with real HSV colors so it does not turn muddy).
/// Same colors regardless of theme (data colors showing what will be applied to the photo).
pub fn paint_hue_disc(p: &egui::Painter, c: Pos2, r: f32) {
    let seg = 72;
    let rings = 8;
    let mut mesh = egui::Mesh::default();
    let col_at = |hue: f32, t: f32| {
        // Saturation scales with radius; value drops slightly toward the edge (white center, vivid edge)
        let rgb = hsv_to_rgb(hue, (t * 0.92).min(1.0), 0.97 - 0.07 * t);
        Color32::from_rgb((rgb[0] * 255.0) as u8, (rgb[1] * 255.0) as u8, (rgb[2] * 255.0) as u8)
    };
    for k in 0..=rings {
        let t = k as f32 / rings as f32;
        for i in 0..=seg {
            let a = i as f32 / seg as f32 * std::f32::consts::TAU;
            mesh.colored_vertex(c + vec2(a.cos(), -a.sin()) * r * t, col_at(a.to_degrees(), t));
        }
    }
    let row = (seg + 1) as u32;
    for k in 0..rings as u32 {
        for i in 0..seg as u32 {
            let (a0, a1) = (k * row + i, k * row + i + 1);
            let (b0, b1) = ((k + 1) * row + i, (k + 1) * row + i + 1);
            mesh.add_triangle(a0, b0, b1);
            mesh.add_triangle(a0, b1, a1);
        }
    }
    p.add(egui::Shape::mesh(mesh));
    p.circle_stroke(c, r, Stroke::new(1.0, Color32::from_black_alpha(70)));
}

/// Color grading wheel. Wheel colors are data (the hue applied to the photo), exempt from the neutral UI palette.
pub fn color_wheel(ui: &mut Ui, wheel: &mut Wheel, size: f32, label: &str) -> Edit {
    let mut e = Edit::default();
    ui.vertical(|ui| {
        let (tr, _) = ui.allocate_exact_size(vec2(size, 16.0), Sense::hover());
        ui.painter().text(tr.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.5), TEXT_WEAK());
        let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
        let c = rect.center();
        let r = size * 0.5 - 4.0;
        let p = ui.painter_at(rect.expand(2.0));
        paint_hue_disc(&p, c, r);
        let a = wheel.hue.to_radians();
        let pr = wheel.sat / 100.0 * r;
        let hp = c + vec2(a.cos(), -a.sin()) * pr;
        p.circle_filled(hp, 5.0, Color32::WHITE);
        p.circle_stroke(hp, 5.0, Stroke::new(1.5, Color32::BLACK));
        if resp.double_clicked() {
            wheel.hue = 0.0;
            wheel.sat = 0.0;
            e.changed = true;
            e.committed = true;
        } else if (resp.dragged() || resp.drag_started() || resp.clicked())
            && let Some(pp) = resp.interact_pointer_pos() {
                let d = pp - c;
                let fine = ui.input(|i| i.modifiers.shift);
                let sat = (d.length() / r * 100.0).clamp(0.0, 100.0);
                let hue = (-d.y).atan2(d.x).to_degrees().rem_euclid(360.0);
                if fine {
                    wheel.sat = sat.min(wheel.sat + 2.0).max(wheel.sat - 2.0);
                } else {
                    wheel.sat = sat;
                }
                wheel.hue = hue;
                e.changed = true;
                e.active = resp.dragged();
            }
        if resp.drag_stopped() {
            e.committed = true;
        }
        resp.on_hover_text(tr!("드래그: 색상/채도 · 더블클릭: 초기화", "Drag: hue/saturation · double-click: reset"));
        let (vr, _) = ui.allocate_exact_size(vec2(size, 16.0), Sense::hover());
        let txt = if wheel.sat < 0.5 { "—".to_string() } else { format!("{:.0}° · {:.0}", wheel.hue, wheel.sat) };
        ui.painter().text(vr.center(), Align2::CENTER_CENTER, txt, FontId::proportional(10.5), TEXT_DIM());
    });
    if e.changed && !e.active {
        e.committed = true;
    }
    e
}

/// Draws a star rating (0..5). Returns the clicked rating.
pub fn stars(p: &egui::Painter, pos: Pos2, rating: u8, size: f32) {
    for i in 0..5 {
        let c = pos + vec2(i as f32 * (size + 2.0), 0.0);
        let on = i < rating;
        if on {
            star_shape(p, c, size * 0.5, TEXT());
        } else {
            p.circle_filled(c, 1.2, TEXT_DIM());
        }
    }
}

pub fn star_shape(p: &egui::Painter, c: Pos2, r: f32, col: Color32) {
    let mut pts = Vec::with_capacity(10);
    for k in 0..10 {
        let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 5.0;
        let rr = if k % 2 == 0 { r } else { r * 0.45 };
        pts.push(c + vec2(a.cos(), a.sin()) * rr);
    }
    // Concave polygon: drawn as a fan from the center
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(c, col);
    for q in &pts {
        mesh.colored_vertex(*q, col);
    }
    for k in 0..10u32 {
        mesh.add_triangle(0, 1 + k, 1 + (k + 1) % 10);
    }
    p.add(egui::Shape::mesh(mesh));
}

/// Edit badges on thumbnails, drawn right to left.
/// Order: exported, mask, crop, develop adjustments. Returns (rect, name) for tooltips.
pub fn edit_badges(p: &egui::Painter, right_center: Pos2, sum: crate::develop::settings::EditSummary, exported: bool, chip: bool) -> Vec<(Rect, &'static str)> {
    let s = 12.0;
    let gap = 3.0;
    let mut x = right_center.x;
    let y = right_center.y;
    // Always bright over thumbnails (chip), theme text color over panels
    let col = if chip { Color32::from_gray(215) } else { TEXT_WEAK() };
    let st = Stroke::new(1.1, col);
    let mut out = Vec::new();
    let mut next = |name: &'static str| {
        let r = Rect::from_center_size(pos2(x - s * 0.5, y), vec2(s, s));
        x -= s + gap;
        if chip {
            p.rect_filled(r.expand(1.5), 3.0, Color32::from_black_alpha(150));
        }
        out.push((r, name));
        r
    };
    if exported {
        let r = next(tr!("내보냄", "Exported"));
        let c = r.shrink(2.0);
        // Box + arrow to the upper right
        p.line_segment([pos2(c.left(), c.top() + 3.0), pos2(c.left(), c.bottom())], st);
        p.line_segment([pos2(c.left(), c.bottom()), pos2(c.right() - 3.0, c.bottom())], st);
        p.line_segment([pos2(c.center().x - 1.0, c.center().y + 1.0), c.right_top()], st);
        p.line_segment([c.right_top(), c.right_top() + vec2(-4.0, 0.0)], st);
        p.line_segment([c.right_top(), c.right_top() + vec2(0.0, 4.0)], st);
    }
    if sum.masks {
        let r = next(tr!("로컬 보정(마스크)", "Local adjustments (masks)"));
        let c = r.center();
        let rad = s * 0.36;
        p.circle_stroke(c, rad, st);
        let pts: Vec<Pos2> = (0..=12)
            .map(|i| {
                let a = std::f32::consts::FRAC_PI_2 + i as f32 / 12.0 * std::f32::consts::PI;
                pos2(c.x + rad * a.cos(), c.y + rad * a.sin())
            })
            .collect();
        p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
    }
    if sum.crop {
        let r = next(tr!("자르기·회전", "Crop·Rotate"));
        let c = r.shrink(2.0);
        let k = 3.0;
        p.line_segment([pos2(c.left() + k, c.top() - 1.0), pos2(c.left() + k, c.bottom() - k)], st);
        p.line_segment([pos2(c.left() + k, c.bottom() - k), pos2(c.right() + 1.0, c.bottom() - k)], st);
        p.line_segment([pos2(c.left() - 1.0, c.top() + k), pos2(c.right() - k, c.top() + k)], st);
        p.line_segment([pos2(c.right() - k, c.top() + k), pos2(c.right() - k, c.bottom() + 1.0)], st);
    }
    if sum.adjust {
        let r = next(tr!("현상 조정", "Develop adjustments"));
        let c = r.center();
        p.line_segment([c + vec2(-3.5, -2.0), c + vec2(3.5, -2.0)], st);
        p.line_segment([c + vec2(0.0, -5.5), c + vec2(0.0, 1.5)], st);
        p.line_segment([c + vec2(-3.5, 4.0), c + vec2(3.5, 4.0)], st);
    }
    out
}

pub fn flag_icon(p: &egui::Painter, rect: Rect, flag: crate::catalog::Flag) {
    use crate::catalog::Flag;
    match flag {
        Flag::Pick => {
            let x = rect.left() + 2.0;
            p.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.5, TEXT()));
            p.add(egui::Shape::convex_polygon(
                vec![pos2(x, rect.top()), pos2(rect.right(), rect.top() + rect.height() * 0.25), pos2(x, rect.top() + rect.height() * 0.5)],
                TEXT(),
                Stroke::NONE,
            ));
        }
        Flag::Reject => {
            p.line_segment([rect.left_top(), rect.right_bottom()], Stroke::new(1.8, ACCENT));
            p.line_segment([rect.right_top(), rect.left_bottom()], Stroke::new(1.8, ACCENT));
        }
        Flag::None => {}
    }
}

/// Collapsible section header (mono label).
pub fn section<R>(ui: &mut Ui, title: &str, default_open: bool, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    let id = ui.make_persistent_id(("section", title));
    let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
    let open = st.is_open();
    let hr = ui.horizontal(|ui| {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
        let p = ui.painter();
        p.text(
            rect.left_center() + vec2(2.0, 0.0),
            Align2::LEFT_CENTER,
            title.to_uppercase(),
            FontId::monospace(11.0),
            if resp.hovered() { TEXT() } else { TEXT_WEAK() },
        );
        p.text(rect.right_center() - vec2(4.0, 0.0), Align2::RIGHT_CENTER, if open { "−" } else { "+" }, FontId::monospace(12.0), TEXT_WEAK());
        resp
    });
    if hr.inner.clicked() {
        st.toggle(ui);
    }
    st.store(ui.ctx());
    let r = if st.is_open() {
        let r = ui.indent(id, |ui| add(ui)).inner;
        ui.add_space(4.0);
        Some(r)
    } else {
        None
    };
    let y = ui.cursor().top();
    let x0 = ui.min_rect().left();
    let x1 = ui.max_rect().right();
    ui.painter().line_segment([pos2(x0, y), pos2(x1, y)], Stroke::new(1.0, BORDER()));
    ui.add_space(2.0);
    r
}

pub fn icon_button(ui: &mut Ui, text: &str, selected: bool, tip: &str) -> egui::Response {
    let r = ui.add(egui::Button::new(egui::RichText::new(text).size(12.0)).selected(selected).min_size(vec2(26.0, 22.0)));
    r.on_hover_text(tip)
}

pub fn label_dot(p: &egui::Painter, c: Pos2, r: f32, label: crate::catalog::ColorLabel) {
    let [rr, gg, bb] = label.rgb();
    p.circle_filled(c, r, Color32::from_rgb(rr, gg, bb));
}

pub fn vec_len(v: Vec2) -> f32 {
    v.length()
}

/// Small on/off switch (on = accent color).
pub fn toggle(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let size = vec2(32.0, 16.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let p = ui.painter();
    let bg = if *on { ACCENT } else { WIDGET() };
    p.rect_filled(rect, rect.height() * 0.5, bg);
    let x = egui::lerp((rect.left() + 8.0)..=(rect.right() - 8.0), t);
    p.circle_filled(pos2(x, rect.center().y), 6.0, if resp.hovered() { STRONG() } else { TEXT() });
    resp
}


/// Extended section header: modified marker (accent dot), reset (↺) and temporary disable (eye) buttons.
/// Returns (body result, reset clicked).
pub fn section_ex<R>(ui: &mut Ui, title: &str, default_open: bool, modified: bool, bypass: Option<&mut bool>, add: impl FnOnce(&mut Ui) -> R) -> (Option<R>, bool) {
    let id = ui.make_persistent_id(("section", title));
    let mut st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, default_open);
    let open = st.is_open();
    let mut reset = false;
    let full = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(full, 26.0), Sense::click());
    let off = bypass.as_ref().map(|b| **b).unwrap_or(false);
    // Buttons on the right
    let btn = |ui: &mut Ui, r: Rect, txt: &str, tip: &str, on: bool| -> bool {
        let rr = ui.interact(r, ui.id().with(("secbtn", title, txt)), Sense::click());
        let col = if on { ACCENT } else if rr.hovered() { TEXT() } else { TEXT_DIM() };
        if rr.hovered() {
            ui.painter().rect_filled(r, 3.0, WIDGET());
        }
        ui.painter().text(r.center(), Align2::CENTER_CENTER, txt, FontId::proportional(12.0), col);
        rr.on_hover_text(tip).clicked()
    };
    let mut x = rect.right() - 2.0;
    let r_toggle = Rect::from_center_size(pos2(x - 9.0, rect.center().y), vec2(18.0, 20.0));
    x -= 20.0;
    let r_eye = Rect::from_center_size(pos2(x - 9.0, rect.center().y), vec2(18.0, 20.0));
    if bypass.is_some() {
        x -= 20.0;
    }
    let r_reset = Rect::from_center_size(pos2(x - 9.0, rect.center().y), vec2(18.0, 20.0));
    let p = ui.painter();
    if modified {
        p.circle_filled(rect.left_center() + vec2(4.0, 0.0), 2.5, if off { TEXT_DIM() } else { ACCENT });
    }
    p.text(
        rect.left_center() + vec2(12.0, 0.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(12.5),
        if off { TEXT_DIM() } else if resp.hovered() || open { TEXT() } else { TEXT_WEAK() },
    );
    p.text(r_toggle.center(), Align2::CENTER_CENTER, if open { "−" } else { "+" }, FontId::monospace(12.0), TEXT_WEAK());
    if let Some(b) = bypass
        && btn(ui, r_eye, if *b { "◌" } else { "●" }, if *b { tr!("다시 켜기", "Turn back on") } else { tr!("이 섹션을 잠시 끄고 비교", "Temporarily turn off this section to compare") }, *b) {
            *b = !*b;
        }
    if modified && btn(ui, r_reset, "↺", tr!("이 섹션 초기화", "Reset this section"), false) {
        reset = true;
    }
    if resp.clicked() {
        st.toggle(ui);
    }
    st.store(ui.ctx());
    let r = if st.is_open() {
        ui.add_space(2.0);
        let r = ui.indent(id, |ui| add(ui)).inner;
        ui.add_space(6.0);
        Some(r)
    } else {
        None
    };
    let y = ui.cursor().top();
    ui.painter().line_segment([pos2(rect.left(), y), pos2(rect.right(), y)], Stroke::new(1.0, BORDER()));
    ui.add_space(2.0);
    (r, reset)
}

pub fn band_color(hue: f32, s: f32, v: f32) -> Color32 {
    let rgb = hsv_to_rgb(hue.rem_euclid(360.0), s, v);
    Color32::from_rgb((rgb[0] * 255.0) as u8, (rgb[1] * 255.0) as u8, (rgb[2] * 255.0) as u8)
}

/// Band color for display: darkened in the light theme so bright hues like yellow and sky blue stay visible on the background.
pub fn band_ui(hue: f32, s: f32, v: f32) -> Color32 {
    if current_theme().is_light() {
        // Darken yellow to green (40-180°) more, since those hues look brighter
        let h = hue.rem_euclid(360.0);
        let bright = (1.0 - ((h - 90.0) / 70.0).powi(2)).max(0.0) * 0.18 + if (160.0..=200.0).contains(&h) { 0.1 } else { 0.0 };
        band_color(hue, (s * 1.15 + 0.1).min(1.0), (v * 0.78 - bright).clamp(0.25, 1.0))
    } else {
        band_color(hue, s, v)
    }
}

/// HSL mixer: 8 colors x (hue, saturation, luminance) shown together like an equalizer, adjusted by dragging directly.
/// Click a bar = select that color, drag up/down = value, double-click = reset. Returns (edited, changed item name).
pub fn hsl_mixer(ui: &mut Ui, hsl: &mut [crate::develop::settings::HslBand; 8], sel: &mut usize, names: &[&str; 8], hues: &[f32; 8]) -> (Edit, String) {
    let mut e = Edit::default();
    let mut lab = String::new();
    let full = ui.available_width();
    let label_w = 40.0;
    let cw = ((full - label_w - 4.0) / 8.0).floor();
    let bar_h = 34.0;
    for (row, rname) in [tr!("색조", "Hue"), tr!("채도", "Sat."), tr!("광도", "Lum.")].iter().enumerate() {
        let (rect, _) = ui.allocate_exact_size(vec2(full, bar_h + 4.0), Sense::hover());
        let p = ui.painter();
        p.text(pos2(rect.left(), rect.center().y), Align2::LEFT_CENTER, *rname, FontId::proportional(11.0), TEXT_WEAK());
        for b in 0..8 {
            let r = Rect::from_min_size(pos2(rect.left() + label_w + b as f32 * cw + 2.0, rect.top() + 2.0), vec2(cw - 4.0, bar_h));
            let resp = ui.interact(r, ui.id().with(("hslbar", row, b)), Sense::click_and_drag());
            let v: &mut f32 = match row {
                0 => &mut hsl[b].hue,
                1 => &mut hsl[b].sat,
                _ => &mut hsl[b].lum,
            };
            let selected = *sel == b;
            let p = ui.painter();
            p.rect_filled(r, 3.0, WIDGET());
            if selected {
                p.rect_stroke(r, 3.0, Stroke::new(1.0, TEXT_WEAK()), egui::StrokeKind::Inside);
            } else if resp.hovered() {
                p.rect_stroke(r, 3.0, Stroke::new(1.0, TEXT_DIM()), egui::StrokeKind::Inside);
            }
            let cy = r.center().y;
            p.line_segment([pos2(r.left() + 3.0, cy), pos2(r.right() - 3.0, cy)], Stroke::new(1.0, TEXT_DIM().gamma_multiply(0.6)));
            let col = match row {
                0 => band_ui(hues[b] + *v * 0.3, 0.75, 0.9),
                1 => band_ui(hues[b], (0.45 + *v / 200.0).clamp(0.05, 1.0), 0.85),
                _ => band_ui(hues[b], 0.6, (0.6 + *v / 250.0).clamp(0.2, 1.0)),
            };
            let y = cy - *v / 100.0 * (bar_h * 0.5 - 2.0);
            if v.abs() > 0.5 {
                let top = y.min(cy);
                let bot = y.max(cy);
                p.rect_filled(Rect::from_min_max(pos2(r.left() + 4.0, top), pos2(r.right() - 4.0, bot)), 1.5, col);
            }
            p.line_segment([pos2(r.left() + 2.0, y), pos2(r.right() - 2.0, y)], Stroke::new(2.0, if resp.dragged() { STRONG() } else { col }));
            if row == 0 {
                // Band color strip on top
                p.rect_filled(Rect::from_min_size(r.left_top() + vec2(4.0, 2.0), vec2(r.width() - 8.0, 2.0)), 1.0, band_ui(hues[b], 0.75, 0.9));
            }
            if resp.double_clicked() {
                *v = 0.0;
                e.changed = true;
                e.committed = true;
                lab = format!("HSL {} {}", names[b], rname);
            } else if resp.drag_started() || resp.clicked() {
                *sel = b;
            }
            if resp.dragged() {
                let k = if ui.input(|i| i.modifiers.shift) { 0.25 } else { 1.0 };
                let dv = -resp.drag_delta().y / (bar_h * 0.5) * 100.0 * 0.5 * k;
                let nv = (*v + dv).clamp(-100.0, 100.0);
                if nv != *v {
                    *v = nv.round();
                    e.changed = true;
                    lab = format!("HSL {} {}", names[b], rname);
                }
                e.active = true;
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            }
            if resp.drag_stopped() {
                e.committed = true;
            }
            resp.on_hover_text(trf!("{} {} {:+.0}\n위아래 드래그 · 더블클릭 초기화", "{} {} {:+.0}\nDrag up/down · double-click to reset", names[b], rname, *v));
        }
    }
    // Detail sliders for the selected color (gradient tracks)
    ui.add_space(4.0);
    {
        let (row, _) = ui.allocate_exact_size(vec2(full, 22.0), Sense::hover());
        ui.painter().text(pos2(row.left(), row.center().y), Align2::LEFT_CENTER, names[*sel], FontId::proportional(11.5), TEXT());
        for b in 0..8 {
            let r = Rect::from_center_size(pos2(row.left() + label_w + b as f32 * cw + cw * 0.5, row.center().y), vec2(cw.min(22.0), 20.0));
            let resp = ui.interact(r, ui.id().with(("hsldot", b)), Sense::click());
            let modified = hsl[b].hue != 0.0 || hsl[b].sat != 0.0 || hsl[b].lum != 0.0;
            ui.painter().circle_filled(r.center(), if *sel == b { 7.0 } else { 5.5 }, band_ui(hues[b], 0.75, 0.9));
            ui.painter().circle_stroke(r.center(), if *sel == b { 7.0 } else { 5.5 }, Stroke::new(0.8, BORDER()));
            if *sel == b {
                ui.painter().circle_stroke(r.center(), 8.5, Stroke::new(1.2, TEXT()));
            } else if modified {
                ui.painter().circle_stroke(r.center(), 7.5, Stroke::new(1.0, TEXT_WEAK()));
            }
            if resp.on_hover_text(names[b]).clicked() {
                *sel = b;
            }
        }
    }
    let b = *sel;
    let h0 = hues[b];
    let hue_grad: Vec<Color32> = (0..=8).map(|i| band_ui(h0 + (i as f32 / 8.0 - 0.5) * 60.0, 0.75, 0.9)).collect();
    let sat_grad = [band_ui(h0, 0.0, 0.8), band_ui(h0, 0.5, 0.85), band_ui(h0, 1.0, 0.9)];
    let lum_grad = [band_ui(h0, 0.7, 0.25), band_ui(h0, 0.7, 0.65), band_ui(h0, 0.35, 1.0)];
    for (name, v, g) in [(tr!("색조", "Hue"), &mut hsl[b].hue, &hue_grad[..]), (tr!("채도", "Saturation"), &mut hsl[b].sat, &sat_grad[..]), (tr!("광도", "Luminance"), &mut hsl[b].lum, &lum_grad[..])] {
        let r = slider_grad(ui, name, v, -100.0, 100.0, 0.0, 0, &format!("hsl_{b}_{name}"), g);
        if r.changed || r.committed {
            lab = format!("HSL {} {name}", names[b]);
        }
        e.merge(r);
    }
    if e.changed && !e.active {
        e.committed = true;
    }
    (e, lab)
}

/// Small color wheel thumbnail (color grading region chip) showing the current color point.
pub fn mini_wheel(ui: &mut Ui, wheel: &Wheel, size: f32, selected: bool, label: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size + 8.0, size + 18.0), Sense::click());
    let c = pos2(rect.center().x, rect.top() + 4.0 + size * 0.5);
    let r = size * 0.5;
    let p = ui.painter();
    if selected || resp.hovered() {
        p.rect_filled(rect, 4.0, if selected { WIDGET() } else { PANEL2() });
    }
    paint_hue_disc(p, c, r);
    let a = wheel.hue.to_radians();
    let hp = c + vec2(a.cos(), -a.sin()) * (wheel.sat / 100.0 * r);
    p.circle_filled(hp, 2.5, Color32::WHITE);
    p.circle_stroke(hp, 2.5, Stroke::new(1.0, Color32::BLACK));
    let modified = wheel.sat != 0.0 || wheel.lum != 0.0;
    p.text(
        pos2(rect.center().x, rect.bottom() - 7.0),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(10.5),
        if selected { TEXT() } else if modified { TEXT_WEAK() } else { TEXT_DIM() },
    );
    if modified {
        p.circle_filled(rect.right_top() + vec2(-4.0, 4.0), 2.0, ACCENT);
    }
    resp
}


/// Panel top tab bar (full width, selected = accent underline). Returns true when changed.
pub fn tab_bar(ui: &mut Ui, sel: &mut usize, names: &[String]) -> bool {
    let w = ui.available_width();
    let n = names.len().max(1);
    let (rect, _) = ui.allocate_exact_size(vec2(w, 28.0), Sense::hover());
    let cw = w / n as f32;
    let mut changed = false;
    for (i, name) in names.iter().enumerate() {
        let r = Rect::from_min_size(pos2(rect.left() + i as f32 * cw, rect.top()), vec2(cw, rect.height()));
        let resp = ui.interact(r, ui.id().with(("tabbar", i)), Sense::click());
        let on = *sel == i;
        if resp.hovered() && !on {
            ui.painter().rect_filled(r.shrink2(vec2(2.0, 2.0)), 4.0, HOVER());
        }
        ui.painter().text(r.center(), Align2::CENTER_CENTER, name, FontId::proportional(12.0), if on { STRONG() } else { TEXT_WEAK() });
        if on {
            ui.painter().rect_filled(Rect::from_min_max(pos2(r.left() + 8.0, r.bottom() - 2.0), pos2(r.right() - 8.0, r.bottom())), 1.0, ACCENT);
        }
        if resp.clicked() && !on {
            *sel = i;
            changed = true;
        }
    }
    ui.painter().line_segment([pos2(rect.left(), rect.bottom() + 0.5), pos2(rect.right(), rect.bottom() + 0.5)], Stroke::new(1.0, BORDER()));
    ui.add_space(4.0);
    changed
}

/// Tool icon kinds (drawn by hand instead of font glyphs, for consistent size/weight and no missing glyphs in any font).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToolIcon {
    Crop,
    Mask,
    Picker,
    Privacy,
    Heal,
    RedEye,
    BeforeAfter,
    Clipping,
}

/// Icon button: selected = dark background + accent underline.
pub fn tool_button(ui: &mut Ui, icon: ToolIcon, selected: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(30.0, 26.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 5.0, ACTIVE());
        p.rect_filled(Rect::from_min_max(pos2(rect.left() + 8.0, rect.bottom() - 2.0), pos2(rect.right() - 8.0, rect.bottom())), 1.0, ACCENT);
    } else if resp.hovered() {
        p.rect_filled(rect, 5.0, HOVER());
    }
    let col = if selected || resp.hovered() { STRONG() } else { TEXT_WEAK() };
    let st = Stroke::new(1.4, col);
    let c = rect.center();
    let s = 7.0; // icon half-size
    match icon {
        ToolIcon::Crop => {
            // Two overlapping L shapes (crop)
            p.line_segment([c + vec2(-s + 2.0, -s), c + vec2(-s + 2.0, s - 2.0)], st);
            p.line_segment([c + vec2(-s + 2.0, s - 2.0), c + vec2(s, s - 2.0)], st);
            p.line_segment([c + vec2(s - 2.0, s), c + vec2(s - 2.0, -s + 2.0)], st);
            p.line_segment([c + vec2(s - 2.0, -s + 2.0), c + vec2(-s, -s + 2.0)], st);
        }
        ToolIcon::Mask => {
            // Half-filled circle (mask)
            p.circle_stroke(c, s, st);
            let mut pts = vec![c + vec2(0.0, -s)];
            for k in 0..=16 {
                let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * k as f32 / 16.0;
                pts.push(c + vec2(-a.cos(), a.sin()) * s);
            }
            p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
        }
        ToolIcon::Picker => {
            // Eyedropper: diagonal body + round head
            p.line_segment([c + vec2(-s, s), c + vec2(s * 0.35, -s * 0.35)], Stroke::new(2.0, col));
            p.circle_filled(c + vec2(s * 0.55, -s * 0.55), 2.8, col);
            p.circle_filled(c + vec2(-s, s), 1.3, col);
        }
        ToolIcon::RedEye => {
            // Eye outline + pupil
            let n = 18;
            let mut top = Vec::new();
            let mut bot = Vec::new();
            for k in 0..=n {
                let t = k as f32 / n as f32;
                let x = -s + 2.0 * s * t;
                let yv = (std::f32::consts::PI * t).sin() * s * 0.62;
                top.push(c + vec2(x, -yv));
                bot.push(c + vec2(x, yv));
            }
            p.add(egui::Shape::line(top, Stroke::new(1.3, col)));
            p.add(egui::Shape::line(bot, Stroke::new(1.3, col)));
            p.circle_filled(c, s * 0.34, col);
        }
        ToolIcon::Privacy => {
            // 3x3 mosaic (diagonal cells filled)
            let cell = s * 2.0 / 3.0;
            for r in 0..3 {
                for q in 0..3 {
                    let rr = Rect::from_min_size(c + vec2(-s + q as f32 * cell, -s + r as f32 * cell), vec2(cell - 1.0, cell - 1.0));
                    if (r + q) % 2 == 0 {
                        p.rect_filled(rr, 0.5, col);
                    } else {
                        p.rect_stroke(rr, 0.5, Stroke::new(0.8, col), egui::StrokeKind::Inside);
                    }
                }
            }
        }
        ToolIcon::Heal => {
            // Bandage: tilted rounded rectangle + dots in the middle
            let rot = |v: Vec2| {
                let (sn, cs) = (-0.785f32).sin_cos();
                vec2(v.x * cs - v.y * sn, v.x * sn + v.y * cs)
            };
            let corners = [vec2(-s, -3.2), vec2(s, -3.2), vec2(s, 3.2), vec2(-s, 3.2)];
            let pts: Vec<Pos2> = corners.iter().map(|v| c + rot(*v)).collect();
            p.add(egui::Shape::closed_line(pts, st));
            for v in [vec2(-1.3, -1.3), vec2(1.3, 1.3), vec2(-1.3, 1.3), vec2(1.3, -1.3)] {
                p.circle_filled(c + v, 0.8, col);
            }
        }
        ToolIcon::BeforeAfter => {
            // Split square (left half filled)
            let r = Rect::from_center_size(c, vec2(s * 2.0, s * 1.6));
            p.rect_stroke(r, 1.5, st, egui::StrokeKind::Middle);
            p.rect_filled(Rect::from_min_max(r.min, pos2(r.center().x, r.max.y)), 1.5, col);
        }
        ToolIcon::Clipping => {
            // Warning triangle
            let pts = vec![c + vec2(0.0, -s), c + vec2(s, s * 0.8), c + vec2(-s, s * 0.8)];
            p.add(egui::Shape::closed_line(pts, st));
            p.line_segment([c + vec2(0.0, -2.5), c + vec2(0.0, 2.0)], Stroke::new(1.4, col));
            p.circle_filled(c + vec2(0.0, 4.2), 0.9, col);
        }
    }
    resp.on_hover_text(tip)
}

/// List row (full width, left-aligned, selected = dark background + accent bar).
pub fn list_row(ui: &mut Ui, selected: bool, text: &str, col: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 4.0, ACTIVE());
        p.rect_filled(Rect::from_min_size(rect.left_top() + vec2(0.0, 5.0), vec2(2.5, 12.0)), 1.0, ACCENT);
    } else if resp.hovered() {
        p.rect_filled(rect, 4.0, HOVER());
    }
    let g = p.layout(text.to_string(), FontId::proportional(12.0), if selected { STRONG() } else { col }, rect.width() - 16.0);
    p.galley(pos2(rect.left() + 9.0, rect.center().y - g.size().y * 0.5), g, col);
    resp
}
