//! Lens correction data the camera writes into the RAW file (built-in correction): Sony, Fujifilm, Olympus/OM, Panasonic, Nikon Z, DNG opcodes.
//! Tag locations and value meanings follow public descriptions (darktable lens.cc / exif.cc, DNG specification); no code was copied.
//!
//! Model (coordinates centered on the image, radius 1 = half the image diagonal):
//!   distortion/CA: at each output-radius knot `kd`, the ratio source radius / output radius `cor[R,G,B]` (linear interpolation, clamped at the ends)
//!   vignetting: at each source-radius knot `kv`, the source brightness ratio `vig` (corrected = value / vig)
//! © 2026 OrionNest

use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Embedded {
    /// Data source (for display): "Sony", "Fujifilm", "Olympus", "Panasonic", "DNG"
    pub maker: &'static str,
    pub kd: Vec<f64>,
    pub cor: [Vec<f64>; 3],
    pub kv: Vec<f64>,
    pub vig: Vec<f64>,
    pub has_dist: bool,
    pub has_ca: bool,
    pub has_vig: bool,
    /// Default amount (d, c, v) applied when the file itself marks correction as on (Nikon Z)
    pub auto: Option<&'static str>,
    /// In-camera vignette control level (Nikon makernote 0x002a: 0 off, 1 low, 3 normal, 5 high)
    pub vig_control: Option<u8>,
}

/// Linear interpolation (end values outside the knots)
fn interp(k: &[f64], v: &[f64], x: f64) -> f64 {
    let n = k.len().min(v.len());
    if n == 0 {
        return 1.0;
    }
    if x <= k[0] {
        return v[0];
    }
    for i in 1..n {
        if x <= k[i] {
            let d = k[i] - k[i - 1];
            return if d > 0.0 { v[i - 1] + (x - k[i - 1]) * (v[i] - v[i - 1]) / d } else { v[i] };
        }
    }
    v[n - 1]
}

impl Embedded {
    /// Output radius → source radius ratio (channel 0 R, 1 G, 2 B)
    #[inline]
    pub fn cor_at(&self, c: usize, r: f64) -> f64 {
        interp(&self.kd, &self.cor[c], r)
    }
    /// Source radius → source brightness ratio (correction divides by it)
    #[inline]
    pub fn vig_at(&self, r: f64) -> f64 {
        interp(&self.kv, &self.vig, r)
    }
}

// ───────────────────────── TIFF reading ─────────────────────────

#[derive(Clone, Copy)]
struct T<'a> {
    b: &'a [u8],
    /// Base position for offsets
    base: usize,
    le: bool,
}

impl T<'_> {
    fn u16(&self, o: usize) -> Option<u16> {
        let s: [u8; 2] = self.b.get(self.base + o..self.base + o + 2)?.try_into().ok()?;
        Some(if self.le { u16::from_le_bytes(s) } else { u16::from_be_bytes(s) })
    }
    fn u32(&self, o: usize) -> Option<u32> {
        let s: [u8; 4] = self.b.get(self.base + o..self.base + o + 4)?.try_into().ok()?;
        Some(if self.le { u32::from_le_bytes(s) } else { u32::from_be_bytes(s) })
    }
    fn i16(&self, o: usize) -> Option<i16> {
        self.u16(o).map(|v| v as i16)
    }
    fn i32(&self, o: usize) -> Option<i32> {
        self.u32(o).map(|v| v as i32)
    }
    /// Finds an IFD entry → (type, count, value position relative to base); values of 4 bytes or less sit inside the entry
    fn find(&self, ifd: usize, tag: u16) -> Option<(u16, u32, usize)> {
        let n = self.u16(ifd)? as usize;
        for i in 0..n.min(1000) {
            let e = ifd + 2 + i * 12;
            if self.u16(e)? == tag {
                let (ty, cnt) = (self.u16(e + 2)?, self.u32(e + 4)?);
                let size = match ty {
                    1 | 2 | 6 | 7 => 1,
                    3 | 8 => 2,
                    4 | 9 | 11 | 13 => 4,
                    _ => 8,
                } * cnt as usize;
                let at = if size <= 4 { e + 8 } else { self.u32(e + 8)? as usize };
                return Some((ty, cnt, at));
            }
        }
        None
    }
    /// Numeric array (integer and rational types)
    fn nums(&self, ty: u16, cnt: u32, at: usize) -> Option<Vec<f64>> {
        (0..cnt as usize)
            .map(|i| {
                Some(match ty {
                    3 => self.u16(at + 2 * i)? as f64,
                    8 => self.i16(at + 2 * i)? as f64,
                    4 => self.u32(at + 4 * i)? as f64,
                    9 => self.i32(at + 4 * i)? as f64,
                    5 => {
                        let (n, d) = (self.u32(at + 8 * i)?, self.u32(at + 8 * i + 4)?);
                        if d == 0 { 0.0 } else { n as f64 / d as f64 }
                    }
                    10 => {
                        let (n, d) = (self.i32(at + 8 * i)?, self.i32(at + 8 * i + 4)?);
                        if d == 0 { 0.0 } else { n as f64 / d as f64 }
                    }
                    11 => f32::from_bits(self.u32(at + 4 * i)?) as f64,
                    12 => {
                        let (lo, hi) = (self.u32(at + 8 * i)? as u64, self.u32(at + 8 * i + 4)? as u64);
                        f64::from_bits(if self.le { lo | hi << 32 } else { hi | lo << 32 })
                    }
                    _ => return None,
                })
            })
            .collect()
    }
    fn tag(&self, ifd: usize, tag: u16) -> Option<Vec<f64>> {
        let (ty, cnt, at) = self.find(ifd, tag)?;
        self.nums(ty, cnt, at)
    }
}

