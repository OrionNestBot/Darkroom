//! AI enhancement: denoise (NAFNet-SIDD, trained on real photo noise) and 2x super-resolution (Real-ESRGAN).
//! ONNX Runtime (DirectML, GPU) is loaded at runtime. To keep a single exe, the runtime and models are
//! downloaded to the data folder (`ai/`) on first use from their official sources and verified by SHA-256.
//!
//! The result is saved next to the original as a linear camera-RGB file (.drhdr, marked `enhance`) that stays editable like a RAW.

use crate::develop::image::LinearImage;
use anyhow::{Context, Result, anyhow};
use rayon::prelude::*;
use sha2::Digest;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// One file to download. If `entry` is set, only that entry of the zip (nupkg) is fetched with HTTP Range requests.
pub struct Asset {
    pub file: &'static str,
    pub url: &'static str,
    pub entry: Option<&'static str>,
    pub size: u64,
    pub sha256: &'static str,
}

const ORT_PKG: &str = "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg";
const DML_PKG: &str = "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg";

/// ONNX Runtime 1.24.4 (DirectML), official NuGet package, MIT.
pub const RUNTIME: [Asset; 3] = [
    Asset { file: "onnxruntime.dll", url: ORT_PKG, entry: Some("runtimes/win-x64/native/onnxruntime.dll"), size: 17328152, sha256: "e7eedec6a6f26dc39dc948276a75ef6d2bee3fff944d874ceed0bbd3b97bff40" },
    Asset { file: "onnxruntime_providers_shared.dll", url: ORT_PKG, entry: Some("runtimes/win-x64/native/onnxruntime_providers_shared.dll"), size: 22040, sha256: "265c8daf29637cb259cac8be9f08f2cd45f3883f0f0e4949cbfddd5b4cbec3b6" },
    Asset { file: "DirectML.dll", url: DML_PKG, entry: Some("bin/x64-win/DirectML.dll"), size: 18527776, sha256: "9c9e6d822561c6c41b90e6994b3e8857cf1d66dbfb1e0c4c799c7c89b4e92da1" },
];

/// NAFNet-SIDD width64 (Chen et al. 2022), MIT. The PyTorch weights are converted to ONNX in the app (`onnxw::nafnet`).
/// Chosen over SCUNet: much faster on GPU and keeps more texture.
pub const DENOISE: [Asset; 1] = [Asset {
    file: "NAFNet-SIDD-width64.pth",
    url: "https://huggingface.co/nyanko7/nafnet-models/resolve/08e8c701eb662688cccda3bf3074e90e902bcd3b/NAFNet-SIDD-width64.pth",
    entry: None,
    size: 464154961,
    sha256: "cd685efaae01f7c4e9951f2deab05780079c8eb1e49ed664b72f6db04dabb445",
}];

/// Converted denoise model.
const NAF_ONNX: &str = "nafnet-sidd-w64.onnx";

fn denoise_model() -> PathBuf {
    ai_dir().join("models").join(NAF_ONNX)
}

/// Convert .pth to .onnx, then delete the .pth.
fn convert_denoise() -> Result<()> {
    let pth = path_of(&DENOISE[0]);
    let sd = super::torchpt::load(&pth)?;
    let bytes = super::onnxw::nafnet(&sd)?;
    let part = denoise_model().with_extension("onnx.part");
    std::fs::write(&part, &bytes)?;
    std::fs::rename(&part, denoise_model())?;
    let _ = std::fs::remove_file(&pth);
    Ok(())
}

/// Real-ESRGAN x4plus (Wang et al. 2021), BSD-3-Clause.
pub const UPSCALE: [Asset; 1] = [Asset {
    file: "realesrgan-x4plus.onnx",
    url: "https://huggingface.co/jonathanst29/tinier-upscale-models/resolve/899dc1e4b22bbf1955c2f1739c085edc080cb366/realesrgan-x4plus.onnx",
    entry: None,
    size: 67051618,
    sha256: "4ed6a45a8185bde6c8d7c7790a6e80e3c5b9f608946f634021068dd0fd0473c8",
}];

/// Enhance options: denoise and super-resolution can be combined.
#[derive(Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Options {
    pub denoise: bool,
    /// Denoise amount 0..100
    pub amount: f32,
    /// 2x super-resolution
    pub superres: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { denoise: true, amount: 50.0, superres: false }
    }
}

impl Options {
    pub fn any(&self) -> bool {
        self.denoise || self.superres
    }
    pub fn assets(&self) -> Vec<&'static Asset> {
        let mut v: Vec<&Asset> = RUNTIME.iter().collect();
        if self.denoise {
            v.extend(DENOISE.iter());
        }
        if self.superres {
            v.extend(UPSCALE.iter());
        }
        v
    }
    /// Suffix appended to the output file name.
    pub fn suffix(&self) -> &'static str {
        match (self.denoise, self.superres) {
            (true, true) => "Enhanced-NR-SR",
            (false, true) => "Enhanced-SR",
            _ => "Enhanced-NR",
        }
    }
}

pub fn ai_dir() -> PathBuf {
    std::env::var_os("DARKROOM_AI_DIR").map(PathBuf::from).unwrap_or_else(|| crate::config::data_dir().join("ai"))
}

fn dir_of(a: &Asset) -> PathBuf {
    if RUNTIME.iter().any(|r| r.file == a.file) { ai_dir().join("runtime") } else { ai_dir().join("models") }
}

fn path_of(a: &Asset) -> PathBuf {
    dir_of(a).join(a.file)
}

