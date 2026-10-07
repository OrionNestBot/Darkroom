//! People: face detection (YuNet), 5-point alignment, face embeddings (SFace, 128-d), and clustering by person.
//! Models come from OpenCV Zoo (YuNet: MIT, SFace: Apache-2.0) and run on ONNX Runtime (CPU), same as enhance.
//! © 2026 OrionNest

use super::ai::{self, Asset};
use super::decode::Rgba8;
use anyhow::{Result, anyhow};
use std::path::PathBuf;

pub const FACE_MODELS: [Asset; 2] = [
    Asset {
        file: "face_detection_yunet_2023mar.onnx",
        url: "https://huggingface.co/opencv/face_detection_yunet/resolve/3cc26e7f1014a5ee5d74a42acee58bafc9d0a310/face_detection_yunet_2023mar.onnx",
        entry: None,
        size: 232589,
        sha256: "8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4",
    },
    Asset {
        file: "face_recognition_sface_2021dec.onnx",
        url: "https://huggingface.co/opencv/face_recognition_sface/resolve/3d7082438a6e4551e840c9b2bb60b71e8da4b524/face_recognition_sface_2021dec.onnx",
        entry: None,
        size: 38696353,
        sha256: "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79",
    },
];

/// Required files (runtime library plus the two models).
pub fn assets() -> Vec<&'static Asset> {
    ai::RUNTIME.iter().chain(FACE_MODELS.iter()).collect()
}

pub fn ready() -> bool {
    assets().iter().all(|a| ai::present(a))
}

/// Total size of files not yet downloaded (bytes).
pub fn missing_bytes() -> u64 {
    assets().iter().filter(|a| !ai::present(a)).map(|a| if a.entry.is_some() { a.size / 2 } else { a.size }).sum()
}

pub fn download(progress: &dyn Fn(u64, u64, &str)) -> Result<()> {
    ai::download_list(&assets(), progress)
}

/// One detected face.
#[derive(Clone, Debug)]
pub struct FaceHit {
    /// Normalized [x, y, w, h] in image coordinates.
    pub bbox: [f32; 4],
    pub score: f32,
    /// Unit-length 128-d embedding.
    pub emb: Vec<f32>,
    /// 96×96 JPEG thumbnail (for the people list).
    pub thumb: Vec<u8>,
}

struct Det {
    score: f32,
    b: [f32; 4],
    kps: [[f32; 2]; 5],
}

const DET: usize = 640;
const REC: usize = 112;
/// SFace alignment landmarks (standard ArcFace 112×112).
const TEMPLATE: [[f32; 2]; 5] = [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]];

struct Models {
    det: ort::session::Session,
    rec: ort::session::Session,
}

static MODELS: std::sync::Mutex<Option<Models>> = std::sync::Mutex::new(None);

fn model_path(a: &Asset) -> PathBuf {
    ai::ai_dir().join("models").join(a.file)
}

fn with_models<R>(f: impl FnOnce(&mut Models) -> Result<R>) -> Result<R> {
    let mut g = MODELS.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        let open = |a: &Asset| -> Result<ort::session::Session> {
            let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(4);
            let b = ort::session::Session::builder().map_err(|e| anyhow!("{e}"))?;
            let mut b = b.with_intra_threads(threads).map_err(|e| anyhow!("{e}"))?;
            b.commit_from_file(model_path(a)).map_err(|e| anyhow!("{}", trf!("모델 열기 실패 ({}): {e}", "Couldn't open model ({}): {e}", a.file)))
        };
        ai::init_runtime()?;
        *g = Some(Models { det: open(&FACE_MODELS[0])?, rec: open(&FACE_MODELS[1])? });
    }
    f(g.as_mut().unwrap())
}

/// Free the model memory when done.
pub fn release() {
    *MODELS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn px(img: &Rgba8, x: f32, y: f32) -> [f32; 3] {
    // Bilinear sample
    let w = img.w as i32;
    let h = img.h as i32;
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x.floor() as i32, y.floor() as i32);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (tx, ty) = (x - x0 as f32, y - y0 as f32);
    let g = |xx: i32, yy: i32, c: usize| img.data[((yy * w + xx) * 4) as usize + c] as f32;
    let mut o = [0.0; 3];
    for (c, v) in o.iter_mut().enumerate() {
        let a = g(x0, y0, c) * (1.0 - tx) + g(x1, y0, c) * tx;
        let b = g(x0, y1, c) * (1.0 - tx) + g(x1, y1, c) * tx;
        *v = a * (1.0 - ty) + b * ty;
    }
    o
}