fn tiff_at(b: &[u8], base: usize) -> Option<(T<'_>, usize)> {
    let h = b.get(base..base + 4)?;
    let le = match h {
        [b'I', b'I', _, _] => true,
        [b'M', b'M', _, _] => false,
        _ => return None,
    };
    let t = T { b, base, le };
    let ifd0 = t.u32(4)? as usize;
    Some((t, ifd0))
}

// ───────────────────────── Per camera maker ─────────────────────────

/// Reads built-in correction data from a file (None if absent or in an unknown format)
pub fn read(path: &Path) -> Option<Embedded> {
    use std::io::Read;
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    if !matches!(ext.as_str(), "arw" | "sr2" | "raf" | "orf" | "rw2" | "rwl" | "dng" | "nef" | "nrw") {
        return None;
    }
    let parse = |b: &[u8]| match ext.as_str() {
        "arw" | "sr2" => sony(b),
        "raf" => fuji(b),
        "orf" => olympus(b),
        "rw2" | "rwl" => panasonic(b),
        "dng" => dng(b),
        _ => nikon(b),
    };
    // The data is usually near the start: read the first 16 MB, and the whole file only if not found there
    const HEAD: u64 = 16 << 20;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut b = Vec::new();
    f.by_ref().take(HEAD).read_to_end(&mut b).ok()?;
    let e = match parse(&b) {
        Some(e) => e,
        None if len > HEAD => parse(&std::fs::read(path).ok()?)?,
        None => return None,
    };
    (e.has_dist || e.has_ca || e.has_vig).then_some(e)
}

/// Sony ARW: IFD0 › SubIFD(0x14a) › 0x7037 distortion, 0x7035 CA, 0x7032 vignetting (first value = knot count)
fn sony(b: &[u8]) -> Option<Embedded> {
    let (t, ifd0) = tiff_at(b, 0)?;
    let subs = t.tag(ifd0, 0x14a)?;
    for s in subs {
        let s = s as usize;
        let (Some(d), Some(c), Some(v)) = (t.tag(s, 0x7037), t.tag(s, 0x7035), t.tag(s, 0x7032)) else { continue };
        // Each table has its own knot count (usually equal, but e.g. RX10 IV uses 11 for distortion/CA and 16 for vignetting)
        let nc = *d.first()? as usize;
        let nv = *v.first()? as usize;
        if !(2..=16).contains(&nc) || !(2..=32).contains(&nv) || d.len() < nc + 1 || v.len() < nv + 1 {
            continue;
        }
        // CA only when it shares the distortion knots (red nc + blue nc)
        let ca_ok = c.first().copied()? as usize == 2 * nc && c.len() > 2 * nc;
        let kd: Vec<f64> = (0..nc).map(|i| (i as f64 + 0.5) / (nc - 1) as f64).collect();
        let kv: Vec<f64> = (0..nv).map(|i| (i as f64 + 0.5) / (nv - 1) as f64).collect();
        let g: Vec<f64> = (0..nc).map(|i| d[i + 1] * (-14f64).exp2() + 1.0).collect();
        let ca = |k: usize| if ca_ok { c[k] } else { 0.0 };
        let r: Vec<f64> = (0..nc).map(|i| g[i] * (ca(i + 1) * (-21f64).exp2() + 1.0)).collect();
        let bl: Vec<f64> = (0..nc).map(|i| g[i] * (ca(nc + i + 1) * (-21f64).exp2() + 1.0)).collect();
        let vig: Vec<f64> = (0..nv).map(|i| (0.5 - (v[i + 1] * (-13f64).exp2() - 1.0).exp2()).exp2()).collect();
        return Some(Embedded {
            maker: "Sony",
            has_dist: d[1..=nc].iter().any(|&x| x != 0.0),
            has_ca: ca_ok && c[1..=2 * nc].iter().any(|&x| x != 0.0),
            has_vig: v[1..=nv].iter().any(|&x| x != 0.0),
            kv,
            kd,
            cor: [r, g, bl],
            vig,
            auto: None,
            vig_control: None,
        });
    }
    None
}

