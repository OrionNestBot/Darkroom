//! Develop view layout: cards (photo, presets, history, snapshots, sync, histogram, adjustments) can be dragged to rearrange.
//! Limits: up to 2 left columns (default 1), 1 right column, 1 bottom row. Column widths and bottom height are resized by dragging edges and saved.

use super::app::*;
use super::theme::*;
use super::widgets::Edit;
use egui::{Align2, Color32, FontId, Id, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Card {
    Film,
    Presets,
    History,
    Snapshots,
    Sync,
    Histogram,
    Adjust,
}

impl Card {
    pub const ALL: [Card; 7] = [Card::Film, Card::Presets, Card::History, Card::Snapshots, Card::Sync, Card::Histogram, Card::Adjust];
    pub fn title(self) -> &'static str {
        match self {
            Card::Film => tr!("사진", "Photos"),
            Card::Presets => tr!("프리셋", "Presets"),
            Card::History => tr!("히스토리", "History"),
            Card::Snapshots => tr!("스냅샷", "Snapshots"),
            Card::Sync => tr!("복사 · 동기화", "Copy · Sync"),
            Card::Histogram => tr!("히스토그램", "Histogram"),
            Card::Adjust => tr!("현상", "Develop"),
        }
    }
    /// Share of the leftover height (width) this card takes. 0 = fixed to content size.
    fn weight(self) -> f32 {
        match self {
            Card::Film => 3.0,
            Card::Adjust => 5.0,
            Card::Presets | Card::History => 2.0,
            Card::Snapshots => 1.0,
            Card::Sync | Card::Histogram => 0.0,
        }
    }
    /// Content height of fixed cards.
    fn fixed_h(self) -> f32 {
        match self {
            Card::Sync => 64.0,
            Card::Histogram => 104.0,
            _ => 0.0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Loc {
    Left(usize),
    Right,
    Bottom,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DevLayout {
    pub left: Vec<Vec<Card>>,
    pub right: Vec<Card>,
    pub bottom: Vec<Card>,
    pub hidden: Vec<Card>,
    pub collapsed: Vec<Card>,
    /// Widths of the first left column, second left column and right column, plus bottom height (points).
    pub col_w: f32,
    pub col2_w: f32,
    pub right_w: f32,
    pub bottom_h: f32,
    /// Layout lock: blocks dragging cards and resizing (collapsing/expanding still works).
    pub locked: bool,
    /// Per-card height (width) ratios, set by dragging the edge between cards.
    pub weights: Vec<(Card, f32)>,
}

impl Default for DevLayout {
    fn default() -> Self {
        Self {
            left: vec![vec![Card::Film, Card::Presets, Card::History, Card::Snapshots, Card::Sync]],
            right: vec![Card::Histogram, Card::Adjust],
            bottom: vec![],
            hidden: vec![],
            collapsed: vec![Card::Presets, Card::Snapshots],
            col_w: 250.0,
            col2_w: 250.0,
            right_w: 340.0,
            bottom_h: 170.0,
            locked: false,
            weights: Vec::new(),
        }
    }
}

impl DevLayout {
    /// Height ratio of a card (user-set, or the default).
    pub fn weight_of(&self, c: Card) -> f32 {
        if c.weight() <= 0.0 {
            return 0.0;
        }
        self.weights.iter().find(|(k, _)| *k == c).map(|(_, w)| *w).unwrap_or(c.weight())
    }
    fn set_weight(&mut self, c: Card, w: f32) {
        match self.weights.iter_mut().find(|(k, _)| *k == c) {
            Some(x) => x.1 = w,
            None => self.weights.push((c, w)),
        }
    }

    /// Ensures every card appears exactly once, removes empty columns, caps left columns at 2.
    pub fn normalize(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let mut keep = |v: &mut Vec<Card>| v.retain(|c| seen.insert(*c));
        for col in &mut self.left {
            keep(col);
        }
        keep(&mut self.right);
        keep(&mut self.bottom);
        keep(&mut self.hidden);
        while self.left.len() > 2 {
            let extra = self.left.pop().unwrap();
            self.left[1].extend(extra);
        }
        self.left.retain(|c| !c.is_empty());
        if self.left.is_empty() {
            self.left.push(Vec::new());
        }
        for c in Card::ALL {
            let present = self.left.iter().any(|col| col.contains(&c)) || self.right.contains(&c) || self.bottom.contains(&c) || self.hidden.contains(&c);
            if !present {
                self.left[0].push(c);
            }
        }
        self.col_w = self.col_w.clamp(180.0, 440.0);
        self.col2_w = self.col2_w.clamp(180.0, 440.0);
        self.right_w = self.right_w.clamp(260.0, 560.0);
        self.bottom_h = self.bottom_h.clamp(90.0, 420.0);
    }

    pub fn loc_of(&self, c: Card) -> Option<Loc> {
        for (i, col) in self.left.iter().enumerate() {
            if col.contains(&c) {
                return Some(Loc::Left(i));
            }
        }
        if self.right.contains(&c) {
            return Some(Loc::Right);
        }
        if self.bottom.contains(&c) {
            return Some(Loc::Bottom);
        }
        None
    }

    fn remove(&mut self, c: Card) {
        for col in &mut self.left {
            col.retain(|x| *x != c);
        }
        self.right.retain(|x| *x != c);
        self.bottom.retain(|x| *x != c);
        self.hidden.retain(|x| *x != c);
    }

    /// Inserts a card at a location and index (Left(n) equal to the current column count creates a new column).
    pub fn place(&mut self, c: Card, loc: Loc, idx: usize) {
        self.remove(c);
        match loc {
            Loc::Left(i) => {
                if i >= self.left.len() {
                    self.left.push(Vec::new());
                }
                let i = i.min(self.left.len() - 1);
                let col = &mut self.left[i];
                col.insert(idx.min(col.len()), c);
            }
            Loc::Right => self.right.insert(idx.min(self.right.len()), c),
            Loc::Bottom => self.bottom.insert(idx.min(self.bottom.len()), c),
        }
        self.normalize();
    }

    pub fn hide(&mut self, c: Card) {
        self.remove(c);
        self.hidden.push(c);
        self.normalize();
    }

    /// Maps the filmstrip position setting (F6 / preferences) to the photo card location.
    pub fn set_film(&mut self, pos: FilmPos) {
        match pos {
            FilmPos::Left => self.place(Card::Film, Loc::Left(0), 0),
            FilmPos::Bottom => self.place(Card::Film, Loc::Bottom, 0),
            FilmPos::Hidden => self.hide(Card::Film),
        }
    }

    pub fn film_pos(&self) -> FilmPos {
        match self.loc_of(Card::Film) {
            Some(Loc::Bottom) => FilmPos::Bottom,
            Some(_) => FilmPos::Left,
            None => FilmPos::Hidden,
        }
    }
}

/// Drag state (temporary memory between frames).
#[derive(Clone, Copy, Debug)]
struct DragState {
    card: Card,
    /// Drop target (computed last frame).
    target: Option<(Loc, usize)>,
}

fn drag_id() -> Id {
    Id::new("devlayout_drag")
}

/// All develop view panels (left, right, bottom). Collects edit results into edit/label.
pub fn panels(app: &mut App, ui: &mut Ui, edit: &mut Edit, label: &mut String) {
    app.prefs.dev_layout.normalize();
    let ctx = ui.ctx().clone();
    let mut drag: Option<DragState> = ctx.data(|d| d.get_temp(drag_id()));
    // Drop: when the drag button is released, move to the slot computed last frame
    if let Some(ds) = drag
        && !ctx.input(|i| i.pointer.primary_down()) {
            if let Some((loc, idx)) = ds.target {
                app.prefs.dev_layout.place(ds.card, loc, idx);
                app.prefs.film_pos = app.prefs.dev_layout.film_pos();
                app.save_prefs();
            }
            drag = None;
            ctx.data_mut(|d| d.remove::<DragState>(drag_id()));
        }
    let mut new_target: Option<(Loc, usize)> = None;
    let pointer = ctx.pointer_latest_pos();
    let lay = app.prefs.dev_layout.clone();
    let dragging = drag.map(|d| d.card);
    let show_left = app.prefs.show_left;
    let show_right = app.prefs.show_right;

    // ── Left ──
    let left_cols = lay.left.iter().filter(|c| !c.is_empty()).count();
    let extra_slot = dragging.is_some() && lay.left.len() < 2 && show_left && !lay.locked;
    if show_left && (left_cols > 0 || dragging.is_some()) {
        let ncols = left_cols.max(1);
        // Width saved per column
        let widths: Vec<f32> = (0..ncols).map(|i| if i == 0 { lay.col_w } else { lay.col2_w }).collect();
        const SEP: f32 = 9.0;
        let w = widths.iter().sum::<f32>() + SEP * (ncols - 1) as f32 + if extra_slot { 70.0 } else { 0.0 };
        let mut col_rects: Vec<Rect> = Vec::new();
        let pr = egui::Panel::left("dev_left_cards")
            .exact_size(w)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin::symmetric(8, 6)))
            .show(ui, |ui| {
                let full = ui.max_rect();
                // Scale down by the inner margin to fit
                let usable = full.width() - if extra_slot { 70.0 } else { 0.0 } - SEP * (ncols - 1) as f32;
                let total: f32 = widths.iter().sum();
                let mut x = full.left();
                for ci in 0..ncols {
                    let cw = usable * widths[ci] / total.max(1.0);
                    let r = Rect::from_min_size(pos2(x, full.top()), vec2(cw, full.height()));
                    col_rects.push(r);
                    let cards = lay.left.get(ci).cloned().unwrap_or_default();
                    column(app, ui, r, &cards, Loc::Left(ci), dragging, pointer, &mut new_target, edit, label);
                    x += cw + SEP;
                }
                if extra_slot {
                    let r = Rect::from_min_max(pos2(full.right() - 64.0, full.top()), full.max);
                    drop_slot(ui, r, tr!("새 열", "New column"), Loc::Left(lay.left.len()), dragging, pointer, &mut new_target);
                }
            });
        // Edge between columns: only the first column width changes (the panel grows or shrinks by that much)
        if ncols == 2 && col_rects.len() == 2 {
            let x = (col_rects[0].right() + col_rects[1].left()) * 0.5;
            let div = Rect::from_min_max(pos2(x - 4.0, col_rects[0].top()), pos2(x + 3.0, col_rects[0].bottom()));
            ui.painter().line_segment([pos2(x, div.top()), pos2(x, div.bottom())], Stroke::new(1.0, BORDER()));
            if !lay.locked {
                resize_handle(ui, div, Side::Right, "devl_resize_col1", |dx| {
                    app.prefs.dev_layout.col_w = (app.prefs.dev_layout.col_w + dx).clamp(180.0, 440.0);
                });
            }
        }
        // Outer edge: last column width
        if !lay.locked {
        resize_handle(ui, pr.response.rect, Side::Right, "devl_resize_left", |dx| {
            let l = &mut app.prefs.dev_layout;
            if ncols == 2 {
                l.col2_w = (l.col2_w + dx).clamp(180.0, 440.0);
            } else {
                l.col_w = (l.col_w + dx).clamp(180.0, 440.0);
            }
        });
        }
    }

    // ── Right ──
    if show_right && (!lay.right.is_empty() || dragging.is_some()) {
        let pr = egui::Panel::right("dev_right_cards")
            .exact_size(lay.right_w)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ui, |ui| {
                let r = ui.max_rect();
                if lay.right.is_empty() {
                    drop_slot(ui, r, tr!("오른쪽", "Right"), Loc::Right, dragging, pointer, &mut new_target);
                } else {
                    column(app, ui, r, &lay.right, Loc::Right, dragging, pointer, &mut new_target, edit, label);
                }
            });
        ui.ctx().data_mut(|d| d.insert_temp(Id::new("dev_right_w"), pr.response.rect.width()));
        if !lay.locked {
            resize_handle(ui, pr.response.rect, Side::Left, "devl_resize_right", |dx| {
                app.prefs.dev_layout.right_w = (app.prefs.dev_layout.right_w - dx).clamp(260.0, 560.0);
            });
        }
    }

    // ── Bottom ──
    if !lay.bottom.is_empty() || dragging.is_some() {
        let h = if lay.bottom.is_empty() { 56.0 } else { lay.bottom_h };
        let pr = egui::Panel::bottom("dev_bottom_cards")
            .exact_size(h)
            .resizable(false)
            .frame(egui::Frame::new().fill(PANEL()).inner_margin(egui::Margin::symmetric(8, 6)))
            .show(ui, |ui| {
                let r = ui.max_rect();
                if lay.bottom.is_empty() {
                    drop_slot(ui, r, tr!("아래 줄", "Bottom row"), Loc::Bottom, dragging, pointer, &mut new_target);
                } else {
                    row(app, ui, r, &lay.bottom, dragging, pointer, &mut new_target, edit, label);
                }
            });
        if !lay.bottom.is_empty() && !lay.locked {
            resize_handle(ui, pr.response.rect, Side::Top, "devl_resize_bottom", |dy| {
                app.prefs.dev_layout.bottom_h = (app.prefs.dev_layout.bottom_h - dy).clamp(90.0, 420.0);
            });
        }
    }

    // While dragging: draw a ghost and remember the drop target
    if let Some(mut ds) = drag {
        ds.target = new_target;
        ctx.data_mut(|d| d.insert_temp(drag_id(), ds));
        if let Some(p) = pointer {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("devl_ghost")));
            let g = painter.layout_no_wrap(ds.card.title().to_string(), FontId::proportional(12.0), STRONG());
            let r = Rect::from_min_size(p + vec2(12.0, 10.0), g.size() + vec2(18.0, 10.0));
            painter.rect_filled(r, 6.0, WIDGET());
            painter.rect_stroke(r, 6.0, Stroke::new(1.0, ACCENT), egui::StrokeKind::Inside);
            painter.galley(r.min + vec2(9.0, 5.0), g, STRONG());
        }
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        ctx.request_repaint();
    }
}

