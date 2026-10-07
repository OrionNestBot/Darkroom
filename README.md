# Darkroom

**English** · [한국어](README.ko.md)

Darkroom is a RAW photo manager and non-destructive editor for Windows — an independent alternative to the
popular commercial photo editing suites. It is a single executable with no installer, and it needs no other software.

## Features

**Library**
- Catalogs with ratings, color labels, flags, keywords, titles and captions
- Collections, smart collections, Quick Collection, stacks and virtual copies
- Grid, loupe, compare and survey views; filters, sorting and search
- People view: finds faces and groups photos of the same person (on your PC)
- Import (add in place, copy or move), batch rename, several catalogs with automatic backups

**Develop**
- White balance, exposure, contrast, highlights, shadows, whites, blacks, texture, clarity, dehaze, vibrance, saturation
- Tone curve (parametric and point, per channel), HSL / B&W mixer, point color, color grading, calibration
- Sharpening, noise reduction, lens corrections, transform (upright), crop and straighten, vignette and grain
- Local masks: brush, linear, radial, color range, luminance range, and AI masks (subject, background, sky, people)
- Spot removal, red-eye correction, privacy blur for faces and plates
- History, snapshots, presets, copy/paste and sync of settings, before/after and split views, reference view, soft proofing

**Color and lenses**
- Color pipeline based on the DNG specification, with Darkroom's own camera profiles (about 120 camera models) and looks,
  plus a generic profile for cameras it has not seen before
- Lens corrections from the open [lensfun](https://lensfun.github.io) database (built in) and from the correction data
  cameras record in their RAW files

**Merge and AI**
- HDR merge and panorama merge
- AI Enhance (denoise and super resolution), AI masks, AI object removal — all processing stays on your PC

**Output**
- Export to JPEG, TIFF or PNG in sRGB, Adobe RGB (1998) compatible or Display P3, with resizing, output sharpening,
  watermarks (text or image) and metadata options; export presets
- Print layouts to PDF and slideshow

**Moving from another photo editor**
- Import catalogs (`.lrcat`, with develop settings, history and collections), develop presets and XMP sidecars
  from widely used photo editing software
- Write XMP sidecars that other photo editors can read

Interface languages: English and Korean.

## Install

Download `Darkroom.exe` from [Releases](https://github.com/OrionNestBot/Darkroom/releases) and run it (Windows 10 or 11, 64-bit).

- Catalogs and caches are stored in `%LOCALAPPDATA%\Darkroom\`. Your original photos are never modified.
- AI features download the files they need the first time you use them. Every file is checked with SHA-256,
  and all processing happens on your PC.
- HEIC files need the "HEIF Image Extensions" from the Microsoft Store.

### New cameras and lenses without updating

Put files in the data folder (`%LOCALAPPDATA%\Darkroom\` — Preferences can open it); they are read the next time Darkroom starts.

| Folder | What to put there |
|---|---|
| `cameras` | [rawler](https://github.com/dnglab/dnglab) camera definitions (`.toml`). A camera missing from the list otherwise opens with the settings of the most similar model of the same maker, with a notice. |
| `profiles` | DCP camera profiles (the open format of the DNG specification) |
| `lensfun` | [lensfun](https://lensfun.github.io) lens database XML files. A file with the same name replaces the built-in one; a new name is added. |

## Build

With [Rust](https://rustup.rs) (stable):

```
cargo build --release
```

The program is written to `target\release\Darkroom.exe`.

## License

[PolyForm Noncommercial License 1.0.0](LICENSE.md) — free to use, change and share for any noncommercial purpose.

Third-party code and data and their licenses are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
The RAW decoder is a modified copy of rawler (LGPL-2.1); its source is included in `vendor/rawler`.