/// Fujifilm RAF: TIFF at the CFA offset in header 0x64 (big-endian) › IFD0 › 0xf000 › 0xf00b distortion, 0xf00f CA, 0xf010 vignetting
/// Knots are given in source radius and are converted to output radius
fn fuji(b: &[u8]) -> Option<Embedded> {
    let cfa = u32::from_be_bytes(b.get(100..104)?.try_into().ok()?) as usize;
    let (t, ifd0) = tiff_at(b, cfa)?;
    let (_, _, at) = t.find(ifd0, 0xf000)?;
    let sub = t.u32(at)? as usize;
    let (d, c, v) = (t.tag(sub, 0xf00b)?, t.tag(sub, 0xf00f)?, t.tag(sub, 0xf010)?);
    // (knot, distortion %, red/blue CA, vignetting %)
    let (knots, dist, car, cab, vig): (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) = if d.len() == 19 && c.len() == 29 && v.len() == 19 {
        let nc = 9;
        if (0..nc).any(|i| d[i + 1] != c[i + 1] || d[i + 1] != v[i + 1]) {
            return None;
        }
        (d[1..=nc].to_vec(), d[10..10 + nc].to_vec(), c[10..10 + nc].to_vec(), c[19..19 + nc].to_vec(), v[10..10 + nc].to_vec())
    } else if d.len() == 23 && c.len() == 31 && v.len() == 23 {
        let nc = 11;
        if (0..nc).any(|i| d[i + 1] != v[i + 1] || (i > 0 && d[i + 1] != c[i])) {
            return None;
        }
        let car = (0..nc).map(|i| if i == 0 { 0.0 } else { c[i + 10] }).collect();
        let cab = (0..nc).map(|i| if i == 0 { 0.0 } else { c[i + 20] }).collect();
        (d[1..=nc].to_vec(), d[12..12 + nc].to_vec(), car, cab, v[12..12 + nc].to_vec())
    } else {
        return None;
    };
    // Add an identity knot at 0
    let (mut kin, mut m, mut cr, mut cb, mut kv, mut vg) = (vec![], vec![], vec![], vec![], vec![], vec![]);
    if knots[0] > 0.0 {
        kin.push(0.0);
        m.push(1.0);
        cr.push(0.0);
        cb.push(0.0);
        kv.push(0.0);
        vg.push(1.0);
    }
    for i in 0..knots.len() {
        kin.push(knots[i]);
        m.push(dist[i] / 100.0 + 1.0);
        cr.push(car[i]);
        cb.push(cab[i]);
        kv.push(knots[i]);
        vg.push((vig[i] / 100.0).max(0.05));
    }
    // Source radius rin → output radius rin / m
    let n = 48;
    let rmax = kin.last().copied().unwrap_or(1.0).max(1.0);
    let (mut kd, mut g, mut r, mut bl) = (vec![], vec![], vec![], vec![]);
    for i in 0..n {
        let rin = rmax * i as f64 / (n - 1) as f64;
        let mm = interp(&kin, &m, rin);
        kd.push(rin / mm);
        g.push(mm);
        r.push(mm * (interp(&kin, &cr, rin) + 1.0));
        bl.push(mm * (interp(&kin, &cb, rin) + 1.0));
    }
    Some(Embedded {
        maker: "Fujifilm",
        has_dist: dist.iter().any(|&x| x != 0.0),
        has_ca: car.iter().chain(cab.iter()).any(|&x| x != 0.0),
        has_vig: vig.iter().any(|&x| x != 100.0 && x != 0.0),
        kd,
        cor: [r, g, bl],
        kv,
        vig: vg,
        auto: None,
            vig_control: None,
    })
}

/// Evaluates the polynomial at knots evenly spaced over output radius 0..1
fn sampled(maker: &'static str, f: impl Fn(f64) -> [f64; 3], vig: Option<&dyn Fn(f64) -> f64>, has: (bool, bool)) -> Embedded {
    let n = 33;
    let kd: Vec<f64> = (0..n).map(|i| 1.1 * i as f64 / (n - 1) as f64).collect();
    let v: Vec<[f64; 3]> = kd.iter().map(|&r| f(r)).collect();
    Embedded {
        maker,
        cor: [v.iter().map(|x| x[0]).collect(), v.iter().map(|x| x[1]).collect(), v.iter().map(|x| x[2]).collect()],
        vig: kd.iter().map(|&r| vig.map(|g| g(r)).unwrap_or(1.0)).collect(),
        kv: kd.clone(),
        kd,
        has_dist: has.0,
        has_ca: has.1,
        has_vig: vig.is_some(),
        auto: None,
            vig_control: None,
    }
}