#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
    Top,
}

/// Resizes a panel by dragging its edge.
fn resize_handle(ui: &mut Ui, panel: Rect, side: Side, id: &str, mut apply: impl FnMut(f32)) {
    // Grab strip only inside the panel (the photo area takes input on the outer half)
    let r = match side {
        Side::Right => Rect::from_min_max(pos2(panel.right() - 7.0, panel.top()), pos2(panel.right(), panel.bottom())),
        Side::Left => Rect::from_min_max(pos2(panel.left(), panel.top()), pos2(panel.left() + 7.0, panel.bottom())),
        Side::Top => Rect::from_min_max(pos2(panel.left(), panel.top()), pos2(panel.right(), panel.top() + 7.0)),
    };
    let resp = ui.interact(r, Id::new(id), Sense::drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(if matches!(side, Side::Top) { egui::CursorIcon::ResizeVertical } else { egui::CursorIcon::ResizeHorizontal });
        ui.painter().rect_filled(r.shrink2(vec2(if matches!(side, Side::Top) { 0.0 } else { 1.5 }, if matches!(side, Side::Top) { 1.5 } else { 0.0 })), 1.0, ACCENT.gamma_multiply(0.6));
    }
    if resp.dragged() {
        let d = resp.drag_delta();
        apply(if matches!(side, Side::Top) { d.y } else { d.x });
    }
}