/// Detect faces in one image region scaled into 640×640 (coordinates in image pixels).
fn detect_region(m: &mut Models, img: &Rgba8, rx: f32, ry: f32, rw: f32, rh: f32, min_score: f32) -> Result<Vec<Det>> {
    let s = DET as f32 / rw.max(rh);
    let mut buf = vec![0.0f32; 3 * DET * DET];
    let (ow, oh) = ((rw * s) as usize, (rh * s) as usize);
    // Reduce aliasing when downscaling: average a 2×2 sample
    for y in 0..oh.min(DET) {
        for x in 0..ow.min(DET) {
            let mut acc = [0.0f32; 3];
            for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let p = px(img, rx + (x as f32 + dx) / s, ry + (y as f32 + dy) / s);
                for c in 0..3 {
                    acc[c] += p[c] * 0.25;
                }
            }
            // Input is BGR, 0..255
            buf[y * DET + x] = acc[2];
            buf[DET * DET + y * DET + x] = acc[1];
            buf[2 * DET * DET + y * DET + x] = acc[0];
        }
    }
    let t = ort::value::Tensor::from_array((vec![1i64, 3, DET as i64, DET as i64], buf)).map_err(|e| anyhow!("{e}"))?;
    let outs = m.det.run(ort::inputs!["input" => t]).map_err(|e| anyhow!("{}", trf!("얼굴 찾기 실패: {e}", "Face detection failed: {e}")))?;
    let get = |n: &str| -> Result<Vec<f32>> { Ok(outs[n].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?.1.to_vec()) };
    let mut v = Vec::new();
    for st in [8usize, 16, 32] {
        let (cls, obj, bb, kp) = (get(&format!("cls_{st}"))?, get(&format!("obj_{st}"))?, get(&format!("bbox_{st}"))?, get(&format!("kps_{st}"))?);
        let cols = DET / st;
        for i in 0..cols * cols {
            let score = (cls[i].clamp(0.0, 1.0) * obj[i].clamp(0.0, 1.0)).sqrt();
            if score < min_score {
                continue;
            }
            let (r, c) = ((i / cols) as f32, (i % cols) as f32);
            let stf = st as f32;
            let cx = (c + bb[i * 4]) * stf;
            let cy = (r + bb[i * 4 + 1]) * stf;
            let w = bb[i * 4 + 2].exp() * stf;
            let h = bb[i * 4 + 3].exp() * stf;
            let mut kps = [[0.0f32; 2]; 5];
            for (n, k) in kps.iter_mut().enumerate() {
                *k = [rx + (kp[i * 10 + 2 * n] + c) * stf / s, ry + (kp[i * 10 + 2 * n + 1] + r) * stf / s];
            }
            v.push(Det { score, b: [rx + (cx - w * 0.5) / s, ry + (cy - h * 0.5) / s, w / s, h / s], kps });
        }
    }
    Ok(v)
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    let i = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    i / (a[2] * a[3] + b[2] * b[3] - i).max(1e-6)
}

fn nms(mut v: Vec<Det>) -> Vec<Det> {
    v.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Det> = Vec::new();
    for d in v {
        if keep.iter().all(|k| iou(&k.b, &d.b) < 0.3) {
            keep.push(d);
        }
    }
    keep
}

