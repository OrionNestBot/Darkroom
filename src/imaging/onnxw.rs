//! Minimal ONNX writer plus the NAFNet graph (weights -> .onnx).
//! Encodes protobuf directly, only the fields needed (ModelProto/GraphProto/NodeProto/TensorProto/AttributeProto/ValueInfoProto).

use super::torchpt::Tensor;
use anyhow::{Result, anyhow};
use std::collections::HashMap;

// ── protobuf ──
fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}
fn key(out: &mut Vec<u8>, field: u32, wire: u8) {
    varint(out, ((field as u64) << 3) | wire as u64);
}
fn f_int(out: &mut Vec<u8>, field: u32, v: i64) {
    key(out, field, 0);
    varint(out, v as u64);
}
fn f_bytes(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    key(out, field, 2);
    varint(out, b.len() as u64);
    out.extend_from_slice(b);
}
fn f_str(out: &mut Vec<u8>, field: u32, s: &str) {
    f_bytes(out, field, s.as_bytes());
}
fn f_float(out: &mut Vec<u8>, field: u32, v: f32) {
    key(out, field, 5);
    out.extend_from_slice(&v.to_le_bytes());
}

#[allow(dead_code)]
enum Attr {
    Int(i64),
    Ints(Vec<i64>),
    Float(f32),
    Str(String),
}

fn attr(name: &str, a: &Attr) -> Vec<u8> {
    let mut o = Vec::new();
    f_str(&mut o, 1, name);
    match a {
        Attr::Float(f) => {
            f_float(&mut o, 2, *f);
            f_int(&mut o, 20, 1);
        }
        Attr::Int(i) => {
            f_int(&mut o, 3, *i);
            f_int(&mut o, 20, 2);
        }
        Attr::Str(s) => {
            f_bytes(&mut o, 4, s.as_bytes());
            f_int(&mut o, 20, 3);
        }
        Attr::Ints(v) => {
            for i in v {
                f_int(&mut o, 8, *i);
            }
            f_int(&mut o, 20, 7);
        }
    }
    o
}

/// Graph builder.
pub struct Graph {
    nodes: Vec<Vec<u8>>,
    inits: Vec<Vec<u8>>,
    n: usize,
}