/// Drop slot in an empty area.
fn drop_slot(ui: &mut Ui, r: Rect, text: &str, loc: Loc, dragging: Option<Card>, pointer: Option<Pos2>, target: &mut Option<(Loc, usize)>) {
    if dragging.is_none() {
        return;
    }
    let hot = pointer.map(|p| r.contains(p)).unwrap_or(false);
    if hot {
        *target = Some((loc, 0));
    }
    let p = ui.painter();
    p.rect_filled(r.shrink(2.0), 6.0, if hot { ACCENT.gamma_multiply(0.18) } else { WIDGET().gamma_multiply(0.6) });
    p.rect_stroke(r.shrink(2.0), 6.0, Stroke::new(1.0, if hot { ACCENT } else { BORDER() }), egui::StrokeKind::Inside);
    p.text(r.center(), Align2::CENTER_CENTER, text, FontId::proportional(11.5), if hot { STRONG() } else { TEXT_WEAK() });
}

/// Vertical column: stacks cards and splits leftover height by ratio.
#[allow(clippy::too_many_arguments)]
fn column(app: &mut App, ui: &mut Ui, r: Rect, cards: &[Card], loc: Loc, dragging: Option<Card>, pointer: Option<Pos2>, target: &mut Option<(Loc, usize)>, edit: &mut Edit, label: &mut String) {
    const HEAD: f32 = 26.0;
    const GAP: f32 = 6.0;
    let collapsed = app.prefs.dev_layout.collapsed.clone();
    let locked = app.prefs.dev_layout.locked;
    let lay = app.prefs.dev_layout.clone();
    let is_open = |c: &Card| !collapsed.contains(c);
    let fixed: f32 = cards.iter().map(|c| HEAD + GAP + if is_open(c) { c.fixed_h() } else { 0.0 }).sum();
    let wsum: f32 = cards.iter().filter(|c| is_open(c)).map(|c| lay.weight_of(*c)).sum();
    let free = (r.height() - fixed).max(0.0);
    // Stretchable (open) cards; dragging the edge between neighbors adjusts their height ratio
    let flex: Vec<Card> = cards.iter().copied().filter(|c| is_open(c) && lay.weight_of(*c) > 0.0).collect();
    let mut y = r.top();
    let mut heads: Vec<Rect> = Vec::new();
    for c in cards {
        let open = is_open(c);
        let wc = lay.weight_of(*c);
        let body_h = if !open {
            0.0
        } else if wc > 0.0 && wsum > 0.0 {
            (free * wc / wsum).max(60.0)
        } else {
            c.fixed_h()
        };
        let head = Rect::from_min_size(pos2(r.left(), y), vec2(r.width(), HEAD));
        heads.push(head);
        card_header(app, ui, head, *c, open, loc);
        y += HEAD;
        if open && body_h > 0.0 {
            let body = Rect::from_min_size(pos2(r.left(), y), vec2(r.width(), body_h));
            card_body(app, ui, body, *c, false, edit, label);
            y += body_h;
            // Edge between this card and the next stretchable card (gap below the card)
            let fi = flex.iter().position(|x| x == c);
            if let (false, Some(fi)) = (locked, fi)
                && let Some(next) = flex.get(fi + 1).copied() {
                    let hr = Rect::from_min_max(pos2(r.left(), y - 1.0), pos2(r.right(), y + GAP + 1.0));
                    let resp = ui.interact(hr, Id::new(("card_split", *c as u8)), Sense::drag());
                    if resp.hovered() || resp.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                        ui.painter().rect_filled(hr.shrink2(vec2(0.0, 2.0)), 1.0, ACCENT.gamma_multiply(0.6));
                    }
                    if resp.dragged() && free > 0.0 {
                        let dy = resp.drag_delta().y;
                        let l = &mut app.prefs.dev_layout;
                        let (a, b) = (l.weight_of(*c), l.weight_of(next));
                        let dw = dy / free * wsum;
                        let min = wsum * 70.0 / free.max(1.0);
                        let na = (a + dw).max(min);
                        let nb = (a + b - na).max(min);
                        let na = a + b - nb;
                        l.set_weight(*c, na);
                        l.set_weight(next, nb);
                    }
                }
        }
        y += GAP;
    }
    // Drop slot indicator: insertion index found by comparing with header midlines
    if let (Some(dc), Some(p)) = (dragging, pointer)
        && r.expand2(vec2(0.0, 4.0)).contains(p) {
            let idx = heads.iter().position(|h| p.y < h.center().y).unwrap_or(cards.len());
            *target = Some((loc, idx));
            let ly = if idx < heads.len() { heads[idx].top() - GAP * 0.5 } else { y.min(r.bottom() - 2.0) };
            let p = ui.painter();
            p.line_segment([pos2(r.left(), ly), pos2(r.right(), ly)], Stroke::new(2.5, ACCENT));
            p.circle_filled(pos2(r.left() + 3.0, ly), 3.5, ACCENT);
            let _ = dc;
        }
}