/// 5 landmarks to a 112×112 aligned crop (similarity transform, complex least squares).
fn align(img: &Rgba8, kps: &[[f32; 2]; 5]) -> Vec<f32> {
    let n = 5.0;
    let (mut sx, mut sy, mut dx, mut dy) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for i in 0..5 {
        sx += kps[i][0];
        sy += kps[i][1];
        dx += TEMPLATE[i][0];
        dy += TEMPLATE[i][1];
    }
    let (sx, sy, dx, dy) = (sx / n, sy / n, dx / n, dy / n);
    // α = Σ (s - s̄)·conj(d - d̄) / Σ |d - d̄|²
    let (mut ar, mut ai, mut den) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..5 {
        let (pr, pi) = (kps[i][0] - sx, kps[i][1] - sy);
        let (qr, qi) = (TEMPLATE[i][0] - dx, TEMPLATE[i][1] - dy);
        ar += pr * qr + pi * qi;
        ai += pi * qr - pr * qi;
        den += qr * qr + qi * qi;
    }
    let (ar, ai) = (ar / den, ai / den);
    let mut out = vec![0.0f32; 3 * REC * REC];
    for v in 0..REC {
        for u in 0..REC {
            let (qr, qi) = (u as f32 + 0.5 - dx, v as f32 + 0.5 - dy);
            let x = sx + ar * qr - ai * qi - 0.5;
            let y = sy + ai * qr + ar * qi - 0.5;
            let p = px(img, x, y);
            // SFace input is RGB, 0..255
            for c in 0..3 {
                out[c * REC * REC + v * REC + u] = p[c];
            }
        }
    }
    out
}

fn thumb(img: &Rgba8, b: &[f32; 4]) -> Vec<u8> {
    const T: usize = 96;
    let side = b[2].max(b[3]) * 1.5;
    let (cx, cy) = (b[0] + b[2] * 0.5, b[1] + b[3] * 0.5);
    let mut rgb = vec![0u8; T * T * 3];
    for y in 0..T {
        for x in 0..T {
            let p = px(img, cx - side * 0.5 + (x as f32 + 0.5) * side / T as f32, cy - side * 0.5 + (y as f32 + 0.5) * side / T as f32);
            for c in 0..3 {
                rgb[(y * T + x) * 3 + c] = p[c] as u8;
            }
        }
    }
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    let _ = image::ImageEncoder::write_image(enc, &rgb, T as u32, T as u32, image::ExtendedColorType::Rgb8);
    out
}

