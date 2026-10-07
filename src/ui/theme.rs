//! Visual design: neutral dark theme with accent color #C32B21 and a dense layout.

use crate::config;
use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals};
use std::sync::Arc;

/// Theme (neutral colors only; the accent is always #C32B21)
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum ThemeKind {
    #[default]
    Dark,
    Midnight,
    Graphite,
    Light,
}

impl ThemeKind {
    pub const ALL: [ThemeKind; 4] = [ThemeKind::Dark, ThemeKind::Midnight, ThemeKind::Graphite, ThemeKind::Light];
    pub fn name(self) -> &'static str {
        match self {
            ThemeKind::Dark => tr!("다크", "Dark"),
            ThemeKind::Midnight => tr!("미드나잇 (OLED)", "Midnight (OLED)"),
            ThemeKind::Graphite => tr!("그래파이트", "Graphite"),
            ThemeKind::Light => tr!("라이트", "Light"),
        }
    }
    pub fn is_light(self) -> bool {
        self == ThemeKind::Light
    }
}

#[derive(Clone, Copy)]
struct Palette {
    bg: Color32,
    panel: Color32,
    panel2: Color32,
    widget: Color32,
    hover: Color32,
    active: Color32,
    border: Color32,
    text: Color32,
    text_weak: Color32,
    text_dim: Color32,
    canvas: Color32,
    select: Color32,
    extreme: Color32,
    strong: Color32,
}

const fn g(v: u8) -> Color32 {
    Color32::from_rgb(v, v, v)
}

fn palette_of(k: ThemeKind) -> Palette {
    match k {
        ThemeKind::Dark => Palette {
            bg: g(0x10), panel: g(0x17), panel2: g(0x1D), widget: g(0x26), hover: g(0x30), active: g(0x3A), border: g(0x2A),
            text: g(0xD6), text_weak: g(0x88), text_dim: g(0x5C), canvas: g(0x0B), select: g(0x4A), extreme: g(0x0E), strong: g(0xFF),
        },
        ThemeKind::Midnight => Palette {
            bg: g(0x00), panel: g(0x07), panel2: g(0x0D), widget: g(0x18), hover: g(0x22), active: g(0x2C), border: g(0x1A),
            text: g(0xCF), text_weak: g(0x80), text_dim: g(0x50), canvas: g(0x00), select: g(0x3A), extreme: g(0x03), strong: g(0xFF),
        },
        ThemeKind::Graphite => Palette {
            bg: g(0x26), panel: g(0x2E), panel2: g(0x35), widget: g(0x40), hover: g(0x4A), active: g(0x55), border: g(0x44),
            text: g(0xE4), text_weak: g(0xA0), text_dim: g(0x78), canvas: g(0x22), select: g(0x60), extreme: g(0x24), strong: g(0xFF),
        },
        ThemeKind::Light => Palette {
            bg: g(0xE9), panel: g(0xF3), panel2: g(0xFA), widget: g(0xDF), hover: g(0xD3), active: g(0xC6), border: g(0xD0),
            text: g(0x1E), text_weak: g(0x5E), text_dim: g(0x92), canvas: g(0x8A), select: g(0xBE), extreme: g(0xFF), strong: g(0x00),
        },
    }
}

static PAL: std::sync::RwLock<Option<(ThemeKind, Palette)>> = std::sync::RwLock::new(None);

fn pal() -> Palette {
    PAL.read().ok().and_then(|g| g.map(|x| x.1)).unwrap_or_else(|| palette_of(ThemeKind::Dark))
}

pub fn current_theme() -> ThemeKind {
    PAL.read().ok().and_then(|g| g.map(|x| x.0)).unwrap_or_default()
}

macro_rules! pal_fn {
    ($($name:ident => $f:ident),* $(,)?) => {
        $(
            #[allow(non_snake_case, dead_code)]
            #[inline]
            pub fn $name() -> Color32 {
                pal().$f
            }
        )*
    };
}