/// Quick check by file size (hashes are verified on download). Denoise counts as installed once the converted model exists.
pub fn present(a: &Asset) -> bool {
    if a.file == DENOISE[0].file && std::fs::metadata(denoise_model()).map(|m| m.len() > 0).unwrap_or(false) {
        return true;
    }
    std::fs::metadata(path_of(a)).map(|m| m.len() == a.size).unwrap_or(false)
}

/// Downloaded but not yet converted.
fn needs_convert() -> bool {
    !std::fs::metadata(denoise_model()).map(|m| m.len() > 0).unwrap_or(false) && std::fs::metadata(path_of(&DENOISE[0])).map(|m| m.len() == DENOISE[0].size).unwrap_or(false)
}

/// Missing files and the download size in bytes (approximate; zip entries count only their compressed size).
pub fn missing(opts: &Options) -> (Vec<&'static Asset>, u64) {
    let mut v: Vec<&Asset> = opts.assets().into_iter().filter(|a| !present(a)).collect();
    // A file that still needs conversion counts as missing (conversion runs in the download step).
    if opts.denoise && needs_convert() && !v.iter().any(|a| a.file == DENOISE[0].file) {
        v.push(&DENOISE[0]);
    }
    let bytes = v.iter().filter(|a| !(a.file == DENOISE[0].file && needs_convert())).map(|a| if a.entry.is_some() { a.size / 2 } else { a.size }).sum();
    (v, bytes)
}

/// Total size of installed AI files (shown in preferences).
pub fn installed_bytes() -> u64 {
    let conv = std::fs::metadata(denoise_model()).map(|m| m.len()).unwrap_or(0);
    RUNTIME
        .iter()
        .chain(UPSCALE.iter())
        .chain(super::people::FACE_MODELS.iter())
        .chain([super::aimask::SUBJECT_MODEL, super::aimask::SEG_MODEL, super::inpaint::LAMA].iter())
        .filter(|a| present(a))
        .map(|a| a.size)
        .sum::<u64>()
        + conv
}

/// Delete all downloaded files (also releases open sessions).
pub fn remove_all() -> Result<()> {
    release();
    super::people::release();
    let d = ai_dir().join("models");
    if d.exists() {
        std::fs::remove_dir_all(&d)?;
    }
    // A runtime library that is already loaded cannot be deleted; mark it for deletion on next launch.
    let r = ai_dir().join("runtime");
    if r.exists() && std::fs::remove_dir_all(&r).is_err() {
        let _ = std::fs::write(ai_dir().join("remove_runtime"), b"1");
    }
    Ok(())
}

/// At startup: remove runtime libraries left over from a previous delete, and the old SCUNet model.
pub fn cleanup_pending() {
    for f in ["scunet_color_real_psnr.onnx", "scunet_color_real_psnr.onnx.data"] {
        let _ = std::fs::remove_file(ai_dir().join("models").join(f));
    }
    let flag = ai_dir().join("remove_runtime");
    if flag.exists() {
        let _ = std::fs::remove_dir_all(ai_dir().join("runtime"));
        let _ = std::fs::remove_file(flag);
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// HTTPS root certificates come from the Windows certificate store, so trust stays current through OS updates.
fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
    ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(1800))).tls_config(tls).build().into()
}

fn get_range(ag: &ureq::Agent, url: &str, a: u64, b: u64) -> Result<Vec<u8>> {
    let mut r = ag.get(url).header("Range", &format!("bytes={a}-{b}")).call().with_context(|| trf!("받기 실패: {url}", "Download failed: {url}"))?;
    let v = r.body_mut().with_config().limit(256 << 20).read_to_vec()?;
    if v.len() as u64 != b - a + 1 {
        return Err(anyhow!("{}", trf!("부분 받기 크기가 맞지 않음 ({} / {})", "Partial download size mismatch ({} / {})", v.len(), b - a + 1)));
    }
    Ok(v)
}

/// Extract one zip entry (central directory -> local header -> compressed data).
fn fetch_zip_entry(ag: &ureq::Agent, url: &str, entry: &str, progress: &dyn Fn(u64)) -> Result<Vec<u8>> {
    let head = ag.head(url).call().with_context(|| trf!("연결 실패: {url}", "Connection failed: {url}"))?;
    let size: u64 = head.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|s| s.parse().ok()).ok_or_else(|| anyhow!("{}", trf!("크기를 알 수 없음", "Unknown size")))?;
    let tail_len = size.min(65536);
    let tail = get_range(ag, url, size - tail_len, size - 1)?;
    let eocd = tail.windows(4).rposition(|w| w == b"PK\x05\x06").ok_or_else(|| anyhow!("{}", trf!("zip 형식이 아님", "Not a zip file")))?;
    let u16le = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as u64;
    let u32le = |b: &[u8], o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as u64;
    let (cds, cdo) = (u32le(&tail, eocd + 12), u32le(&tail, eocd + 16));
    let cd = get_range(ag, url, cdo, cdo + cds - 1)?;
    let mut p = 0usize;
    while p + 46 <= cd.len() {
        let meth = u16le(&cd, p + 10);
        let crc = u32le(&cd, p + 16) as u32;
        let csz = u32le(&cd, p + 20);
        let usz = u32le(&cd, p + 24);
        let (fl, el, cl) = (u16le(&cd, p + 28) as usize, u16le(&cd, p + 30) as usize, u16le(&cd, p + 32) as usize);
        let off = u32le(&cd, p + 42);
        let name = String::from_utf8_lossy(&cd[p + 46..p + 46 + fl]).to_string();
        p += 46 + fl + el + cl;
        if name != entry {
            continue;
        }
        let lh = get_range(ag, url, off, off + 29)?;
        let start = off + 30 + u16le(&lh, 26) + u16le(&lh, 28);
        progress(0);
        let comp = get_range(ag, url, start, start + csz - 1)?;
        progress(csz);
        let data = match meth {
            0 => comp,
            8 => {
                let mut v = Vec::with_capacity(usz as usize);
                flate2::read::DeflateDecoder::new(&comp[..]).read_to_end(&mut v)?;
                v
            }
            m => return Err(anyhow!("{}", trf!("지원하지 않는 압축 방식 {m}", "Unsupported compression {m}"))),
        };
        let mut h = flate2::Crc::new();
        h.update(&data);
        if h.sum() != crc {
            return Err(anyhow!("{}", trf!("압축 해제 검사 실패: {entry}", "Decompression check failed: {entry}")));
        }
        return Ok(data);
    }
    Err(anyhow!("{}", trf!("패키지에 {entry}가 없음", "{entry} not found in package")))
}