/// Olympus/OM ORF: makernote › ImageProcessing(0x2040) › 0x150a distortion (k2, k4, k6, corner radius), 0x150c CA
fn olympus(b: &[u8]) -> Option<Embedded> {
    // Makernote header ("OLYMPUS\0II\3\0" → IFD at +12, "OM SYSTEM\0\0\0II\4\0" → +16); offsets are relative to the makernote start
    let head = 4 << 20;
    let s = &b[..b.len().min(head)];
    let (mn, ifd) = if let Some(i) = find(s, b"OLYMPUS\0") {
        (i, 12)
    } else {
        let i = find(s, b"OM SYSTEM\0")?;
        (i, 16)
    };
    let le = match b.get(mn + ifd - 4..mn + ifd - 2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let t = T { b, base: mn, le };
    let (_, _, at) = t.find(ifd, 0x2040)?;
    let ip = t.u32(at)? as usize;
    let dist = t.tag(ip, 0x150a).filter(|v| v.len() == 4);
    let ca = t.tag(ip, 0x150c).filter(|v| v.len() == 6);
    let has_dist = dist.as_ref().map(|d| d[..3].iter().any(|&x| x != 0.0)).unwrap_or(false);
    let has_ca = ca.as_ref().map(|c| c.iter().any(|&x| x != 0.0)).unwrap_or(false);
    let (dk2, dk4, dk6, drs) = match (&dist, has_dist) {
        (Some(d), true) => (d[0], d[1], d[2], d[3]),
        _ => (0.0, 0.0, 0.0, 1.0),
    };
    let c = ca.filter(|_| has_ca).unwrap_or_else(|| vec![0.0; 6]);
    Some(sampled(
        "Olympus",
        |r| {
            let rs2 = (r * drs) * (r * drs);
            let g = drs * (1.0 + rs2 * (dk2 + rs2 * (dk4 + rs2 * dk6)));
            if r <= 0.0 {
                return [g, g, g];
            }
            let rd = g * r;
            let rd2 = rd * rd;
            [g + rd * (c[0] + rd2 * (c[1] + rd2 * c[2])) / r, g, g + rd * (c[3] + rd2 * (c[4] + rd2 * c[5])) / r]
        },
        None,
        (has_dist, has_ca),
    ))
}

/// Panasonic RW2: IFD0 0x0119 (16 x 16-bit values): Ru = Rd + s·(a Rd³ + b Rd⁵ + c Rd⁷)
fn panasonic(b: &[u8]) -> Option<Embedded> {
    let (t, ifd0) = tiff_at(b, 0)?;
    let (_, cnt, at) = t.find(ifd0, 0x0119)?;
    if cnt != 32 {
        return None;
    }
    let v: Vec<f64> = (0..16).map(|i| t.i16(at + 2 * i).map(|x| x as f64)).collect::<Option<_>>()?;
    if (v[7] as i32 & 0x0f) != 1 {
        return None;
    }
    let sc = 1.0 / (1.0 + v[5] / 32768.0);
    let (a, bb, c) = (v[8] / 32768.0, v[4] / 32768.0, v[11] / 32768.0);
    Some(sampled(
        "Panasonic",
        |r| {
            // Ru → Rd: fixed-point iteration
            let mut rd = r;
            for _ in 0..6 {
                let rd2 = rd * rd;
                let f = 1.0 + sc * (a * rd2 + bb * rd2 * rd2 + c * rd2 * rd2 * rd2);
                rd = if f > 1e-6 { r / f } else { r };
            }
            let k = if r > 0.0 { rd / r } else { 1.0 };
            [k, k, k]
        },
        None,
        (true, false),
    ))
}

/// DNG: WarpRectilinear(1) and FixVignetteRadial(3) in OpcodeList3(0xC74E); the center is assumed to be the image center
fn dng(b: &[u8]) -> Option<Embedded> {
    let (t, ifd0) = tiff_at(b, 0)?;
    // First one found in any raw IFD (IFD0 or SubIFD)
    let mut ifds = vec![ifd0];
    if let Some(s) = t.tag(ifd0, 0x14a) {
        ifds.extend(s.into_iter().map(|x| x as usize));
    }
    let (_, cnt, at) = ifds.iter().find_map(|&i| t.find(i, 0xC74E))?;
    let ops = b.get(at..at + cnt as usize)?;
    let be32 = |o: usize| -> Option<u32> { Some(u32::from_be_bytes(ops.get(o..o + 4)?.try_into().ok()?)) };
    let bef = |o: usize| -> Option<f64> { Some(f64::from_be_bytes(ops.get(o..o + 8)?.try_into().ok()?)) };
    let n = be32(0)?;
    let mut p = 4;
    let mut warp: Option<Vec<[f64; 4]>> = None;
    let mut vig: Option<[f64; 5]> = None;
    for _ in 0..n.min(64) {
        let (id, size) = (be32(p)?, be32(p + 12)? as usize);
        let q = p + 16;
        match id {
            1 => {
                let planes = be32(q)? as usize;
                let mut w = vec![];
                for k in 0..planes.min(3) {
                    let o = q + 4 + k * 48;
                    w.push([bef(o)?, bef(o + 8)?, bef(o + 16)?, bef(o + 24)?]);
                }
                warp = Some(w);
            }
            3 => vig = Some([bef(q)?, bef(q + 8)?, bef(q + 16)?, bef(q + 24)?, bef(q + 32)?]),
            _ => {}
        }
        p = q + size;
    }
    if warp.is_none() && vig.is_none() {
        return None;
    }
    let w = warp.clone().unwrap_or_else(|| vec![[1.0, 0.0, 0.0, 0.0]]);
    let poly = |c: &[f64; 4], r: f64| {
        let r2 = r * r;
        c[0] + c[1] * r2 + c[2] * r2 * r2 + c[3] * r2 * r2 * r2
    };
    let vf = vig.map(|k| {
        move |r: f64| {
            let r2 = r * r;
            1.0 / (1.0 + k[0] * r2 + k[1] * r2.powi(2) + k[2] * r2.powi(3) + k[3] * r2.powi(4) + k[4] * r2.powi(5))
        }
    });
    let has_ca = w.len() == 3 && (w[0] != w[1] || w[2] != w[1]);
    Some(sampled(
        "DNG",
        |r| {
            if w.len() == 3 {
                [poly(&w[0], r), poly(&w[1], r), poly(&w[2], r)]
            } else {
                let g = poly(&w[0], r);
                [g, g, g]
            }
        },
        vf.as_ref().map(|f| f as &dyn Fn(f64) -> f64),
        (warp.is_some(), has_ca),
    ))
}

/// Nikon Z NEF: NikonNEFInfo(0xc7d5) in SubIFD = "Nikon\0" + 4 bytes + TIFF; inside, 0x05 is distortion (rationals k1, k2, k3 at 0x14, 0x1c, 0x24;
/// 4th byte 3 = in-camera auto distortion correction on). The format is undocumented and was fitted against reference renders:
/// output → source radius ratio = 1 + k1 r² + k2 r⁴ + k3 r⁶ (r relative to half diagonal × `config::NIKON_DIST_SCALE`), applied only when marked on
fn nikon(b: &[u8]) -> Option<Embedded> {
    let (t, ifd0) = tiff_at(b, 0)?;
    let subs = t.tag(ifd0, 0x14a)?;
    let (blob_at, cnt) = subs.iter().find_map(|&s| t.find(s as usize, 0xc7d5).map(|(_, c, at)| (at, c as usize)))?;
    let blob = b.get(blob_at..blob_at + cnt)?;
    if !blob.starts_with(b"Nikon\0") {
        return None;
    }
    let (n, ifd) = tiff_at(blob, 10)?;
    let (_, c5, at5) = n.find(ifd, 0x05)?;
    if c5 < 0x2c {
        return None;
    }
    let on = *blob.get(10 + at5 + 4)? == 3;
    let k: Vec<f64> = [0x14usize, 0x1c, 0x24]
        .iter()
        .map(|&o| {
            let (nu, de) = (n.i32(at5 + o)?, n.i32(at5 + o + 4)?);
            Some(if de == 0 { 0.0 } else { nu as f64 / de as f64 })
        })
        .collect::<Option<_>>()?;
    let sc = crate::config::NIKON_DIST_SCALE;
    // Vignetting 0x06: four rational coefficients (16-byte stride from 0x14) and a strength (rational at 0x60, set by the in-camera vignette control; 0 when off)
    // Gain = 1 + strength · (c0 r^p + c1 r^2p + c2 r^3p + c3 r^4p), fitted against reference renders (p: config::NIKON_VIG_POW)
    let vig = n.find(ifd, 0x06).and_then(|(_, c6, at6)| {
        if c6 < 0x6c {
            return None;
        }
        let rat = |o: usize| -> Option<f64> {
            let (nu, de) = (n.i32(at6 + o)?, n.i32(at6 + o + 4)?);
            Some(if de == 0 { 0.0 } else { nu as f64 / de as f64 })
        };
        let cs = [rat(0x14)?, rat(0x24)?, rat(0x34)?, rat(0x44)?];
        let s = rat(0x60)?;
        (s > 0.0 && s <= 2.0 && cs.iter().any(|&x| x != 0.0)).then_some((cs, s))
    });
    let pw = std::env::var("DARKROOM_NIKON_VIG_POW").ok().and_then(|v| v.parse().ok()).unwrap_or(crate::config::NIKON_VIG_POW);
    let vf = vig.map(|(cs, s)| {
        move |r: f64| {
            let x = r.powf(pw);
            let g = 1.0 + s * (cs[0] * x + cs[1] * x * x + cs[2] * x * x * x + cs[3] * x * x * x * x);
            1.0 / g.max(0.05)
        }
    });
    let mut e = sampled(
        "Nikon",
        |r| {
            let r2 = (r * sc) * (r * sc);
            let g = 1.0 + k[0] * r2 + k[1] * r2 * r2 + k[2] * r2 * r2 * r2;
            [g, g, g]
        },
        vf.as_ref().map(|f| f as &dyn Fn(f64) -> f64),
        (k.iter().any(|&x| x != 0.0), false),
    );
    // Off by default: reference renders use this data for some lenses but not others, so enabling it does not help overall.
    // Test switch: DARKROOM_NIKON_VIG=1 enables it.
    let use_vig = std::env::var("DARKROOM_NIKON_VIG").map(|v| v == "1").unwrap_or(false) && e.has_vig;
    e.auto = match (on, use_vig) {
        (true, true) => Some("dv"),
        (true, false) => Some("d"),
        (false, true) => Some("v"),
        _ => None,
    };
    // Vignette control: makernote ("Nikon\0\2" + TIFF after 4 bytes) tag 0x002a
    e.vig_control = find(b, b"Nikon\0\x02").and_then(|i| {
        let (t, ifd) = tiff_at(b, i + 10)?;
        t.tag(ifd, 0x002a)?.first().map(|v| *v as u8)
    });
    Some(e)
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interp_ends() {
        let k = [0.0, 0.5, 1.0];
        let v = [1.0, 2.0, 4.0];
        assert_eq!(interp(&k, &v, -1.0), 1.0);
        assert_eq!(interp(&k, &v, 0.75), 3.0);
        assert_eq!(interp(&k, &v, 2.0), 4.0);
    }

    /// Prints a summary of the data read from each RAW in the sample folder (DARKROOM_EMB_DIR)
    #[test]
    #[ignore]
    fn embedded_probe() {
        let dir = std::env::var("DARKROOM_EMB_DIR").expect("DARKROOM_EMB_DIR");
        let mut ds: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).collect();
        ds.sort();
        for d in ds {
            let Ok(rd) = std::fs::read_dir(d.join("raw")) else { continue };
            for f in rd.flatten().map(|e| e.path()).take(2) {
                if f.extension().map(|e| e.eq_ignore_ascii_case("xmp")).unwrap_or(true) {
                    continue;
                }
                match read(&f) {
                    Some(e) => {
                        let c1 = e.cor_at(1, 1.0);
                        println!(
                            "{:40} {:>9} 왜곡 {} 색수차 {} 비네팅 {} | 모서리 비 {:.4} R/G {:.5} B/G {:.5} 모서리 밝기 {:.3}",
                            d.file_name().unwrap().to_string_lossy(),
                            e.maker,
                            e.has_dist,
                            e.has_ca,
                            e.has_vig,
                            c1,
                            e.cor_at(0, 1.0) / c1,
                            e.cor_at(2, 1.0) / c1,
                            e.vig_at(1.0)
                        )
                    }
                    None => println!("{:40} 없음", d.file_name().unwrap().to_string_lossy()),
                }
            }
        }
    }
}

