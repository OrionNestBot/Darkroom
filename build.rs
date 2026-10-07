// Build script: embeds the icon and version info, packs the bundled lensfun data, and collects UI string pairs.
use std::io::Write;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/lensfun");
    // Concatenate assets/lensfun/*.xml (sorted) as "name\ncontent\0" records and deflate them into OUT_DIR/lensfun.bin.
    let mut files: Vec<_> = std::fs::read_dir("assets/lensfun").expect("assets/lensfun").flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "xml")).collect();
    files.sort();
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
    for p in &files {
        println!("cargo:rerun-if-changed={}", p.display());
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        enc.write_all(name.as_bytes()).unwrap();
        enc.write_all(b"\n").unwrap();
        enc.write_all(&std::fs::read(p).unwrap()).unwrap();
        enc.write_all(b"\0").unwrap();
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("lensfun.bin");
    std::fs::write(out, enc.finish().unwrap()).unwrap();

    i18n_pairs();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Darkroom");
        res.set("FileDescription", "Darkroom — RAW photo manager and editor");
        res.set("LegalCopyright", "© 2026 OrionNest");
        let _ = res.compile();
    }
}

/// Collects the (Korean, English) pairs of every tr!/trf! call into OUT_DIR/i18n_pairs.rs, so stored labels
/// such as history entries can be shown in the current language (`i18n::label`).
fn i18n_pairs() {
    println!("cargo:rerun-if-changed=src");
    let mut dirs = vec![std::path::PathBuf::from("src")];
    let mut rs = Vec::new();
    while let Some(d) = dirs.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                rs.push(p);
            }
        }
    }
    rs.sort();
    let mut tr = std::collections::BTreeSet::new();
    let mut trf = std::collections::BTreeSet::new();
    for p in &rs {
        let Ok(text) = std::fs::read_to_string(p) else { continue };
        for (mac, is_fmt) in [("trf!(", true), ("tr!(", false)] {
            let mut i = 0;
            while let Some(k) = text[i..].find(mac) {
                let at = i + k;
                i = at + mac.len();
                // Skip the "tr!(" that is part of "trf!(".
                if !is_fmt && at > 0 && text.as_bytes()[at - 1] == b'f' {
                    continue;
                }
                let Some((ko, r2)) = lit(&text[i..]) else { continue };
                let Some(r3) = r2.trim_start().strip_prefix(',') else { continue };
                let Some((en, _)) = lit(r3.trim_start()) else { continue };
                if ko != en {
                    if is_fmt { trf.insert((ko, en)) } else { tr.insert((ko, en)) };
                }
            }
        }
    }
    let mut out = String::from("pub static TR_PAIRS: &[(&str, &str)] = &[\n");
    for (k, e) in &tr {
        out += &format!("    ({k:?}, {e:?}),\n");
    }
    out += "];\npub static TRF_PAIRS: &[(&str, &str)] = &[\n";
    for (k, e) in &trf {
        out += &format!("    ({k:?}, {e:?}),\n");
    }
    out += "];\n";
    let dst = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("i18n_pairs.rs");
    std::fs::write(dst, out).unwrap();
}

/// Parses a leading Rust string literal and returns (unescaped content, rest of input).
fn lit(s: &str) -> Option<(String, &str)> {
    let s = s.strip_prefix('"')?;
    let mut out = String::new();
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        match c {
            '"' => return Some((out, &s[i + 1..])),
            '\\' => match it.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                '0' => out.push('\0'),
                // Line continuation: skip the newline and the next line's leading whitespace.
                '\n' | '\r' => {
                    while it.peek().is_some_and(|(_, c)| c.is_whitespace()) {
                        it.next();
                    }
                }
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
    None
}