/// Download missing files. progress(bytes done, total bytes, file name)
pub fn download(opts: &Options, progress: &dyn Fn(u64, u64, &str)) -> Result<()> {
    let (list, total) = missing(opts);
    fetch(list, total, progress)?;
    if needs_convert() {
        progress(total, total, tr!("모델 변환 중", "Converting model"));
        convert_denoise()?;
    }
    Ok(())
}

/// Download only the missing files from a list (e.g. portrait models).
pub fn download_list(list: &[&'static Asset], progress: &dyn Fn(u64, u64, &str)) -> Result<()> {
    let v: Vec<&'static Asset> = list.iter().copied().filter(|a| !present(a)).collect();
    let total = v.iter().map(|a| if a.entry.is_some() { a.size / 2 } else { a.size }).sum();
    fetch(v, total, progress)
}

fn fetch(list: Vec<&'static Asset>, total: u64, progress: &dyn Fn(u64, u64, &str)) -> Result<()> {
    let ag = agent();
    let mut done = 0u64;
    for a in list {
        if a.file == DENOISE[0].file && needs_convert() {
            continue;
        }
        let dir = dir_of(a);
        std::fs::create_dir_all(&dir)?;
        let dst = dir.join(a.file);
        let part = dir.join(format!("{}.part", a.file));
        // Try the official source, then each mirror (config::ASSET_MIRRORS, same file name). The SHA-256 must match either way.
        let mut sources: Vec<(String, Option<&str>)> = vec![(a.url.to_string(), a.entry)];
        sources.extend(crate::config::ASSET_MIRRORS.iter().map(|m| (format!("{m}{}", a.file), None)));
        let base = done;
        let mut last_err = None;
        for (url, entry) in &sources {
            match fetch_one(&ag, a, url, *entry, &part, &|n| progress(base + n, total, a.file)) {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    last_err = Some(e);
                }
            }
        }
        if let Some(e) = last_err {
            return Err(e.context(trf!("직접 받은 파일을 {} 폴더에 넣어도 됩니다", "You can also put the file in the {} folder", dir.display())));
        }
        done = base + if a.entry.is_some() { a.size / 2 } else { a.size };
        std::fs::rename(&part, &dst)?;
        progress(done, total, a.file);
    }
    Ok(())
}

/// Download from one source into `part` and verify SHA-256. Progress counts bytes within this file.
fn fetch_one(ag: &ureq::Agent, a: &Asset, url: &str, entry: Option<&str>, part: &Path, progress: &dyn Fn(u64)) -> Result<()> {
    let mut hasher = sha2::Sha256::new();
    match entry {
        Some(e) => {
            let data = fetch_zip_entry(ag, url, e, progress)?;
            hasher.update(&data);
            std::fs::write(part, &data)?;
        }
        None => {
            let mut r = ag.get(url).call().with_context(|| trf!("받기 실패: {}", "Download failed: {}", a.file))?;
            let mut rd = r.body_mut().with_config().limit(u64::MAX).reader();
            let mut f = std::io::BufWriter::new(std::fs::File::create(part)?);
            let mut buf = vec![0u8; 1 << 20];
            let mut last = std::time::Instant::now();
            let mut got = 0u64;
            loop {
                let n = rd.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                f.write_all(&buf[..n])?;
                got += n as u64;
                if last.elapsed().as_millis() > 150 {
                    progress(got);
                    last = std::time::Instant::now();
                }
            }
            f.flush()?;
        }
    }
    if hex(&hasher.finalize()) != a.sha256 {
        return Err(anyhow!("{}", trf!("{}: 내려받은 파일 검증 실패 (SHA-256 불일치)", "{}: downloaded file failed verification (SHA-256 mismatch)", a.file)));
    }
    Ok(())
}

// ───────────────────────── Inference ─────────────────────────

static INIT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();

/// Load ONNX Runtime (once).
pub(crate) fn init_runtime() -> Result<()> {
    INIT.get_or_init(|| {
        let dir = ai_dir().join("runtime");
        if !RUNTIME.iter().all(present) {
            return Err(tr!("AI 실행 라이브러리가 없습니다", "AI runtime library is missing").into());
        }
        // Load the bundled DirectML.dll first so onnxruntime.dll uses it instead of the older system copy.
        unsafe {
            use windows::Win32::System::LibraryLoader::LoadLibraryW;
            use windows::core::HSTRING;
            let _ = LoadLibraryW(&HSTRING::from(dir.join("DirectML.dll").as_os_str()));
        }
        let b = ort::init_from(dir.join("onnxruntime.dll")).map_err(|e| trf!("ONNX Runtime 불러오기 실패: {e}", "Couldn't load ONNX Runtime: {e}"))?;
        b.with_name("darkroom").commit();
        Ok(())
    })
    .clone()
    .map_err(|e| anyhow!(e))
}