// ───────────────────────── In-camera aspect ratio ─────────────────────────

/// DNG 1.4 DefaultUserCrop(0xC7B5): aspect/crop chosen in camera (e.g. Leica aspect modes, digital zoom), relative to the default crop area,
/// as (top, left, bottom, right) 0..1, used as the default crop. None if it covers the whole frame (0,0,1,1)
pub fn dng_user_crop(path: &Path) -> Option<[f64; 4]> {
    use std::io::Read;
    if !path.extension()?.to_string_lossy().eq_ignore_ascii_case("dng") {
        return None;
    }
    let mut b = Vec::new();
    std::fs::File::open(path).ok()?.take(8 << 20).read_to_end(&mut b).ok()?;
    let (t, ifd0) = tiff_at(&b, 0)?;
    let mut ifds = vec![ifd0];
    if let Some(s) = t.tag(ifd0, 0x14a) {
        ifds.extend(s.into_iter().map(|x| x as usize));
    }
    let (ty, cnt, at) = ifds.iter().find_map(|&i| t.find(i, 0xC7B5))?;
    let v = t.nums(ty, cnt, at).filter(|v| v.len() == 4)?;
    let r = [v[0], v[1], v[2], v[3]];
    let ok = r.iter().all(|v| (0.0..=1.0).contains(v)) && r[2] - r[0] > 0.05 && r[3] - r[1] > 0.05;
    let full = r[0] < 1e-4 && r[1] < 1e-4 && r[2] > 1.0 - 1e-4 && r[3] > 1.0 - 1e-4;
    (ok && !full).then_some(r)
}