impl Graph {
    fn new() -> Self {
        Self { nodes: Vec::new(), inits: Vec::new(), n: 0 }
    }
    fn node(&mut self, op: &str, inputs: &[&str], attrs: &[(&str, Attr)]) -> String {
        self.n += 1;
        let out = format!("t{}", self.n);
        let mut o = Vec::new();
        for i in inputs {
            f_str(&mut o, 1, i);
        }
        f_str(&mut o, 2, &out);
        f_str(&mut o, 3, &format!("n{}", self.n));
        f_str(&mut o, 4, op);
        for (k, a) in attrs {
            f_bytes(&mut o, 5, &attr(k, a));
        }
        self.nodes.push(o);
        out
    }
    fn init_f32(&mut self, name: &str, dims: &[usize], data: &[f32]) -> String {
        let mut o = Vec::new();
        for d in dims {
            f_int(&mut o, 1, *d as i64);
        }
        f_int(&mut o, 2, 1); // FLOAT
        f_str(&mut o, 8, name);
        let mut raw = Vec::with_capacity(data.len() * 4);
        for v in data {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        f_bytes(&mut o, 9, &raw);
        self.inits.push(o);
        name.to_string()
    }
    fn init_i64(&mut self, name: &str, data: &[i64]) -> String {
        let mut o = Vec::new();
        f_int(&mut o, 1, data.len() as i64);
        f_int(&mut o, 2, 7); // INT64
        f_str(&mut o, 8, name);
        let mut raw = Vec::new();
        for v in data {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        f_bytes(&mut o, 9, &raw);
        self.inits.push(o);
        name.to_string()
    }

    /// Input "image" [batch,3,height,width]; output is the given last node.
    fn finish(self, output: &str) -> Vec<u8> {
        let vi = |name: &str| -> Vec<u8> {
            let mut shape = Vec::new();
            for d in ["batch", "3", "height", "width"] {
                let mut dim = Vec::new();
                match d.parse::<i64>() {
                    Ok(v) => f_int(&mut dim, 1, v),
                    Err(_) => f_str(&mut dim, 2, d),
                }
                f_bytes(&mut shape, 1, &dim);
            }
            let mut tt = Vec::new();
            f_int(&mut tt, 1, 1);
            f_bytes(&mut tt, 2, &shape);
            let mut tp = Vec::new();
            f_bytes(&mut tp, 1, &tt);
            let mut v = Vec::new();
            f_str(&mut v, 1, name);
            f_bytes(&mut v, 2, &tp);
            v
        };
        let mut g = Vec::new();
        for n in &self.nodes {
            f_bytes(&mut g, 1, n);
        }
        f_str(&mut g, 2, "nafnet");
        for t in &self.inits {
            f_bytes(&mut g, 5, t);
        }
        f_bytes(&mut g, 11, &vi("image"));
        // Rename the output to "out" via an Identity node.
        let mut idn = Vec::new();
        f_str(&mut idn, 1, output);
        f_str(&mut idn, 2, "out");
        f_str(&mut idn, 3, "n_out");
        f_str(&mut idn, 4, "Identity");
        f_bytes(&mut g, 1, &idn);
        f_bytes(&mut g, 12, &vi("out"));
        let mut m = Vec::new();
        f_int(&mut m, 1, 8); // ir_version
        f_str(&mut m, 2, "darkroom");
        let mut op = Vec::new();
        f_str(&mut op, 1, "");
        f_int(&mut op, 2, 17);
        f_bytes(&mut m, 8, &op);
        f_bytes(&mut m, 7, &g);
        m
    }
}

/// Builds an ONNX model from a NAFNet (Chen et al. 2022) state dict; width and block count are inferred from tensor names.
pub fn nafnet(sd: &HashMap<String, Tensor>) -> Result<Vec<u8>> {
    let t = |k: &str| sd.get(k).ok_or_else(|| anyhow!("{}", trf!("가중치 없음: {k}", "Missing weight: {k}")));
    let mut g = Graph::new();
    g.init_f32("eps", &[], &[1e-6]);
    let split2 = |g: &mut Graph, c: usize| -> String {
        g.init_i64(&format!("split{c}"), &[c as i64 / 2, c as i64 / 2])
    };
    let mut split_done: HashMap<usize, String> = HashMap::new();
    let conv = |g: &mut Graph, x: &str, p: &str, pad: i64, stride: i64, group: i64, bias: bool| -> Result<String> {
        let w = t(&format!("{p}.weight"))?;
        let wn = g.init_f32(&format!("{p}.weight"), &w.shape, &w.data);
        let k = w.shape[2] as i64;
        let mut ins = vec![x.to_string(), wn];
        if bias {
            let b = t(&format!("{p}.bias"))?;
            ins.push(g.init_f32(&format!("{p}.bias"), &b.shape, &b.data));
        }
        let refs: Vec<&str> = ins.iter().map(|s| s.as_str()).collect();
        Ok(g.node(
            "Conv",
            &refs,
            &[
                ("kernel_shape", Attr::Ints(vec![k, k])),
                ("pads", Attr::Ints(vec![pad, pad, pad, pad])),
                ("strides", Attr::Ints(vec![stride, stride])),
                ("group", Attr::Int(group)),
            ],
        ))
    };
    // Reshape a vector to (1,C,1,1) for broadcast multiply/add.
    let vec4 = |g: &mut Graph, k: &str| -> Result<String> {
        let v = t(k)?;
        let c = v.data.len();
        Ok(g.init_f32(k, &[1, c, 1, 1], &v.data))
    };
    let ln = |g: &mut Graph, x: &str, p: &str| -> Result<String> {
        let mu = g.node("ReduceMean", &[x], &[("axes", Attr::Ints(vec![1])), ("keepdims", Attr::Int(1))]);
        let d = g.node("Sub", &[x, &mu], &[]);
        let sq = g.node("Mul", &[&d, &d], &[]);
        let var = g.node("ReduceMean", &[&sq], &[("axes", Attr::Ints(vec![1])), ("keepdims", Attr::Int(1))]);
        let ve = g.node("Add", &[&var, "eps"], &[]);
        let sd_ = g.node("Sqrt", &[&ve], &[]);
        let y = g.node("Div", &[&d, &sd_], &[]);
        let w = vec4(g, &format!("{p}.weight"))?;
        let b = vec4(g, &format!("{p}.bias"))?;
        let yw = g.node("Mul", &[&y, &w], &[]);
        Ok(g.node("Add", &[&yw, &b], &[]))
    };
    let mut block = |g: &mut Graph, inp: &str, p: &str| -> Result<String> {
        let c = t(&format!("{p}.conv1.weight"))?.shape[1];
        let dw = t(&format!("{p}.conv1.weight"))?.shape[0];
        let ff = t(&format!("{p}.conv4.weight"))?.shape[0];
        let x = ln(g, inp, &format!("{p}.norm1"))?;
        let x = conv(g, &x, &format!("{p}.conv1"), 0, 1, 1, true)?;
        let x = conv(g, &x, &format!("{p}.conv2"), 1, 1, dw as i64, true)?;
        let sp = split_done.entry(dw).or_insert_with(|| split2(g, dw)).clone();
        let ab = {
            g.n += 1;
            let (a, b) = (format!("t{}a", g.n), format!("t{}b", g.n));
            let mut o = Vec::new();
            f_str(&mut o, 1, &x);
            f_str(&mut o, 1, &sp);
            f_str(&mut o, 2, &a);
            f_str(&mut o, 2, &b);
            f_str(&mut o, 3, &format!("n{}", g.n));
            f_str(&mut o, 4, "Split");
            f_bytes(&mut o, 5, &attr("axis", &Attr::Int(1)));
            g.nodes.push(o);
            (a, b)
        };
        let x = g.node("Mul", &[&ab.0, &ab.1], &[]);
        let pooled = g.node("GlobalAveragePool", &[&x], &[]);
        let s = conv(g, &pooled, &format!("{p}.sca.1"), 0, 1, 1, true)?;
        let x = g.node("Mul", &[&x, &s], &[]);
        let x = conv(g, &x, &format!("{p}.conv3"), 0, 1, 1, true)?;
        let beta = vec4(g, &format!("{p}.beta"))?;
        let xb = g.node("Mul", &[&x, &beta], &[]);
        let y = g.node("Add", &[inp, &xb], &[]);
        let x = ln(g, &y, &format!("{p}.norm2"))?;
        let x = conv(g, &x, &format!("{p}.conv4"), 0, 1, 1, true)?;
        let sp2 = split_done.entry(ff).or_insert_with(|| split2(g, ff)).clone();
        let (a, b) = {
            g.n += 1;
            let (a, b) = (format!("t{}a", g.n), format!("t{}b", g.n));
            let mut o = Vec::new();
            f_str(&mut o, 1, &x);
            f_str(&mut o, 1, &sp2);
            f_str(&mut o, 2, &a);
            f_str(&mut o, 2, &b);
            f_str(&mut o, 3, &format!("n{}", g.n));
            f_str(&mut o, 4, "Split");
            f_bytes(&mut o, 5, &attr("axis", &Attr::Int(1)));
            g.nodes.push(o);
            (a, b)
        };
        let x = g.node("Mul", &[&a, &b], &[]);
        let x = conv(g, &x, &format!("{p}.conv5"), 0, 1, 1, true)?;
        let gamma = vec4(g, &format!("{p}.gamma"))?;
        let xg = g.node("Mul", &[&x, &gamma], &[]);
        let _ = c;
        Ok(g.node("Add", &[&y, &xg], &[]))
    };
    let count = |prefix: &str| -> usize { (0..64).take_while(|i| sd.contains_key(&format!("{prefix}.{i}.conv1.weight")) || sd.keys().any(|k| k.starts_with(&format!("{prefix}.{i}.")))).count() };
    let n_enc = (0..16).take_while(|i| sd.keys().any(|k| k.starts_with(&format!("encoders.{i}.")) || k.starts_with(&format!("downs.{i}.")))).count();
    if n_enc == 0 {
        return Err(anyhow!("{}", trf!("NAFNet 가중치가 아님", "Not NAFNet weights")));
    }
    let x0 = conv(&mut g, "image", "intro", 1, 1, 1, true)?;
    let mut x = x0;
    let mut skips = Vec::new();
    for i in 0..n_enc {
        for j in 0..count(&format!("encoders.{i}")) {
            x = block(&mut g, &x, &format!("encoders.{i}.{j}"))?;
        }
        skips.push(x.clone());
        x = conv(&mut g, &x, &format!("downs.{i}"), 0, 2, 1, true)?;
    }
    for j in 0..count("middle_blks") {
        x = block(&mut g, &x, &format!("middle_blks.{j}"))?;
    }
    for i in 0..n_enc {
        x = conv(&mut g, &x, &format!("ups.{i}.0"), 0, 1, 1, false)?;
        x = g.node("DepthToSpace", &[&x], &[("blocksize", Attr::Int(2)), ("mode", Attr::Str("CRD".into()))]);
        let sk = skips[n_enc - 1 - i].clone();
        x = g.node("Add", &[&x, &sk], &[]);
        for j in 0..count(&format!("decoders.{i}")) {
            x = block(&mut g, &x, &format!("decoders.{i}.{j}"))?;
        }
    }
    let x = conv(&mut g, &x, "ending", 1, 1, 1, true)?;
    let out = g.node("Add", &[&x, "image"], &[]);
    Ok(g.finish(&out))
}
