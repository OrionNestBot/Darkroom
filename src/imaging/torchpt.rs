//! Reads PyTorch weights (.pth, zip + pickle), state dict (name -> tensor) only.
//! Never runs model code: interprets only the needed pickle opcodes and keeps any call other than tensor rebuilding as an inert value.

use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

pub struct Tensor {
    pub shape: Vec<usize>,
    pub data: Vec<f32>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
enum V {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Tuple(Vec<V>),
    List(Vec<V>),
    Dict(Vec<(V, V)>),
    Global(String),
    /// Storage reference: (key, dtype)
    Storage(String, String),
    /// Tensor: storage key, dtype, offset, shape, strides
    Tensor(String, String, usize, Vec<usize>, Vec<usize>),
    Object,
    Mark,
}

/// Zip entry name -> (start, compressed size, method)
fn zip_index(b: &[u8]) -> Result<HashMap<String, (usize, usize, u16)>> {
    let eocd = b.windows(4).rposition(|w| w == b"PK\x05\x06").ok_or_else(|| anyhow!("{}", trf!("zip 형식이 아님", "Not a zip file")))?;
    let u16le = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    let u32le = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as usize;
    let (mut n, mut p) = (u16le(eocd + 10), u32le(eocd + 16));
    // zip64 (large files): if the central directory offset is 0xFFFFFFFF, read it from the zip64 end record
    if p == 0xFFFF_FFFF || n == 0xFFFF {
        let loc = b[..eocd].windows(4).rposition(|w| w == b"PK\x06\x07").ok_or_else(|| anyhow!("{}", trf!("zip64 위치 없음", "zip64 locator missing")))?;
        let rec = u64::from_le_bytes(b[loc + 8..loc + 16].try_into().unwrap()) as usize;
        n = u64::from_le_bytes(b[rec + 32..rec + 40].try_into().unwrap()) as usize;
        p = u64::from_le_bytes(b[rec + 48..rec + 56].try_into().unwrap()) as usize;
    }
    let mut out = HashMap::new();
    for _ in 0..n {
        if u32le(p) != 0x0201_4b50 {
            return Err(anyhow!("{}", trf!("zip 디렉터리 손상", "zip directory damaged")));
        }
        let meth = u16le(p + 10) as u16;
        let mut csz = u32le(p + 20);
        let (fl, el, cl) = (u16le(p + 28), u16le(p + 30), u16le(p + 32));
        let mut off = u32le(p + 42);
        let name = String::from_utf8_lossy(&b[p + 46..p + 46 + fl]).to_string();
        // zip64 extended information field
        let mut e = p + 46 + fl;
        let eend = e + el;
        while e + 4 <= eend {
            let (id, sz) = (u16le(e), u16le(e + 2));
            if id == 1 {
                let mut q = e + 4;
                let usz = u32le(p + 24);
                if usz == 0xFFFF_FFFF {
                    q += 8;
                }
                if csz == 0xFFFF_FFFF {
                    csz = u64::from_le_bytes(b[q..q + 8].try_into().unwrap()) as usize;
                    q += 8;
                }
                if off == 0xFFFF_FFFF {
                    off = u64::from_le_bytes(b[q..q + 8].try_into().unwrap()) as usize;
                }
            }
            e += 4 + sz;
        }
        let start = off + 30 + u16le(off + 26) + u16le(off + 28);
        out.insert(name, (start, csz, meth));
        p += 46 + fl + el + cl;
    }
    Ok(out)
}

fn entry<'a>(b: &'a [u8], idx: &HashMap<String, (usize, usize, u16)>, name: &str) -> Result<std::borrow::Cow<'a, [u8]>> {
    let (s, n, m) = idx.get(name).copied().ok_or_else(|| anyhow!("{}", trf!("{name} 없음", "{name} not found")))?;
    match m {
        0 => Ok(std::borrow::Cow::Borrowed(&b[s..s + n])),
        8 => {
            let mut v = Vec::new();
            flate2::read::DeflateDecoder::new(&b[s..s + n]).read_to_end(&mut v)?;
            Ok(std::borrow::Cow::Owned(v))
        }
        m => Err(anyhow!("{}", trf!("지원하지 않는 압축 {m}", "Unsupported compression {m}"))),
    }
}