/// Aspect ratio chosen in camera (1:1, 4:3, 16:9, 5:4 …) as a (width, height) ratio in sensor orientation.
/// Used as the default centered crop; the caller ignores it when it equals the native ratio.
pub fn camera_aspect(path: &Path) -> Option<(f64, f64)> {
    use std::io::Read;
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let mut f = std::fs::File::open(path).ok()?;
    let mut b = Vec::new();
    f.by_ref().take(8 << 20).read_to_end(&mut b).ok()?;
    let r = match ext.as_str() {
        "arw" => {
            // SonyCropSize(0x74c8) in the raw IFD (SubIFD)
            let (t, ifd0) = tiff_at(&b, 0)?;
            t.tag(ifd0, 0x14a)?.into_iter().find_map(|s| t.tag(s as usize, 0x74c8)).filter(|v| v.len() == 2).map(|v| (v[0], v[1]))
        }
        "rw2" | "rwl" => {
            // Crop top/left/bottom/right (0x2f–0x32)
            let (t, ifd0) = tiff_at(&b, 0)?;
            let g = |tag| t.tag(ifd0, tag).and_then(|v| v.first().copied());
            Some((g(0x32)? - g(0x30)?, g(0x31)? - g(0x2f)?))
        }
        "raf" => {
            // RawImageAspectRatio(0x115) in the header directory (offset 0x5C, big-endian) = (height, width)
            let ho = u32::from_be_bytes(b.get(92..96)?.try_into().ok()?) as usize;
            let n = u32::from_be_bytes(b.get(ho..ho + 4)?.try_into().ok()?) as usize;
            let mut q = ho + 4;
            let mut out = None;
            for _ in 0..n.min(256) {
                let t = u16::from_be_bytes(b.get(q..q + 2)?.try_into().ok()?);
                let sz = u16::from_be_bytes(b.get(q + 2..q + 4)?.try_into().ok()?) as usize;
                if t == 0x115 && sz == 4 {
                    let h = u16::from_be_bytes(b.get(q + 4..q + 6)?.try_into().ok()?) as f64;
                    let w = u16::from_be_bytes(b.get(q + 6..q + 8)?.try_into().ok()?) as f64;
                    out = Some((w, h));
                }
                q += 4 + sz;
            }
            out
        }
        "nef" | "nrw" => {
            // Makernote ("Nikon\0\2" + TIFF after 4 bytes) CropHiSpeed(0x1b) = (mode, full width, full height, width, height, left, top)
            let i = find(&b, b"Nikon\0\x02")?;
            let (t, ifd) = tiff_at(&b, i + 10)?;
            let v = t.tag(ifd, 0x1b).filter(|v| v.len() >= 5)?;
            (v[3] > 0.0 && v[4] > 0.0).then(|| (v[3], v[4]))
        }
        "cr3" | "cr2" => {
            // Makernote AspectInfo(0x9a) = (ratio id, width, height, left, top); id 0 is 3:2 (default)
            let (t, ifd) = if ext == "cr3" {
                let p = find(&b, b"CMT3")?;
                tiff_at(&b, p + 4)?
            } else {
                let (t, ifd0) = tiff_at(&b, 0)?;
                let (_, _, exif) = t.find(ifd0, 0x8769)?;
                let exif = t.u32(exif)? as usize;
                let (_, _, mn) = t.find(exif, 0x927C)?;
                (t, mn)
            };
            let v = t.tag(ifd, 0x9a).filter(|v| v.len() >= 3)?;
            (v[0] != 0.0 && v[1] > 0.0 && v[2] > 0.0).then(|| (v[1], v[2]))
        }
        "orf" => {
            // Makernote › ImageProcessing(0x2040) › AspectFrame(0x1113) = (left, top, right, bottom)
            let (mn, ifd) = if let Some(i) = find(&b, b"OLYMPUS\0") {
                (i, 12)
            } else {
                (find(&b, b"OM SYSTEM\0")?, 16)
            };
            let le = b.get(mn + ifd - 4..mn + ifd - 2)? == b"II";
            let t = T { b: &b, base: mn, le };
            let (_, _, at) = t.find(ifd, 0x2040)?;
            let ip = t.u32(at)? as usize;
            let v = t.tag(ip, 0x1113).filter(|v| v.len() == 4)?;
            Some((v[2] - v[0] + 1.0, v[3] - v[1] + 1.0))
        }
        _ => None,
    }?;
    (r.0 > 0.0 && r.1 > 0.0).then_some(r)
}

