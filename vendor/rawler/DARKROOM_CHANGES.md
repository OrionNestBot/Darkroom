# Changes in this copy of rawler

This is rawler 0.8.0 from crates.io (https://github.com/dnglab/dnglab, LGPL-2.1), used by Darkroom through
`[patch.crates-io]` in Darkroom's `Cargo.toml`. Test data, benchmarks and the command-line tools were left out;
everything else is unchanged except:

1. **Extra camera definitions** — `rawler::set_extra_camera_dir(dir)`: before the first decode, the loader also reads
   every `*.toml` in that folder (same format as `data/cameras/<maker>/<model>.toml`). An entry with the same
   make/model/mode replaces the built-in one; a file that fails to parse is skipped instead of panicking.
   (`src/lib.rs`, `RawLoader::new` and `RawLoader::cameras_from_toml` in `src/decoders/mod.rs`)
2. **Unknown camera fallback** — when a file's make is known but its model is not in the list, the loader borrows a
   definition instead of failing: it picks the most similar model name of the same make (longest common prefix, then
   the closest number with the same number of digits where the names differ — X-T9 → X-T5, ILCE-7M9 → ILCE-7M5) and
   uses that model's definition for the requested mode (none if that model has no such mode). `remark` is set to
   `darkroom-fallback:<borrowed model>` and `clean_model` drops a leading maker word. Can be turned off with
   `rawler::set_unknown_camera_fallback(false)`. (`RawLoader::fallback_camera` in `src/decoders/mod.rs`)
3. **ORF high-resolution mode CFA fix** — the Olympus/OM decoder chose the "highres" camera definition but built the CFA
   from the base definition, so high-res shots of bodies whose high-res pattern differs (e.g. PEN-F, `GRBG`) came out
   strongly magenta. It now uses the CFA of the chosen definition. (`src/decoders/orf.rs`)
4. `Cargo.toml`: the `[[bin]]`, `[[test]]` and `[[bench]]` targets were removed (their files are not included).
