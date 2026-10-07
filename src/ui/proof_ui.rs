//! Soft proofing in develop (S): preview print output with a printer/output profile.
//! © 2026 OrionNest

use super::app::*;
use super::theme::*;
use crate::export::proof::{self, Intent, ProfileInfo};
use egui::Ui;
use std::path::PathBuf;

#[derive(Default)]
pub struct ProofUi {
    pub on: bool,
    lut_id: u32,
    built: Option<(PathBuf, Intent, bool)>,
    profiles: Option<Vec<ProfileInfo>>,
}

fn profiles(app: &mut App) -> &Vec<ProfileInfo> {
    if app.proof.profiles.is_none() {
        let mut v = proof::installed_profiles();
        // Also list a previously chosen external profile file
        let cur = PathBuf::from(&app.prefs.proof_profile);
        if !app.prefs.proof_profile.is_empty() && !v.iter().any(|p| p.path == cur)
            && let Some(i) = proof::describe(&cur) {
                v.push(i);
            }
        app.proof.profiles = Some(v);
    }
    app.proof.profiles.as_ref().unwrap()
}

pub fn toggle(app: &mut App) {
    if app.proof.on {
        app.proof.on = false;
        return;
    }
    if app.prefs.proof_profile.is_empty() || !std::path::Path::new(&app.prefs.proof_profile).exists() {
        let first = profiles(app).first().map(|p| p.path.clone());
        match first {
            Some(p) => app.prefs.proof_profile = p.to_string_lossy().to_string(),
            None => {
                if !pick_file(app) {
                    app.toast(tr!("교정할 프린터 프로필(.icc/.icm)을 고르세요", "Choose a printer profile (.icc/.icm) to proof"));
                    return;
                }
            }
        }
    }
    app.proof.on = true;
}

fn pick_file(app: &mut App) -> bool {
    let Some(f) = rfd::FileDialog::new().add_filter(tr!("ICC 프로필", "ICC profile"), &["icc", "icm"]).pick_file() else { return false };
    match proof::describe(&f) {
        Some(info) => {
            app.prefs.proof_profile = f.to_string_lossy().to_string();
            let list = app.proof.profiles.get_or_insert_with(Vec::new);
            if !list.iter().any(|p| p.path == info.path) {
                list.push(info);
            }
            true
        }
        None => {
            app.toast_err(tr!("프린터·출력용 RGB/CMYK 프로필이 아닙니다 (화면 프로필은 쓸 수 없음)", "Not an RGB/CMYK printer or output profile (display profiles can't be used)"));
            false
        }
    }
}

/// Proof index to apply to the preview (0 = off). Rebuilds the LUT when the settings changed.
pub fn view_value(app: &mut App) -> u32 {
    if !app.proof.on {
        return 0;
    }
    let key = (PathBuf::from(&app.prefs.proof_profile), app.prefs.proof_intent, app.prefs.proof_paper);
    if app.proof.built.as_ref() != Some(&key) {
        match proof::build(&key.0, key.1, key.2) {
            Ok(lut) => {
                app.proof.lut_id += 1;
                proof::install(app.proof.lut_id, std::sync::Arc::new(lut));
                app.proof.built = Some(key);
            }
            Err(e) => {
                app.proof.on = false;
                app.proof.built = None;
                app.toast_err(trf!("교정 프로필을 쓸 수 없습니다: {e:#}", "Can't use proof profile: {e:#}"));
                return 0;
            }
        }
    }
    proof::encode(app.proof.lut_id, app.prefs.proof_gamut)
}

pub fn profile_name(app: &mut App) -> String {
    let cur = PathBuf::from(&app.prefs.proof_profile);
    profiles(app).iter().find(|p| p.path == cur).map(|p| p.name.clone()).unwrap_or_else(|| cur.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
}

/// Proof settings attached to the bottom bar (only while enabled).
pub fn controls(app: &mut App, ui: &mut Ui) {
    if !app.proof.on {
        return;
    }
    let name = profile_name(app);
    let list: Vec<(PathBuf, String, String)> = profiles(app).iter().map(|p| (p.path.clone(), p.name.clone(), format!("{} {}", p.kind, if p.cmyk { "CMYK" } else { "RGB" }))).collect();
    let mut pick = false;
    egui::ComboBox::from_id_salt("proof_profile")
        .width(190.0)
        .selected_text(egui::RichText::new(&name).size(11.5))
        .show_ui(ui, |ui| {
            for (path, n, kind) in &list {
                let sel = path.to_string_lossy() == app.prefs.proof_profile;
                if ui.selectable_label(sel, format!("{n}  ·  {kind}")).clicked() {
                    app.prefs.proof_profile = path.to_string_lossy().to_string();
                }
            }
            ui.separator();
            if ui.button(tr!("파일에서 고르기…", "Choose file…")).clicked() {
                pick = true;
            }
        })
        .response
        .on_hover_text(tr!("교정할 프린터·출력 프로필 (Windows 색 폴더의 프로필 + 직접 고른 파일)", "Printer·output profile to proof (profiles in the Windows color folder + files you picked)"));
    if pick {
        pick_file(app);
    }
    for (v, l, tip) in [(Intent::Perceptual, tr!("지각", "Perceptual"), tr!("영역 밖 색을 전체적으로 눌러 담음 (사진에 무난)", "Compresses out-of-gamut colors overall (good for photos)")), (Intent::Relative, tr!("상대", "Relative"), tr!("영역 안 색은 그대로, 밖은 가장자리로 자름", "Keeps in-gamut colors, clips the rest to the edge"))] {
        if ui.selectable_label(app.prefs.proof_intent == v, l).on_hover_text(tip).clicked() {
            app.prefs.proof_intent = v;
        }
    }
    ui.checkbox(&mut app.prefs.proof_paper, tr!("종이·잉크", "Paper·Ink")).on_hover_text(tr!("종이 흰색과 잉크 검정의 한계를 흉내 냄 (화면이 어두워 보이는 게 정상)", "Simulates paper white and ink black limits (looking darker on screen is normal)"));
    ui.checkbox(&mut app.prefs.proof_gamut, tr!("영역 경고", "Gamut warning")).on_hover_text(tr!("이 프로필로 인쇄할 수 없는 색을 빨강으로 표시", "Shows colors this profile can't print in red"));
}

/// Indicator in the top-right corner of the canvas.
pub fn badge(name: &str, ui: &Ui, r: egui::Rect) {
    let p = ui.painter_at(r);
    let g = p.layout_no_wrap(trf!("교정 미리보기 · {name}", "Proof preview · {name}"), egui::FontId::proportional(11.5), egui::Color32::WHITE);
    let br = egui::Align2::RIGHT_TOP.anchor_size(r.right_top() + egui::vec2(-10.0, 8.0), g.size() + egui::vec2(12.0, 6.0));
    p.rect_filled(br, 4.0, ACCENT);
    p.galley(br.center() - g.size() * 0.5, g, egui::Color32::WHITE);
}