// ───────────────────────── Exposure offset for shooting modes ─────────────────────────

/// Modes where the camera deliberately underexposes to protect highlights; the image is brightened by that amount (EV).
/// Canon Highlight Tone Priority (LightingOpt 0x4018 [3] ≥ 1): +1.
/// Fujifilm DR200/DR400 (0x1403, or 0x140b when auto) and D-Range Priority (0x1444 auto / 0x1445 fixed: weak 1, strong 2): +1/+2.
pub fn mode_exposure_offset(path: &Path) -> f64 {
    use std::io::Read;
    let Some(ext) = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()) else { return 0.0 };
    let mut b = Vec::new();
    let Ok(f) = std::fs::File::open(path) else { return 0.0 };
    if f.take(2 << 20).read_to_end(&mut b).is_err() {
        return 0.0;
    }
    let r = match ext.as_str() {
        "cr3" | "cr2" => (|| {
            let (t, ifd) = if ext == "cr3" {
                tiff_at(&b, find(&b, b"CMT3")? + 4)?
            } else {
                let (t, ifd0) = tiff_at(&b, 0)?;
                let (_, _, exif) = t.find(ifd0, 0x8769)?;
                let exif = t.u32(exif)? as usize;
                let (_, _, mn) = t.find(exif, 0x927C)?;
                (t, mn)
            };
            let v = t.tag(ifd, 0x4018)?;
            Some(if v.get(3).copied().unwrap_or(0.0) >= 1.0 { 1.0 } else { 0.0 })
        })(),
        "raf" => (|| {
            // Makernote: "FUJIFILM" + 4-byte IFD offset (relative to the makernote, little-endian)
            let mut i = 100;
            let mn = loop {
                let p = i + find(&b[i..], b"FUJIFILM")?;
                let off = u32::from_le_bytes(b.get(p + 8..p + 12)?.try_into().ok()?) as usize;
                if (9..64).contains(&off) {
                    break p;
                }
                i = p + 8;
            };
            let t = T { b: &b, base: mn, le: true };
            let ifd = t.u32(8)? as usize;
            let g = |tag: u16| t.tag(ifd, tag).and_then(|v| v.first().copied());
            let dr = g(0x1403).or_else(|| g(0x140b)).filter(|&d| d >= 100.0);
            if let Some(d) = dr {
                return Some((d / 100.0).log2());
            }
            let p = g(0x1444).or_else(|| g(0x1445)).unwrap_or(0.0);
            Some(if (1.0..=2.0).contains(&p) { p } else { 0.0 })
        })(),
        _ => None,
    };
    r.unwrap_or(0.0)
}