/// Session cache, reused by preview and final processing (creating a session takes seconds of GPU compilation).
struct Cached {
    model: PathBuf,
    /// Fixed input dimensions (name, value)
    dims: Vec<(String, i64)>,
    sess: ort::session::Session,
    gpu: bool,
}

fn tile_dims(tile: usize) -> Vec<(String, i64)> {
    [("batch", 1), ("height", tile as i64), ("width", tile as i64), ("h", tile as i64), ("w", tile as i64)].iter().map(|(n, v)| (n.to_string(), *v)).collect()
}

static SESSIONS: std::sync::Mutex<Vec<Cached>> = std::sync::Mutex::new(Vec::new());

pub(crate) fn with_session<R>(model: &Path, tile: usize, f: impl FnOnce(&mut ort::session::Session, bool) -> Result<R>) -> Result<R> {
    with_dims(model, tile_dims(tile), f)
}

/// Get a session with explicitly named input dimensions (e.g. SegFormer: batch_size, height, width).
pub(crate) fn with_session_dims<R>(model: &Path, dims: &[(&str, i64)], f: impl FnOnce(&mut ort::session::Session, bool) -> Result<R>) -> Result<R> {
    with_dims(model, dims.iter().map(|(n, v)| (n.to_string(), *v)).collect(), f)
}

fn with_dims<R>(model: &Path, dims: Vec<(String, i64)>, f: impl FnOnce(&mut ort::session::Session, bool) -> Result<R>) -> Result<R> {
    let mut g = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    if !g.iter().any(|c| c.model == model && c.dims == dims) {
        let gpu_ok = std::env::var_os("DARKROOM_AI_CPU").is_none();
        let (sess, gpu) = new_session_dims(model, gpu_ok, &dims)?;
        g.push(Cached { model: model.to_path_buf(), dims: dims.clone(), sess, gpu });
    }
    let c = g.iter_mut().find(|c| c.model == model && c.dims == dims).unwrap();
    let gpu = c.gpu;
    f(&mut c.sess, gpu)
}