/// Bottom row: cards side by side, widths split by ratio.
#[allow(clippy::too_many_arguments)]
fn row(app: &mut App, ui: &mut Ui, r: Rect, cards: &[Card], dragging: Option<Card>, pointer: Option<Pos2>, target: &mut Option<(Loc, usize)>, edit: &mut Edit, label: &mut String) {
    const HEAD: f32 = 24.0;
    const GAP: f32 = 8.0;
    let wsum: f32 = cards.iter().map(|c| c.weight().max(1.0)).sum();
    let free = r.width() - GAP * (cards.len().saturating_sub(1)) as f32;
    let mut x = r.left();
    let mut slots: Vec<Rect> = Vec::new();
    let collapsed = app.prefs.dev_layout.collapsed.clone();
    for c in cards {
        let w = free * c.weight().max(1.0) / wsum;
        let slot = Rect::from_min_size(pos2(x, r.top()), vec2(w, r.height()));
        slots.push(slot);
        let head = Rect::from_min_size(slot.min, vec2(w, HEAD));
        let open = !collapsed.contains(c);
        card_header(app, ui, head, *c, open, Loc::Bottom);
        if open {
            let body = Rect::from_min_max(pos2(slot.left(), slot.top() + HEAD), slot.max);
            card_body(app, ui, body, *c, true, edit, label);
        }
        x += w + GAP;
    }
    if let (Some(_), Some(p)) = (dragging, pointer)
        && r.contains(p) {
            let idx = slots.iter().position(|s| p.x < s.center().x).unwrap_or(cards.len());
            *target = Some((Loc::Bottom, idx));
            let lx = if idx < slots.len() { slots[idx].left() - GAP * 0.5 } else { r.right() - 2.0 };
            ui.painter().line_segment([pos2(lx, r.top()), pos2(lx, r.bottom())], Stroke::new(2.5, ACCENT));
        }
}