/// Pickle interpreter (only tensor rebuilding is special-cased)
fn unpickle(p: &[u8]) -> Result<V> {
    let mut st: Vec<V> = Vec::new();
    let mut memo: HashMap<usize, V> = HashMap::new();
    let mut i = 0usize;
    let rd = |i: &mut usize, n: usize| -> Result<&[u8]> {
        let s = p.get(*i..*i + n).ok_or_else(|| anyhow!("{}", trf!("pickle 잘림", "pickle truncated")))?;
        *i += n;
        Ok(s)
    };
    let pop_mark = |st: &mut Vec<V>| -> Vec<V> {
        let k = st.iter().rposition(|v| matches!(v, V::Mark)).unwrap_or(0);
        let items = st.split_off(k + 1);
        st.pop();
        items
    };
    loop {
        let op = *p.get(i).ok_or_else(|| anyhow!("{}", trf!("pickle 끝 없음", "pickle has no end")))?;
        i += 1;
        match op {
            0x80 => {
                rd(&mut i, 1)?;
            }
            b'}' => st.push(V::Dict(Vec::new())),
            b']' => st.push(V::List(Vec::new())),
            b')' => st.push(V::Tuple(Vec::new())),
            b'(' => st.push(V::Mark),
            b'N' => st.push(V::None),
            0x88 => st.push(V::Bool(true)),
            0x89 => st.push(V::Bool(false)),
            b'K' => st.push(V::Int(rd(&mut i, 1)?[0] as i64)),
            b'M' => {
                let b = rd(&mut i, 2)?;
                st.push(V::Int(u16::from_le_bytes([b[0], b[1]]) as i64))
            }
            b'J' => {
                let b = rd(&mut i, 4)?;
                st.push(V::Int(i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64))
            }
            0x8a => {
                let n = rd(&mut i, 1)?[0] as usize;
                let b = rd(&mut i, n)?;
                let mut v: i64 = 0;
                for (k, x) in b.iter().enumerate().take(8) {
                    v |= (*x as i64) << (8 * k);
                }
                if n < 8 && n > 0 && b[n - 1] & 0x80 != 0 {
                    v -= 1i64 << (8 * n);
                }
                st.push(V::Int(v))
            }
            b'G' => {
                let b = rd(&mut i, 8)?;
                st.push(V::Float(f64::from_be_bytes(b.try_into().unwrap())))
            }
            b'X' | b'T' => {
                let b = rd(&mut i, 4)?;
                let n = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
                st.push(V::Str(String::from_utf8_lossy(rd(&mut i, n)?).to_string()))
            }
            b'U' | 0x8c => {
                let n = rd(&mut i, 1)?[0] as usize;
                st.push(V::Str(String::from_utf8_lossy(rd(&mut i, n)?).to_string()))
            }
            b'c' => {
                let line = |i: &mut usize| -> Result<String> {
                    let s = *i;
                    while *p.get(*i).ok_or_else(|| anyhow!("{}", trf!("pickle 잘림", "pickle truncated")))? != b'\n' {
                        *i += 1;
                    }
                    let t = String::from_utf8_lossy(&p[s..*i]).to_string();
                    *i += 1;
                    Ok(t)
                };
                let m = line(&mut i)?;
                let n = line(&mut i)?;
                st.push(V::Global(format!("{m}.{n}")))
            }
            0x93 => {
                let n = st.pop();
                let m = st.pop();
                match (m, n) {
                    (Some(V::Str(m)), Some(V::Str(n))) => st.push(V::Global(format!("{m}.{n}"))),
                    _ => return Err(anyhow!("{}", trf!("STACK_GLOBAL 형식 오류", "STACK_GLOBAL format error"))),
                }
            }
            b'q' => {
                let k = rd(&mut i, 1)?[0] as usize;
                memo.insert(k, st.last().cloned().unwrap_or(V::None));
            }
            b'r' => {
                let b = rd(&mut i, 4)?;
                memo.insert(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize, st.last().cloned().unwrap_or(V::None));
            }
            0x94 => {
                let k = memo.len();
                memo.insert(k, st.last().cloned().unwrap_or(V::None));
            }
            b'h' => {
                let k = rd(&mut i, 1)?[0] as usize;
                st.push(memo.get(&k).cloned().ok_or_else(|| anyhow!("{}", trf!("memo {k} 없음", "memo {k} missing")))?)
            }
            b'j' => {
                let b = rd(&mut i, 4)?;
                let k = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
                st.push(memo.get(&k).cloned().ok_or_else(|| anyhow!("{}", trf!("memo {k} 없음", "memo {k} missing")))?)
            }
            b't' => {
                let items = pop_mark(&mut st);
                st.push(V::Tuple(items))
            }
            0x85..=0x87 => {
                let n = (op - 0x84) as usize;
                let items = st.split_off(st.len().saturating_sub(n));
                st.push(V::Tuple(items))
            }
            b'Q' => {
                // Storage: ('storage', dtype, key, location, count)
                let pid = st.pop().unwrap_or(V::None);
                match pid {
                    V::Tuple(t) if t.len() >= 3 => {
                        let dtype = match &t[1] {
                            V::Global(g) => g.clone(),
                            _ => String::new(),
                        };
                        let key = match &t[2] {
                            V::Str(s) => s.clone(),
                            V::Int(n) => n.to_string(),
                            _ => String::new(),
                        };
                        st.push(V::Storage(key, dtype))
                    }
                    _ => st.push(V::Object),
                }
            }
            b'R' => {
                let args = st.pop().unwrap_or(V::None);
                let f = st.pop().unwrap_or(V::None);
                let v = match (&f, args) {
                    (V::Global(g), V::Tuple(a)) if g.ends_with("_rebuild_tensor_v2") || g.ends_with("_rebuild_tensor") => {
                        let ints = |v: &V| -> Vec<usize> {
                            match v {
                                V::Tuple(x) | V::List(x) => x.iter().map(|e| if let V::Int(n) = e { *n as usize } else { 0 }).collect(),
                                _ => Vec::new(),
                            }
                        };
                        match (a.first(), a.get(1)) {
                            (Some(V::Storage(k, dt)), Some(V::Int(off))) => V::Tensor(k.clone(), dt.clone(), *off as usize, a.get(2).map(ints).unwrap_or_default(), a.get(3).map(ints).unwrap_or_default()),
                            _ => V::Object,
                        }
                    }
                    (V::Global(g), _) if g.ends_with("OrderedDict") => V::Dict(Vec::new()),
                    _ => V::Object,
                };
                st.push(v)
            }
            b'b' => {
                st.pop(); // State is ignored
            }
            b's' => {
                let v = st.pop().unwrap_or(V::None);
                let k = st.pop().unwrap_or(V::None);
                if let Some(V::Dict(d)) = st.last_mut() {
                    d.push((k, v));
                }
            }
            b'u' => {
                let items = pop_mark(&mut st);
                if let Some(V::Dict(d)) = st.last_mut() {
                    for kv in items.chunks(2) {
                        if kv.len() == 2 {
                            d.push((kv[0].clone(), kv[1].clone()));
                        }
                    }
                }
            }
            b'a' => {
                let v = st.pop().unwrap_or(V::None);
                if let Some(V::List(l)) = st.last_mut() {
                    l.push(v);
                }
            }
            b'e' => {
                let items = pop_mark(&mut st);
                if let Some(V::List(l)) = st.last_mut() {
                    l.extend(items);
                }
            }
            b'.' => return st.pop().ok_or_else(|| anyhow!("{}", trf!("pickle 비어 있음", "pickle is empty"))),
            o => return Err(anyhow!("{}", trf!("지원하지 않는 pickle 명령 0x{o:02x}", "Unsupported pickle opcode 0x{o:02x}"))),
        }
    }
}