/// Release GPU memory (dialog closed or job finished).
pub fn release() {
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Create a session: GPU (DirectML) first, CPU as fallback. Returns (session, uses GPU).
/// Input dimensions are fixed to the tile size: dynamic-shape graphs can crash on DirectML,
/// while fixed shapes fold to constants and run faster and more reliably.
#[cfg(test)]
fn new_session(model: &Path, gpu: bool, tile: usize) -> Result<(ort::session::Session, bool)> {
    new_session_dims(model, gpu, &tile_dims(tile))
}

fn new_session_dims(model: &Path, gpu: bool, dims: &[(String, i64)]) -> Result<(ort::session::Session, bool)> {
    use ort::session::{Session, builder::SessionBuilder};
    init_runtime()?;
    let e = |e: ort::Error<SessionBuilder>| anyhow!("{e}");
    let fix = |b: SessionBuilder| -> Result<SessionBuilder> {
        let mut b = b;
        for (n, v) in dims {
            b = b.with_dimension_override(n, *v).map_err(e)?;
        }
        Ok(b)
    };
    if gpu {
        let r = Session::builder()
            .map_err(|e| anyhow!("{e}"))
            .and_then(fix)
            .and_then(|b| b.with_parallel_execution(false).map_err(e))
            .and_then(|b| b.with_memory_pattern(false).map_err(e))
            .and_then(|b| b.with_execution_providers([ort::ep::DirectML::default().with_device_id(0).build().error_on_failure()]).map_err(e))
            .and_then(|mut b| b.commit_from_file(model).map_err(|e| anyhow!("{e}")));
        match r {
            Ok(s) => return Ok((s, true)),
            Err(err) => eprintln!("DirectML 사용 불가, CPU로: {err:#}"),
        }
    }
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let b = fix(Session::builder().map_err(|e| anyhow!("{e}"))?)?;
    let mut b = b.with_intra_threads(threads).map_err(e)?;
    Ok((b.commit_from_file(model).map_err(|e| anyhow!("{}", trf!("모델 열기 실패: {e}", "Couldn't open model: {e}")))?, false))
}

/// Model input encoding: linear camera RGB (before white balance) -> white balance and exposure -> soft shoulder -> sRGB gamma.
/// Must be exactly invertible (`decode_px`).
#[derive(Clone, Copy)]
struct Codec {
    gain: [f32; 3],
}

const KNEE: f32 = 0.8;

impl Codec {
    fn new(neutral: [f64; 3], baseline_ev: f64) -> Self {
        let e = (baseline_ev as f32).exp2();
        let g = |n: f64| e / (n.max(1e-6) as f32);
        Self { gain: [g(neutral[0]), g(neutral[1]), g(neutral[2])] }
    }
    fn enc(&self, v: f32, c: usize) -> f32 {
        let x = (v * self.gain[c]).max(0.0);
        let y = if x <= KNEE { x } else { KNEE + (1.0 - KNEE) * (1.0 - (-(x - KNEE) / (1.0 - KNEE)).exp()) };
        crate::develop::color::srgb_encode(y.min(1.0))
    }
    fn dec(&self, e: f32, c: usize) -> f32 {
        let y = crate::develop::color::srgb_decode(e.clamp(0.0, 1.0));
        let x = if y <= KNEE {
            y
        } else {
            let t = ((y - KNEE) / (1.0 - KNEE)).min(0.9999);
            KNEE - (1.0 - KNEE) * (1.0 - t).ln()
        };
        x / self.gain[c]
    }
}

/// 2x upscale (Lanczos3, mirrored edges), used for back-projection.
fn up2(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    fn lanczos(x: f32) -> f32 {
        let x = x.abs();
        if x < 1e-6 {
            1.0
        } else if x >= 3.0 {
            0.0
        } else {
            let px = std::f32::consts::PI * x;
            3.0 * px.sin() * (px / 3.0).sin() / (px * px)
        }
    }
    // 6-tap weights at phase p from source base b (taps b-2..b+3)
    let taps = |p: f32| -> [f32; 6] {
        let mut t = [0.0f32; 6];
        for (k, v) in t.iter_mut().enumerate() {
            *v = lanczos(p - (k as f32 - 2.0));
        }
        let s: f32 = t.iter().sum();
        t.map(|v| v / s)
    };
    // Source position of output j is j/2 - 0.25: even j=2i -> i-1+0.75, odd j=2i+1 -> i+0.25
    let (te, to) = (taps(0.75), taps(0.25));
    let mir = |i: isize, n: usize| -> usize {
        let n = n as isize;
        let mut i = i;
        if i < 0 {
            i = -i - 1;
        }
        if i >= n {
            i = 2 * n - 1 - i;
        }
        i.clamp(0, n - 1) as usize
    };
    let pick = |j: usize| if j.is_multiple_of(2) { (j as isize / 2 - 1, &te) } else { (j as isize / 2, &to) };
    let w2 = w * 2;
    let mut tmp = vec![0.0f32; w2 * h * 3];
    tmp.par_chunks_mut(w2 * 3).enumerate().for_each(|(y, row)| {
        for j in 0..w2 {
            let (b, t) = pick(j);
            for c in 0..3 {
                let mut s = 0.0;
                for (k, wt) in t.iter().enumerate() {
                    s += wt * src[(y * w + mir(b + k as isize - 2, w)) * 3 + c];
                }
                row[j * 3 + c] = s;
            }
        }
    });
    let h2 = h * 2;
    let mut out = vec![0.0f32; w2 * h2 * 3];
    out.par_chunks_mut(w2 * 3).enumerate().for_each(|(j, row)| {
        let (b, t) = pick(j);
        for (x, o) in row.iter_mut().enumerate() {
            let mut s = 0.0;
            for (k, wt) in t.iter().enumerate() {
                s += wt * tmp[mir(b + k as isize - 2, h) * w2 * 3 + x];
            }
            *o = s;
        }
    });
    out
}

/// Tiled inference. `img`: encoded HWC values. Model scale `ms`, output scale `os` (a divisor of ms).
/// The outer `ov` pixels of each tile are used only as context and discarded, so there are no seams.
/// `bp`: back-projection, which adds back the difference so the downscaled result matches the input (restores texture the model removed).
#[allow(clippy::too_many_arguments)]
fn run_tiled(model: &Path, img: &[f32], w: usize, h: usize, core: usize, ov: usize, ms: usize, os: usize, bp: bool, progress: &dyn Fn(usize, usize, bool)) -> Result<Vec<f32>> {
    let t = core + 2 * ov;
    let (nx, ny) = (w.div_ceil(core), h.div_ceil(core));
    let (ow, oh) = (w * os, h * os);
    let mut out = vec![0.0f32; ow * oh * 3];
    let mirror = |i: isize, n: usize| -> usize {
        let n = n as isize;
        let mut i = i;
        if i < 0 {
            i = -i;
        }
        if i >= n {
            i = 2 * n - 2 - i;
        }
        i.clamp(0, n - 1) as usize
    };
    let k = ms / os;
    let to = t * os;
    for ty in 0..ny {
        for tx in 0..nx {
            let (cx0, cy0) = (tx * core, ty * core);
            let (cw, ch) = (core.min(w - cx0), core.min(h - cy0));
            let (x0, y0) = (cx0 as isize - ov as isize, cy0 as isize - ov as isize);
            // Input tile (HWC, for back-projection) and model input (NCHW)
            let mut tile = vec![0.0f32; t * t * 3];
            tile.par_chunks_mut(t * 3).enumerate().for_each(|(y, line)| {
                let sy = mirror(y0 + y as isize, h);
                for x in 0..t {
                    let sx = mirror(x0 + x as isize, w);
                    line[x * 3..x * 3 + 3].copy_from_slice(&img[(sy * w + sx) * 3..(sy * w + sx) * 3 + 3]);
                }
            });
            let mut buf = vec![0.0f32; 3 * t * t];
            for (i, px) in tile.chunks(3).enumerate() {
                for c in 0..3 {
                    buf[c * t * t + i] = px[c];
                }
            }
            let (mut full, gpu) = with_session(model, t, |sess, gpu| {
                let name = sess.inputs()[0].name().to_string();
                let tensor = ort::value::Tensor::from_array((vec![1i64, 3, t as i64, t as i64], buf)).map_err(|e| anyhow!("{e}"))?;
                let outs = sess.run(ort::inputs![name.as_str() => tensor]).map_err(|e| anyhow!("{}", trf!("추론 실패: {e}", "Inference failed: {e}")))?;
                let (shape, data) = outs[0].try_extract_tensor::<f32>().map_err(|e| anyhow!("{e}"))?;
                let (mh, mw) = (shape[2] as usize, shape[3] as usize);
                if mh != t * ms || mw != t * ms {
                    return Err(anyhow!("{}", trf!("모델 출력 크기가 예상과 다름 ({mw}×{mh})", "Unexpected model output size ({mw}×{mh})")));
                }
                // Box-downscale by factor k -> HWC (to x to)
                let plane = mh * mw;
                let inv = 1.0 / (k * k) as f32;
                let mut full = vec![0.0f32; to * to * 3];
                full.par_chunks_mut(to * 3).enumerate().for_each(|(oy, line)| {
                    for ox in 0..to {
                        for c in 0..3 {
                            let mut s = 0.0;
                            for dy in 0..k {
                                for dx in 0..k {
                                    s += data[c * plane + (oy * k + dy) * mw + ox * k + dx];
                                }
                            }
                            line[ox * 3 + c] = s * inv;
                        }
                    }
                });
                Ok((full, gpu))
            })?;
            if bp && os > 1 {
                // Residual = input - downscale(result), upscaled 2x and added back
                let mut res = vec![0.0f32; t * t * 3];
                res.par_chunks_mut(t * 3).enumerate().for_each(|(y, line)| {
                    for x in 0..t {
                        for c in 0..3 {
                            let mut s = 0.0;
                            for dy in 0..os {
                                for dx in 0..os {
                                    s += full[((y * os + dy) * to + x * os + dx) * 3 + c];
                                }
                            }
                            line[x * 3 + c] = tile[(y * t + x) * 3 + c] - s / (os * os) as f32;
                        }
                    }
                });
                let up = up2(&res, t, t);
                full.par_iter_mut().zip(up.par_iter()).for_each(|(f, u)| *f += u);
            }
            // Copy only the core region into the result
            for oy in 0..ch * os {
                let src = ((ov * os + oy) * to + ov * os) * 3;
                let dst = ((cy0 * os + oy) * ow + cx0 * os) * 3;
                out[dst..dst + cw * os * 3].copy_from_slice(&full[src..src + cw * os * 3]);
            }
            progress(ty * nx + tx + 1, nx * ny, gpu);
        }
    }
    Ok(out)
}

/// Tile settings per model (chosen from GPU timing).
const NR_TILE: (usize, usize) = (704, 32);
const SR_TILE: (usize, usize) = (256, 16);

/// Apply enhancement to linear camera RGB -> (result, uses GPU). progress(stage name, 0..1)
pub fn process(base: &LinearImage, neutral: [f64; 3], baseline_ev: f64, opts: &Options, progress: &dyn Fn(&str, f32)) -> Result<(LinearImage, bool)> {
    let codec = Codec::new(neutral, baseline_ev);
    let (w, h) = (base.w, base.h);
    let mut cur = base.data.clone();
    let gpu_flag = std::cell::Cell::new(false);
    if opts.denoise {
        let enc: Vec<f32> = cur.par_iter().enumerate().map(|(i, v)| codec.enc(*v, i % 3)).collect();
        let span = if opts.superres { 0.5 } else { 1.0 };
        let res = run_tiled(&denoise_model(), &enc, w, h, NR_TILE.0, NR_TILE.1, 1, 1, false, &|i, n, g| {
            gpu_flag.set(g);
            progress(tr!("AI 노이즈 감소", "AI Denoise"), span * i as f32 / n as f32)
        })?;
        let a = (opts.amount / 100.0).clamp(0.0, 1.0);
        cur = res.par_iter().zip(cur.par_iter()).enumerate().map(|(i, (e, o))| o + (codec.dec(*e, i % 3) - o) * a).collect();
    }
    if !opts.superres {
        return Ok((LinearImage { w, h, data: cur }, gpu_flag.get()));
    }
    let enc: Vec<f32> = cur.par_iter().enumerate().map(|(i, v)| codec.enc(*v, i % 3)).collect();
    drop(cur);
    let off = if opts.denoise { 0.5 } else { 0.0 };
    let res = run_tiled(&path_of(&UPSCALE[0]), &enc, w, h, SR_TILE.0, SR_TILE.1, 4, 2, true, &|i, n, g| {
        gpu_flag.set(g);
        progress(tr!("AI 해상도 향상", "AI Super Resolution"), off + (1.0 - off) * i as f32 / n as f32)
    })?;
    let data: Vec<f32> = res.par_iter().enumerate().map(|(i, e)| codec.dec(*e, i % 3)).collect();
    Ok((LinearImage { w: w * 2, h: h * 2, data }, gpu_flag.get()))
}

/// Crop `size` pixels around the normalized `center` of the source.
pub fn crop_square(base: &LinearImage, center: [f32; 2], size: usize) -> LinearImage {
    let s = size.min(base.w).min(base.h);
    let x0 = ((center[0] * base.w as f32) as isize - s as isize / 2).clamp(0, (base.w - s) as isize) as usize;
    let y0 = ((center[1] * base.h as f32) as isize - s as isize / 2).clamp(0, (base.h - s) as isize) as usize;
    let mut d = Vec::with_capacity(s * s * 3);
    for y in 0..s {
        d.extend_from_slice(&base.data[((y0 + y) * base.w + x0) * 3..((y0 + y) * base.w + x0 + s) * 3]);
    }
    LinearImage { w: s, h: s, data: d }
}

pub struct EnhanceResult {
    pub path: PathBuf,
    pub gpu: bool,
    pub secs: f32,
    pub w: usize,
    pub h: usize,
}

/// Source file -> enhanced file (.drhdr, orientation applied).
pub fn enhance(src_path: &Path, orientation: u16, opts: &Options, out: &Path, progress: &dyn Fn(&str, f32)) -> Result<EnhanceResult> {
    let t0 = std::time::Instant::now();
    progress(tr!("원본 읽는 중", "Reading original"), 0.0);
    let src = super::decode::decode_source(src_path, orientation)?;
    let rc = src.raw_color.clone().ok_or_else(|| anyhow!("{}", trf!("RAW 파일만 지원합니다", "Only RAW files are supported")))?;
    let mut base = src.levels[0].clone();
    // Test switch: DARKROOM_AI_CROP=<size> processes only a centered crop.
    if let Some(c) = std::env::var("DARKROOM_AI_CROP").ok().and_then(|v| v.parse::<usize>().ok()) {
        base = std::sync::Arc::new(crop_square(&base, [0.5, 0.5], c));
    }
    let (img, gpu) = process(&base, rc.neutral, rc.baseline_exposure, opts, progress)?;
    drop(base);
    progress(tr!("저장 중", "Saving"), 1.0);
    let li = src.lens_info.clone();
    drop(src);
    let cm = rc.fallback.cm1.unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    let hdr = super::hdr::Header {
        w: img.w,
        h: img.h,
        camera: rc.camera.clone(),
        neutral: rc.neutral,
        baseline_exposure: rc.baseline_exposure,
        fallback_cm: [cm[0][0], cm[0][1], cm[0][2], cm[1][0], cm[1][1], cm[1][2], cm[2][0], cm[2][1], cm[2][2]],
        frames: vec![src_path.to_string_lossy().to_string()],
        ev: vec![],
        lens: li.as_ref().map(|l| l.lens.clone()).unwrap_or_default(),
        make: li.as_ref().map(|l| l.make.clone()).unwrap_or_default(),
        focal: li.as_ref().map(|l| l.focal).unwrap_or(0.0),
        fnumber: li.as_ref().map(|l| l.fnumber).unwrap_or(0.0),
        enhance: opts.suffix().to_string(),
    };
    super::hdr::write(out, &hdr, &img)?;
    Ok(EnhanceResult { path: out.to_path_buf(), gpu, secs: t0.elapsed().as_secs_f32(), w: img.w, h: img.h })
}

/// New file name: <name>-Enhanced-NR.drhdr (-2, -3, ... on collision).
pub fn out_path(src: &Path, opts: &Options) -> PathBuf {
    let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let dir = src.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut out = dir.join(format!("{stem}-{}.drhdr", opts.suffix()));
    let mut n = 2;
    while out.exists() {
        out = dir.join(format!("{stem}-{}-{n}.drhdr", opts.suffix()));
        n += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Upscaling 2x then averaging 2x2 should nearly reproduce the input (why back-projection preserves the original).
    #[test]
    fn up2_roundtrip() {
        let (w, h) = (16usize, 12usize);
        let src: Vec<f32> = (0..w * h * 3).map(|i| ((i * 37 % 101) as f32 / 101.0).powf(1.5)).collect();
        let flat = vec![0.4f32; w * h * 3];
        let u = up2(&flat, w, h);
        assert!(u.iter().all(|v| (v - 0.4).abs() < 1e-5));
        let u = up2(&src, w, h);
        let mut err = 0.0f32;
        for y in 2..h - 2 {
            for x in 2..w - 2 {
                for c in 0..3 {
                    let q = |yy: usize, xx: usize| u[(yy * 2 * w + xx) * 3 + c];
                    let s = (q(2 * y, 2 * x) + q(2 * y, 2 * x + 1) + q(2 * y + 1, 2 * x) + q(2 * y + 1, 2 * x + 1)) / 4.0;
                    err = err.max((s - src[(y * w + x) * 3 + c]).abs());
                }
            }
        }
        assert!(err < 0.2, "{err}");
    }

    #[test]
    fn codec_roundtrip() {
        let c = Codec::new([0.5, 1.0, 0.7], 0.3);
        for ch in 0..3 {
            for v in [0.0f32, 1e-4, 0.01, 0.1, 0.4, 0.6, 0.9, 1.0] {
                let r = c.dec(c.enc(v, ch), ch);
                assert!((r - v).abs() < 2e-4 + v * 2e-3, "ch{ch} {v} -> {r}");
            }
        }
    }

    /// Debug dump of the model input (encoded full image): DARKROOM_AI_DUMP=<RAW> writes <RAW>.enc (u32 w, h + HWC f32).
    #[test]
    #[ignore]
    fn dump_encoded() {
        let Some(p) = std::env::var_os("DARKROOM_AI_DUMP") else { return };
        let p = PathBuf::from(p);
        let src = crate::imaging::decode::decode_source(&p, 1).unwrap();
        let rc = src.raw_color.clone().unwrap();
        let b = &src.levels[0];
        let c = Codec::new(rc.neutral, rc.baseline_exposure);
        let mut out = Vec::with_capacity(8 + b.data.len() * 4);
        out.extend_from_slice(&(b.w as u32).to_le_bytes());
        out.extend_from_slice(&(b.h as u32).to_le_bytes());
        for (i, v) in b.data.iter().enumerate() {
            out.extend_from_slice(&c.enc(*v, i % 3).to_le_bytes());
        }
        std::fs::write(p.with_extension("enc"), out).unwrap();
        eprintln!("gain {:?} neutral {:?} be {}", c.gain, rc.neutral, rc.baseline_exposure);
    }

    /// Live download test (network): DARKROOM_AI_DL=1, DARKROOM_AI_DIR=<empty folder>
    #[test]
    #[ignore]
    fn download_probe() {
        if std::env::var_os("DARKROOM_AI_DL").is_none() {
            return;
        }
        let opts = Options { denoise: true, superres: true, amount: 50.0 };
        let (m, b) = missing(&opts);
        eprintln!("missing {} files, ~{} MB", m.len(), b / 1_048_576);
        let last = std::cell::Cell::new(0u64);
        let t0 = std::time::Instant::now();
        download(&opts, &|a, t, f| {
            if a / 20_000_000 != last.get() {
                last.set(a / 20_000_000);
                eprintln!("{} / {} MB {f}", a / 1_048_576, t / 1_048_576);
            }
        })
        .unwrap();
        eprintln!("done in {:.0}s, missing now {}", t0.elapsed().as_secs_f32(), missing(&opts).0.len());
        // Downloading again does nothing
        assert!(missing(&opts).0.is_empty());
    }

    /// NAFNet conversion check: output must match the PyTorch export.
    /// DARKROOM_NAF_PTH=<.pth> DARKROOM_NAF_REF=<torch.onnx.export output> DARKROOM_AI_DIR=<runtime folder>
    #[test]
    #[ignore]
    fn nafnet_convert_matches_torch() {
        let (Some(pth), Some(rf)) = (std::env::var_os("DARKROOM_NAF_PTH"), std::env::var_os("DARKROOM_NAF_REF")) else { return };
        let t0 = std::time::Instant::now();
        let sd = super::super::torchpt::load(Path::new(&pth)).unwrap();
        eprintln!("{} tensors, {:.1}s", sd.len(), t0.elapsed().as_secs_f32());
        let bytes = super::super::onnxw::nafnet(&sd).unwrap();
        let ours = std::env::temp_dir().join("naf_ours.onnx");
        std::fs::write(&ours, &bytes).unwrap();
        eprintln!("onnx {} MB, {:.1}s", bytes.len() / 1_048_576, t0.elapsed().as_secs_f32());
        let t = 512usize;
        let x: Vec<f32> = (0..3 * t * t).map(|i| (i * 7919 % 1000) as f32 / 1000.0).collect();
        let run = |m: &Path| -> Vec<f32> {
            let (mut s, gpu) = new_session(m, true, t).unwrap();
            eprintln!("gpu {gpu}");
            let name = s.inputs()[0].name().to_string();
            let tensor = ort::value::Tensor::from_array((vec![1i64, 3, t as i64, t as i64], x.clone())).unwrap();
            let o = s.run(ort::inputs![name.as_str() => tensor]).unwrap();
            o[0].try_extract_tensor::<f32>().unwrap().1.to_vec()
        };
        let a = run(&ours);
        let b = run(Path::new(&rf));
        let md = a.iter().zip(&b).map(|(p, q)| (p - q).abs()).fold(0.0f32, f32::max);
        eprintln!("max diff {md}");
        assert!(md < 1e-3);
    }

    /// Find high-ISO RAW files: DARKROOM_AI_SCAN=<folder>
    #[test]
    #[ignore]
    fn iso_scan() {
        let Some(d) = std::env::var_os("DARKROOM_AI_SCAN") else { return };
        let mut v = Vec::new();
        let mut stack = vec![PathBuf::from(d)];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().map(|x| x.eq_ignore_ascii_case("cr3") || x.eq_ignore_ascii_case("cr2")).unwrap_or(false) && v.len() < 1500 {
                    if let Some(m) = crate::imaging::meta::read_raw_meta(&p).or_else(|| crate::imaging::meta::read_exif(&p)) {
                        v.push((m.iso.unwrap_or(0), m.model.unwrap_or_default(), p));
                    }
                }
            }
        }
        v.sort_by(|a, b| b.0.cmp(&a.0));
        eprintln!("scanned {}", v.len());
        for (iso, m, p) in v.iter().take(15) {
            eprintln!("{iso} {m} {}", p.display());
        }
    }

    /// Test on a real RAW: DARKROOM_AI_TEST=<RAW path>, DARKROOM_AI_DIR=<ai folder>
    #[test]
    #[ignore]
    fn enhance_probe() {
        let Some(p) = std::env::var_os("DARKROOM_AI_TEST") else { return };
        let p = PathBuf::from(p);
        {
            let task = std::env::var("DARKROOM_AI_TASK").unwrap_or_else(|_| "nr".into());
            let amt: f32 = std::env::var("DARKROOM_AI_AMOUNT").ok().and_then(|v| v.parse().ok()).unwrap_or(100.0);
            let opts = Options { denoise: task.contains("nr"), superres: task.contains("sr"), amount: amt };
            let out = std::env::temp_dir().join(format!("ai_probe_{}_{amt}.drhdr", opts.suffix()));
            let last = std::cell::Cell::new(-1i32);
            let r = enhance(&p, 1, &opts, &out, &|m, f| {
                let pc = (f * 10.0) as i32;
                if pc != last.get() {
                    last.set(pc);
                    eprintln!("{m} {:.0}%", f * 100.0);
                }
            })
            .unwrap();
            eprintln!("{opts:?}: gpu {} {:.1}s -> {} ({}x{})", r.gpu, r.secs, r.path.display(), r.w, r.h);
            // Write preview PNGs with default develop settings (plus the same region of the source)
            let s = crate::develop::settings::DevelopSettings::default_for(true);
            let src = crate::imaging::decode::decode_source(&r.path, 1).unwrap();
            let img = crate::imaging::decode::render_preview_with(&src, &s, 4096);
            let png = out.with_extension("png");
            image::save_buffer(&png, &img.data, img.w, img.h, image::ExtendedColorType::Rgba8).unwrap();
            eprintln!("preview {}", png.display());
        }
    }
}

/// Live download check (network, Windows certificate store validation): DARKROOM_AI_DIR=<empty folder> cargo test ai_download_small -- --ignored
#[cfg(test)]
#[test]
#[ignore]
fn ai_download_small() {
    let a: &'static Asset = &super::people::FACE_MODELS[0];
    download_list(&[a], &|_, _, _| {}).expect("받기");
    assert!(present(a));
}