/// Detect faces in one photo and extract their embeddings.
pub fn analyze(img: &Rgba8) -> Result<Vec<FaceHit>> {
    let (w, h) = (img.w as f32, img.h as f32);
    let min_score = crate::config::FACE_MIN_SCORE;
    with_models(|m| {
        let mut dets = detect_region(m, img, 0.0, 0.0, w, h, min_score)?;
        // Large photos get a second pass over four overlapping tiles (small faces in group shots)
        if w.max(h) > 1100.0 {
            let (tw, th) = (w * 0.6, h * 0.6);
            for (x, y) in [(0.0, 0.0), (w - tw, 0.0), (0.0, h - th), (w - tw, h - th)] {
                dets.extend(detect_region(m, img, x, y, tw, th, min_score)?);
            }
        }
        let min_px = w.max(h) * crate::config::FACE_MIN_SIZE;
        let dets: Vec<Det> = nms(dets).into_iter().filter(|d| d.b[2].min(d.b[3]) >= min_px).collect();
        let mut out = Vec::new();
        for d in dets {
            let x = align(img, &d.kps);
            let t = ort::value::Tensor::from_array((vec![1i64, 3, REC as i64, REC as i64], x)).map_err(|e| anyhow!("{e}"))?;
            let o = m.rec.run(ort::inputs!["data" => t]).map_err(|e| anyhow!("{}", trf!("얼굴 특징 실패: {e}", "Face feature extraction failed: {e}")))?;
            let mut emb = o[0].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?.1.to_vec();
            let n = emb.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
            emb.iter_mut().for_each(|v| *v /= n);
            out.push(FaceHit { bbox: [d.b[0] / w, d.b[1] / h, d.b[2] / w, d.b[3] / h], score: d.score, emb, thumb: thumb(img, &d.b) });
        }
        Ok(out)
    })
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Group faces by person. Input: (face id, embedding, current person id; 0 = unassigned, <0 = never cluster, score).
/// Returns the changed (face id, person id) pairs. New people get non-negative temporary ids `NEW_BASE + k`.
pub const NEW_BASE: i64 = 1 << 40;

pub fn cluster(faces: &[(i64, &[f32], i64, f32)]) -> Vec<(i64, i64)> {
    use std::collections::HashMap;
    let t_join = crate::config::FACE_SAME_PERSON;
    // Centroids of existing people
    let mut cent: HashMap<i64, (Vec<f32>, usize)> = HashMap::new();
    for (_, e, p, _) in faces {
        if *p > 0 {
            let c = cent.entry(*p).or_insert_with(|| (vec![0.0; e.len()], 0));
            c.0.iter_mut().zip(e.iter()).for_each(|(a, b)| *a += b);
            c.1 += 1;
        }
    }
    let norm = |v: &[f32]| -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        v.iter().map(|x| x / n).collect()
    };
    let known: Vec<(i64, Vec<f32>)> = cent.into_iter().map(|(k, (v, _))| (k, norm(&v))).collect();
    let mut changes = Vec::new();
    let mut rest: Vec<&(i64, &[f32], i64, f32)> = Vec::new();
    for f in faces.iter().filter(|f| f.2 == 0) {
        let best = known.iter().map(|(k, c)| (*k, cosine(f.1, c))).max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((k, s)) if s >= t_join => changes.push((f.0, k)),
            _ => rest.push(f),
        }
    }
    // Remaining faces: seed representatives in descending score order and attach the rest
    rest.sort_by(|a, b| b.3.total_cmp(&a.3));
    let mut groups: Vec<(Vec<f32>, Vec<i64>)> = Vec::new();
    for f in rest {
        let best = groups.iter_mut().map(|g| {
            let s = cosine(f.1, &norm(&g.0));
            (s, g)
        }).max_by(|a, b| a.0.total_cmp(&b.0));
        match best {
            Some((s, g)) if s >= t_join => {
                g.0.iter_mut().zip(f.1.iter()).for_each(|(a, b)| *a += b);
                g.1.push(f.0);
            }
            _ => groups.push((f.1.to_vec(), vec![f.0])),
        }
    }
    let mut k = 0;
    for (_, ids) in groups {
        if ids.len() >= 2 {
            for id in ids {
                changes.push((id, NEW_BASE + k));
            }
            k += 1;
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cluster_groups_similar() {
        let a = [1.0f32, 0.0, 0.0];
        let a2 = [0.95f32, 0.31, 0.0];
        let b = [0.0f32, 0.0, 1.0];
        let b2 = [0.0f32, 0.2, 0.98];
        let c = [0.0f32, 1.0, 0.0];
        let faces = vec![(1, &a[..], 0, 0.9), (2, &a2[..], 0, 0.8), (3, &b[..], 7, 0.9), (4, &b2[..], 0, 0.9), (5, &c[..], 0, 0.9), (6, &c[..], -1, 0.9)];
        let ch = cluster(&faces);
        let get = |id| ch.iter().find(|c| c.0 == id).map(|c| c.1);
        assert_eq!(get(4), Some(7), "기존 인물에 붙음");
        assert_eq!(get(1), get(2));
        assert!(get(1).unwrap() >= NEW_BASE);
        assert_eq!(get(5), None, "혼자인 얼굴은 미분류");
        assert_eq!(get(6), None, "묶지 않음 표시는 그대로");
    }

    /// Real model test: runs only when the models are in DARKROOM_AI_DIR.
    #[test]
    #[ignore]
    fn real_faces() {
        let dir = std::env::var("DARKROOM_FACE_IMG").expect("DARKROOM_FACE_IMG");
        let mut embs = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().map(|e| crate::config::is_supported_ext(&e.to_string_lossy())).unwrap_or(false) {
                let img = crate::imaging::decode::decode_preview(&p, 1600, 1).unwrap();
                let t = std::time::Instant::now();
                let f = analyze(&img).unwrap();
                eprintln!("{}: {} faces {:?} in {:?}", p.display(), f.len(), f.iter().map(|x| (x.score, x.bbox)).collect::<Vec<_>>(), t.elapsed());
                for x in f {
                    embs.push((p.file_name().unwrap().to_string_lossy().to_string(), x.emb));
                }
            }
        }
        for i in 0..embs.len() {
            for j in i + 1..embs.len() {
                eprintln!("{} ~ {}: {:.3}", embs[i].0, embs[j].0, cosine(&embs[i].1, &embs[j].1));
            }
        }
    }
}