pal_fn!(BG => bg, PANEL => panel, PANEL2 => panel2, WIDGET => widget, HOVER => hover, ACTIVE => active, BORDER => border,
    TEXT => text, TEXT_WEAK => text_weak, TEXT_DIM => text_dim, CANVAS => canvas, SELECT => select, STRONG => strong);

/// Theme swatch colors: [background, panel, canvas, text]
pub fn preview_colors(k: ThemeKind) -> [Color32; 4] {
    let p = palette_of(k);
    [p.bg, p.panel2, p.canvas, p.text]
}

pub const ACCENT: Color32 = Color32::from_rgb(config::ACCENT[0], config::ACCENT[1], config::ACCENT[2]);

/// Apply the theme (palette + egui visual style)
pub fn apply_theme(ctx: &egui::Context, k: ThemeKind) {
    if let Ok(mut w) = PAL.write() {
        *w = Some((k, palette_of(k)));
    }
    let p = pal();
    let mut v = if k.is_light() { Visuals::light() } else { Visuals::dark() };
    v.panel_fill = p.panel;
    v.window_fill = p.panel2;
    v.extreme_bg_color = p.extreme;
    v.faint_bg_color = p.panel2;
    v.code_bg_color = p.widget;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(5);
    v.selection.bg_fill = p.select;
    v.selection.stroke = Stroke::new(1.0, p.text);
    v.hyperlink_color = p.text;
    v.override_text_color = None;
    let r = CornerRadius::same(4);
    v.widgets.noninteractive.bg_fill = p.panel;
    v.widgets.noninteractive.weak_bg_fill = p.panel;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text_weak);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.bg_fill = p.widget;
    v.widgets.inactive.weak_bg_fill = p.widget;
    v.widgets.inactive.bg_stroke = Stroke::NONE;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.bg_fill = p.hover;
    v.widgets.hovered.weak_bg_fill = p.hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.active);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.strong);
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.bg_fill = p.active;
    v.widgets.active.weak_bg_fill = p.active;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.text_weak);
    v.widgets.active.fg_stroke = Stroke::new(1.0, p.strong);
    v.widgets.active.corner_radius = r;
    v.widgets.open = v.widgets.active;
    v.striped = false;
    ctx.set_visuals(v);
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut ui_fonts = Vec::new();
    for (i, p) in config::UI_FONT_PATHS.iter().enumerate() {
        if let Ok(data) = std::fs::read(p) {
            let name = format!("ui{i}");
            fonts.font_data.insert(name.clone(), Arc::new(FontData::from_owned(data)));
            ui_fonts.push(name);
        }
    }
    let mut mono_fonts = Vec::new();
    for (i, p) in config::MONO_FONT_PATHS.iter().enumerate() {
        if let Ok(data) = std::fs::read(p) {
            let name = format!("mono{i}");
            fonts.font_data.insert(name.clone(), Arc::new(FontData::from_owned(data)));
            mono_fonts.push(name);
            break;
        }
    }
    if let Some(prop) = fonts.families.get_mut(&FontFamily::Proportional) {
        for (i, n) in ui_fonts.iter().enumerate() {
            prop.insert(i, n.clone());
        }
    }
    if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
        for (i, n) in mono_fonts.iter().enumerate() {
            mono.insert(i, n.clone());
        }
        // Korean fallback font
        for n in &ui_fonts {
            mono.push(n.clone());
        }
    }
    ctx.set_fonts(fonts);

    apply_theme(ctx, current_theme());

    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 4.0);
        s.spacing.button_padding = egui::vec2(8.0, 3.0);
        s.spacing.interact_size.y = 20.0;
        s.spacing.slider_width = 140.0;
        s.spacing.scroll.bar_width = 6.0;
        s.spacing.scroll.floating = true;
        s.text_styles.insert(TextStyle::Body, FontId::proportional(12.5));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(12.5));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(10.5));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(15.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(11.5));
        s.interaction.tooltip_delay = 0.35;
        // Label text is not drag-selectable; drags inside panels are for controls only
        s.interaction.selectable_labels = false;
    });
}

pub fn mono(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into()).monospace()
}