/// Card header: grip dots + title + collapse. Drag to rearrange; right-click menu.
fn card_header(app: &mut App, ui: &mut Ui, r: Rect, c: Card, open: bool, loc: Loc) {
    let locked = app.prefs.dev_layout.locked;
    let resp = ui.interact(r, Id::new(("card_head", c as u8)), if locked { Sense::click() } else { Sense::click_and_drag() });
    // Store the header rect for automated UI checks
    ui.ctx().data_mut(|d| d.insert_temp(Id::new(("card_head_rect", c as u8)), r));
    let p = ui.painter();
    if resp.hovered() || resp.dragged() {
        p.rect_filled(r, 4.0, HOVER());
    }
    if locked {
        // Padlock (when locked): body + shackle
        let c0 = pos2(r.left() + 9.0, r.center().y + 1.5);
        p.rect_filled(Rect::from_center_size(c0, vec2(8.0, 6.0)), 1.0, TEXT_DIM());
        p.circle_stroke(pos2(c0.x, c0.y - 3.5), 2.6, Stroke::new(1.2, TEXT_DIM()));
    } else {
        // Grip dots (2x3)
        for k in 0..6 {
            let (cx, cy) = (r.left() + 7.0 + (k % 2) as f32 * 4.0, r.center().y - 4.0 + (k / 2) as f32 * 4.0);
            p.circle_filled(pos2(cx, cy), 1.1, if resp.hovered() { TEXT_WEAK() } else { TEXT_DIM() });
        }
    }
    let title = if c == Card::Film { format!("{} {}", c.title(), app.visible.len()) } else { c.title().to_string() };
    p.text(pos2(r.left() + 20.0, r.center().y), Align2::LEFT_CENTER, title, FontId::proportional(12.0), if open { TEXT() } else { TEXT_WEAK() });
    p.text(pos2(r.right() - 8.0, r.center().y), Align2::RIGHT_CENTER, if open { "−" } else { "+" }, FontId::monospace(12.0), TEXT_DIM());
    if !resp.dragged() {
        p.line_segment([pos2(r.left(), r.bottom()), pos2(r.right(), r.bottom())], Stroke::new(1.0, BORDER()));
    }
    if resp.clicked() {
        let l = &mut app.prefs.dev_layout;
        if open {
            l.collapsed.push(c);
        } else {
            l.collapsed.retain(|x| *x != c);
        }
        app.save_prefs();
    }
    if resp.drag_started() && !locked {
        ui.ctx().data_mut(|d| d.insert_temp(drag_id(), DragState { card: c, target: None }));
    }
    let resp = resp.on_hover_text(if locked { tr!("클릭: 접기/펼치기 · 배치 잠김 (우클릭: 잠금 풀기)", "Click: collapse/expand · layout locked (right-click: unlock)") } else { tr!("클릭: 접기/펼치기 · 끌기: 위치 옮기기 · 경계 끌기: 크기 · 우클릭: 메뉴", "Click: collapse/expand · drag: move · drag edge: resize · right-click: menu") });
    resp.context_menu(|ui| {
        let mut moved = None;
        if ui.button(if open { tr!("접기", "Collapse") } else { tr!("펼치기", "Expand") }).clicked() {
            let l = &mut app.prefs.dev_layout;
            if open {
                l.collapsed.push(c);
            } else {
                l.collapsed.retain(|x| *x != c);
            }
            ui.close();
        }
        if ui.button(if locked { tr!("배치 잠금 풀기", "Unlock layout") } else { tr!("배치 잠그기 (위치·폭·높이 고정)", "Lock layout (fix position·width·height)") }).clicked() {
            app.prefs.dev_layout.locked = !locked;
            ui.close();
        }
        ui.separator();
        let ncols = app.prefs.dev_layout.left.len();
        if locked {
            ui.label(egui::RichText::new(tr!("잠금 중에는 옮길 수 없습니다", "Can't move while locked")).size(11.0).color(TEXT_DIM()));
            return;
        }
        if !matches!(loc, Loc::Left(_)) && ui.button(tr!("왼쪽으로", "To the left")).clicked() {
            moved = Some(Loc::Left(0));
        }
        if matches!(loc, Loc::Left(0)) && ncols < 2 && ui.button(tr!("왼쪽 둘째 열로", "To left column 2")).clicked() {
            moved = Some(Loc::Left(1));
        }
        if matches!(loc, Loc::Left(1)) && ui.button(tr!("왼쪽 첫째 열로", "To left column 1")).clicked() {
            moved = Some(Loc::Left(0));
        }
        if loc != Loc::Right && ui.button(tr!("오른쪽으로", "To the right")).clicked() {
            moved = Some(Loc::Right);
        }
        if loc != Loc::Bottom && ui.button(tr!("아래 줄로", "To bottom row")).clicked() {
            moved = Some(Loc::Bottom);
        }
        if c != Card::Adjust && ui.button(tr!("숨기기", "Hide")).clicked() {
            app.prefs.dev_layout.hide(c);
            ui.close();
        }
        let hidden = app.prefs.dev_layout.hidden.clone();
        if !hidden.is_empty() {
            ui.menu_button(tr!("숨긴 카드 보이기", "Show hidden cards"), |ui| {
                for h in hidden {
                    if ui.button(h.title()).clicked() {
                        app.prefs.dev_layout.place(h, loc, usize::MAX);
                        ui.close();
                    }
                }
            });
        }
        ui.separator();
        if ui.button(tr!("기본 배치로", "Default layout")).clicked() {
            app.prefs.dev_layout = DevLayout::default();
            ui.close();
        }
        if !app.prefs.dev_layout.weights.is_empty() && ui.button(tr!("카드 높이 기본으로", "Default card heights")).clicked() {
            app.prefs.dev_layout.weights.clear();
            ui.close();
        }
        if let Some(to) = moved {
            app.prefs.dev_layout.place(c, to, usize::MAX);
            ui.close();
        }
        app.prefs.film_pos = app.prefs.dev_layout.film_pos();
    });
}

