# Third-party notices

Darkroom is an independent project. It is not affiliated with, endorsed by, or sponsored by Adobe Inc.
Adobe, Lightroom, Camera Raw and Photoshop are trademarks of Adobe Inc.; they are mentioned only to describe
compatibility (reading Lightroom catalogs, presets and XMP sidecars, and writing XMP sidecars Lightroom can read).

Darkroom does not need, read or include any Adobe software or Adobe data file (camera profiles, lens profiles and the
like). The only files of that kind it opens are ones the user created and chooses to import: catalogs, develop
presets, watermarks and XMP sidecars; to make that easier it suggests the user's own preset folders under
`%APPDATA%\Adobe`. Profile names such as "Adobe Standard" and "Adobe Color" and the look
identifiers (UUIDs) written to XMP sidecars are kept only so that Lightroom recognises the settings; they refer to
Darkroom's own profiles inside Darkroom.

## Bundled code and data

| Component | Use | License |
|---|---|---|
| [rawler](https://github.com/dnglab/dnglab) 0.8.0, modified (`vendor/rawler`, changes listed in `vendor/rawler/DARKROOM_CHANGES.md`) | RAW decoding | LGPL-2.1 — the modified rawler source is included in this repository, so the program can be rebuilt against another modified rawler |
| [lensfun](https://lensfun.github.io) lens database (`assets/lensfun`, XML files of commit bbd4332, 2026-09-24) | Lens corrections | CC BY-SA 3.0 — data by the lensfun contributors, unchanged |
| Other Rust crates (egui, eframe, image, rusqlite, rayon, ureq, ort, …) | | MIT / Apache-2.0 / BSD / Zlib / ISC / MPL-2.0 (option-ext) — see `Cargo.lock` |

Darkroom's own camera profiles, looks, default tone curve and lens vignetting tables (`assets/profiles.json`,
`assets/looks.json`, `TONE_NODES` in `src/develop/dcp.rs`, `assets/lens_vig.json`) start from rawler's open camera color
matrices and the lensfun data and were fitted by the author by comparing rendered images (black-box comparison of
output only); they contain no Adobe profile, lens profile or other Adobe data. Cameras beyond the author's own were
fitted against public sample RAW files from [raw.pixls.us](https://raw.pixls.us) (CC0; used only during development,
not bundled).

The color pipeline follows the color model of the DNG specification (camera/forward matrices interpolated by color
temperature, hue/saturation/value maps, look tables, baseline exposure). The DCP file format (also from the DNG
specification) is read from DNG files and from DCP files the user puts in Darkroom's own `profiles` folder.

Camera built-in lens correction data (Sony, Fujifilm, Olympus/OM, Panasonic, Nikon Z, DNG opcodes — `src/develop/embedded.rs`)
is read from the user's own RAW files. The tag layouts and formulas were written by the author from public descriptions
(darktable's documented behaviour and source as a reference, ExifTool tag names, the DNG specification); no code was copied.

Measured response tables (`measured_curves.rs`, camera baseline/white-balance tables) were measured by the author
by comparing renders of test photos (black-box measurement of output; no code or data files were copied).

## Downloaded on first use (not bundled)

These are downloaded from their official locations when a feature is first used, verified by SHA-256, and stored in the
app data folder.

| Component | Feature | License |
|---|---|---|
| Microsoft ONNX Runtime (DirectML) | AI runtime | MIT |
| Microsoft DirectML | GPU inference | Microsoft DirectML license |
| NAFNet (megvii) | AI Denoise | MIT |
| Real-ESRGAN x4plus (Xintao Wang et al.) | AI Super Resolution | BSD-3-Clause |
| BiRefNet-lite | AI mask: subject / background | MIT |
| SegFormer-B2 ADE20K (NVIDIA) | AI mask: sky / people | NVIDIA Source Code License — **non-commercial use only** |
| LaMa (Samsung AI) | Remove tool | Apache-2.0 |
| YuNet (OpenCV Zoo) | Face detection (People) | MIT |
| SFace (OpenCV Zoo) | Face recognition (People) | Apache-2.0 |