/// Read the state dict. If the top level is a wrapper like {'params': {...}}, use the inner dict that holds tensors
pub fn load(path: &Path) -> Result<HashMap<String, Tensor>> {
    let b = std::fs::read(path).with_context(|| trf!("읽기 실패: {}", "Read failed: {}", path.display()))?;
    let idx = zip_index(&b)?;
    let pkl = idx.keys().find(|k| k.ends_with("data.pkl")).cloned().ok_or_else(|| anyhow!("{}", trf!("data.pkl 없음", "data.pkl missing")))?;
    let prefix = pkl.trim_end_matches("data.pkl").to_string();
    let root = unpickle(&entry(&b, &idx, &pkl)?)?;
    fn find(v: &V) -> Option<&Vec<(V, V)>> {
        if let V::Dict(d) = v {
            if d.iter().any(|(_, x)| matches!(x, V::Tensor(..))) {
                return Some(d);
            }
            for (k, x) in d {
                if matches!(k, V::Str(s) if s == "params" || s == "state_dict" || s == "model")
                    && let Some(r) = find(x) {
                        return Some(r);
                    }
            }
            for (_, x) in d {
                if let Some(r) = find(x) {
                    return Some(r);
                }
            }
        }
        None
    }
    let dict = find(&root).ok_or_else(|| anyhow!("{}", trf!("텐서 사전을 찾지 못함", "Tensor dictionary not found")))?;
    let mut out = HashMap::new();
    for (k, v) in dict {
        let (V::Str(name), V::Tensor(key, dtype, off, shape, stride)) = (k, v) else { continue };
        if !dtype.ends_with("FloatStorage") {
            return Err(anyhow!("{}", trf!("{name}: float32가 아님 ({dtype})", "{name}: not float32 ({dtype})")));
        }
        // Only contiguous layouts are supported
        let mut s = 1usize;
        for (d, st) in shape.iter().zip(stride.iter()).rev() {
            if *d > 1 && *st != s {
                return Err(anyhow!("{}", trf!("{name}: 연속 텐서가 아님", "{name}: not a contiguous tensor")));
            }
            s *= d;
        }
        let n: usize = shape.iter().product::<usize>().max(1);
        let raw = entry(&b, &idx, &format!("{prefix}data/{key}"))?;
        let bytes = raw.get(off * 4..(off + n) * 4).ok_or_else(|| anyhow!("{}", trf!("{name}: 데이터 범위 밖", "{name}: data out of range")))?;
        let data: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        out.insert(name.clone(), Tensor { shape: shape.clone(), data });
    }
    Ok(out)
}
