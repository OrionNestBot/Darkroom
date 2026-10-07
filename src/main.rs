//! Darkroom: RAW photo management and development.
//! © 2026 OrionNest
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[macro_use]
mod i18n;
mod catalog;
mod catalogs;
mod config;
mod develop;
mod export;
mod fonts;
mod imaging;
mod lrcat;
mod lrpreset;
mod xmpwrite;
mod ui;

fn main() -> eframe::Result {
    // Give image-processing threads extra stack (large RAW files + deeply nested rayon work)
    let _ = rayon::ThreadPoolBuilder::new()
        .stack_size(config::WORKER_STACK)
        .thread_name(|i| format!("rayon{i}"))
        .build_global();
    imaging::decode::init_decoder();
    // UI language (Preferences > General). Test switch: DARKROOM_LANG=en forces English.
    let lang = std::env::var("DARKROOM_LANG").unwrap_or_else(|_| catalogs::Registry::load().lang);
    i18n::set_en(lang == "en");
    let icon = image::load_from_memory(config::ICON_PNG).ok().map(|i| {
        let rgba = i.into_rgba8();
        let (w, h) = rgba.dimensions();
        egui::IconData { rgba: rgba.into_raw(), width: w, height: h }
    });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(config::APP_NAME)
        .with_inner_size([1600.0, 1000.0])
        .with_min_inner_size([1000.0, 640.0])
        .with_maximized(true);
    if let Some(i) = icon {
        viewport = viewport.with_icon(i);
    }
    let opts = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native(config::APP_NAME, opts, Box::new(|cc| Ok(Box::new(ui::app::App::new(cc)))))
}