/// Whether the camera already applied vignetting correction to the RAW data (Panasonic makernote 0x008a or Olympus CameraSettings 0x050c is 1).
/// If so, the lens profile vignetting correction is skipped so the corners are not brightened twice.
pub fn shading_compensated(path: &Path) -> bool {
    use std::io::Read;
    let Some(ext) = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()) else { return false };
    if !matches!(ext.as_str(), "rw2" | "rwl" | "orf") {
        return false;
    }
    let mut b = Vec::new();
    let Ok(f) = std::fs::File::open(path) else { return false };
    if f.take(6 << 20).read_to_end(&mut b).is_err() {
        return false;
    }
    let r = (|| -> Option<bool> {
        if ext == "orf" {
            let (mn, ifd) = if let Some(i) = find(&b, b"OLYMPUS\0") { (i, 12) } else { (find(&b, b"OM SYSTEM\0")?, 16) };
            let le = b.get(mn + ifd - 4..mn + ifd - 2)? == b"II";
            let t = T { b: &b, base: mn, le };
            let (_, _, at) = t.find(ifd, 0x2020)?;
            let cs = t.u32(at)? as usize;
            Some(t.tag(cs, 0x050c)?.first().copied()? == 1.0)
        } else {
            let mn = find(&b, b"Panasonic\0\0\0")?;
            let t = T { b: &b, base: mn, le: true };
            Some(t.tag(12, 0x008a)?.first().copied()? == 1.0)
        }
    })();
    r.unwrap_or(false)
}