/// Card body (clipped to rect).
fn card_body(app: &mut App, ui: &mut Ui, body: Rect, c: Card, horizontal: bool, edit: &mut Edit, label: &mut String) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body.shrink2(vec2(2.0, 4.0))).layout(egui::Layout::top_down(egui::Align::Min)));
    child.set_clip_rect(body.intersect(ui.clip_rect()));
    let ui = &mut child;
    match c {
        Card::Film => {
            if horizontal {
                super::library::film_horizontal(app, ui)
            } else {
                super::library::film_vertical(app, ui)
            }
        }
        Card::Presets => {
            egui::ScrollArea::vertical().id_salt("card_presets").auto_shrink([false, false]).show(ui, |ui| super::develop::presets(app, ui));
        }
        Card::History => super::develop::history(app, ui),
        Card::Snapshots => {
            egui::ScrollArea::vertical().id_salt("card_snaps").auto_shrink([false, false]).show(ui, |ui| super::develop::snapshots(app, ui));
        }
        Card::Sync => super::develop::sync_controls(app, ui),
        Card::Histogram => {
            let (e, l) = super::develop::histogram_card(app, ui);
            if e.changed || e.committed {
                edit.merge(e);
                *label = l;
            }
        }
        Card::Adjust => {
            super::develop::tool_strip(app, ui);
            ui.add_space(4.0);
            egui::ScrollArea::vertical().id_salt("dev_right_scroll").auto_shrink([false, false]).show(ui, |ui| {
                let (e, l) = super::develop::right_panel(app, ui);
                if e.changed || e.committed || e.active {
                    edit.merge(e);
                    *label = l;
                }
            });
        }
    }
    let _ = Color32::TRANSPARENT;
}
