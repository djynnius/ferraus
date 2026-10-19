//! Cutting an ONNX model at named tensors, at the protobuf level (no ONNX library).
//!
//! [`cut`] turns chosen intermediate tensors into graph inputs (their producers are dropped),
//! makes other intermediate tensors the graph outputs, and removes every node, initializer,
//! and value-info entry the new outputs no longer depend on. Everything else (weights, opset
//! imports, metadata) is copied byte for byte. Separation uses it to move work that a graph
//! does badly on the CPU (an STFT written as a 4096-tap convolution, an iSTFT unrolled into
//! thousands of nodes) to the host, where an FFT does it in a fraction of the time.

use ferraus_core::{Error, Result};
use std::collections::HashSet;
use std::ops::Range;

fn bad(what: &str) -> Error {
    Error::processing(format!("malformed ONNX model: {what}"))
}

/// One protobuf field: number, wire type, the payload range (for length-delimited fields),
/// and the byte range of the whole field (key included) for copying it unchanged.
#[derive(Clone, Debug)]
struct Field {
    num: u32,
    wire: u8,
    payload: Range<usize>,
    whole: Range<usize>,
}

fn varint(buf: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos).ok_or_else(|| bad("truncated varint"))?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(bad("varint too long"))
}

/// The top-level fields of one message.
fn fields(buf: &[u8], range: Range<usize>) -> Result<Vec<Field>> {
    let mut out = Vec::new();
    let mut pos = range.start;
    while pos < range.end {
        let start = pos;
        let key = varint(buf, &mut pos)?;
        let (num, wire) = ((key >> 3) as u32, (key & 7) as u8);
        let payload = match wire {
            0 => {
                let a = pos;
                varint(buf, &mut pos)?;
                a..pos
            }
            1 => {
                pos += 8;
                pos - 8..pos
            }
            2 => {
                let len = varint(buf, &mut pos)? as usize;
                pos += len;
                pos - len..pos
            }
            5 => {
                pos += 4;
                pos - 4..pos
            }
            _ => return Err(bad("unsupported wire type")),
        };
        if pos > range.end {
            return Err(bad("field past the end of its message"));
        }
        out.push(Field { num, wire, payload, whole: start..pos });
    }
    Ok(out)
}

fn string(buf: &[u8], f: &Field) -> Result<String> {
    String::from_utf8(buf[f.payload.clone()].to_vec()).map_err(|_| bad("non-UTF-8 name"))
}

#[derive(Default)]
struct Enc(Vec<u8>);

impl Enc {
    fn varint(&mut self, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.0.push(b);
                break;
            }
            self.0.push(b | 0x80);
        }
    }
    fn key(&mut self, num: u32, wire: u8) {
        self.varint(((num as u64) << 3) | wire as u64);
    }
    fn bytes(&mut self, num: u32, b: &[u8]) {
        self.key(num, 2);
        self.varint(b.len() as u64);
        self.0.extend_from_slice(b);
    }
    fn int(&mut self, num: u32, v: i64) {
        self.key(num, 0);
        self.varint(v as u64);
    }
}

/// A node of the main graph: its tensors and whether it carries subgraphs (`If`, `Loop`, ...),
/// whose implicit outer-scope inputs this module does not track.
#[derive(Clone, Debug)]
pub struct NodeInfo {
    pub name: String,
    pub op: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    subgraph: bool,
    whole: Range<usize>,
}

/// The parsed top level of a model: enough to inspect and cut its main graph.
pub struct Model<'a> {
    buf: &'a [u8],
    model_fields: Vec<Field>,
    graph: Range<usize>,
    graph_fields: Vec<Field>,
    pub nodes: Vec<NodeInfo>,
}

impl<'a> Model<'a> {
    pub fn parse(buf: &'a [u8]) -> Result<Model<'a>> {
        let model_fields = fields(buf, 0..buf.len())?;
        let g = model_fields.iter().find(|f| f.num == 7 && f.wire == 2).ok_or_else(|| bad("no graph"))?;
        let graph = g.payload.clone();
        let graph_fields = fields(buf, graph.clone())?;
        let mut nodes = Vec::new();
        for f in graph_fields.iter().filter(|f| f.num == 1 && f.wire == 2) {
            let mut n = NodeInfo {
                name: String::new(),
                op: String::new(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                subgraph: false,
                whole: f.whole.clone(),
            };
            for nf in fields(buf, f.payload.clone())? {
                match (nf.num, nf.wire) {
                    (1, 2) => n.inputs.push(string(buf, &nf)?),
                    (2, 2) => n.outputs.push(string(buf, &nf)?),
                    (3, 2) => n.name = string(buf, &nf)?,
                    (4, 2) => n.op = string(buf, &nf)?,
                    // AttributeProto: g = 6, graphs = 11
                    (5, 2) => n.subgraph |= fields(buf, nf.payload.clone())?.iter().any(|a| a.num == 6 || a.num == 11),
                    _ => {}
                }
            }
            nodes.push(n);
        }
        Ok(Model { buf, model_fields, graph, graph_fields, nodes })
    }

    /// Whether some node produces `tensor`.
    pub fn produces(&self, tensor: &str) -> bool {
        self.nodes.iter().any(|n| n.outputs.iter().any(|o| o == tensor))
    }

    /// Name of a graph-level entry: ValueInfoProto (field 1) or TensorProto (field 8).
    fn entry_name(&self, f: &Field, name_field: u32) -> Result<Option<String>> {
        for x in fields(self.buf, f.payload.clone())? {
            if x.num == name_field && x.wire == 2 {
                return Ok(Some(string(self.buf, &x)?));
            }
        }
        Ok(None)
    }

    /// The model with `inputs` (existing tensors) as new graph inputs and `outputs` (existing
    /// tensors) as its only outputs, all `float` with the given dimensions (`-1` = unknown).
    /// Nodes, initializers, and graph inputs the outputs do not need are removed.
    pub fn cut(&self, inputs: &[(&str, Vec<i64>)], outputs: &[(&str, Vec<i64>)]) -> Result<Vec<u8>> {
        let cut_in: HashSet<&str> = inputs.iter().map(|(n, _)| *n).collect();
        for (t, _) in inputs.iter().chain(outputs) {
            if !self.produces(t) {
                return Err(bad(&format!("no node produces '{t}'")));
            }
        }
        // walk back from the outputs (nodes are topologically sorted)
        let mut needed: HashSet<String> = outputs.iter().map(|(n, _)| n.to_string()).collect();
        let mut keep = vec![false; self.nodes.len()];
        for (i, n) in self.nodes.iter().enumerate().rev() {
            if !n.outputs.iter().any(|o| needed.contains(o) && !cut_in.contains(o.as_str())) {
                continue;
            }
            if n.subgraph {
                return Err(bad(&format!("node '{}' has a subgraph; cutting it is not supported", n.name)));
            }
            if let Some(o) = n.outputs.iter().find(|o| cut_in.contains(o.as_str())) {
                return Err(bad(&format!("'{o}' cannot become an input: its producer is still needed")));
            }
            keep[i] = true;
            needed.extend(n.inputs.iter().filter(|s| !s.is_empty()).cloned());
        }
        let produced: HashSet<&str> = self
            .nodes
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .flat_map(|(n, _)| n.outputs.iter().map(|s| s.as_str()))
            .collect();
        let mut g = Enc::default();
        let mut node_iter = self.nodes.iter().zip(&keep);
        for f in &self.graph_fields {
            let whole = &self.buf[f.whole.clone()];
            match (f.num, f.wire) {
                (1, 2) => {
                    let (n, k) = node_iter.next().ok_or_else(|| bad("node count changed"))?;
                    debug_assert_eq!(n.whole, f.whole);
                    if *k {
                        g.0.extend_from_slice(whole);
                    }
                }
                // initializer, sparse initializer: keep the needed ones
                (5, 2) => {
                    if self.entry_name(f, 8)?.is_some_and(|n| needed.contains(&n)) {
                        g.0.extend_from_slice(whole);
                    }
                }
                (15, 2) => g.0.extend_from_slice(whole),
                // graph inputs that are still read
                (11, 2) => {
                    if self.entry_name(f, 1)?.is_some_and(|n| needed.contains(&n) && !cut_in.contains(n.as_str())) {
                        g.0.extend_from_slice(whole);
                    }
                }
                // the old outputs are replaced
                (12, 2) => {}
                // shape annotations of tensors that still exist
                (13, 2) => {
                    if self.entry_name(f, 1)?.is_some_and(|n| produced.contains(n.as_str())) {
                        g.0.extend_from_slice(whole);
                    }
                }
                _ => g.0.extend_from_slice(whole),
            }
        }
        for (name, dims) in inputs {
            g.bytes(11, &value_info(name, dims));
        }
        for (name, dims) in outputs {
            g.bytes(12, &value_info(name, dims));
        }
        let mut m = Enc::default();
        for f in &self.model_fields {
            if f.payload == self.graph && f.num == 7 {
                m.bytes(7, &g.0);
            } else {
                m.0.extend_from_slice(&self.buf[f.whole.clone()]);
            }
        }
        Ok(m.0)
    }
}

/// `ValueInfoProto` of a float tensor.
fn value_info(name: &str, dims: &[i64]) -> Vec<u8> {
    let mut shape = Enc::default();
    for (i, d) in dims.iter().enumerate() {
        let mut dp = Enc::default();
        if *d >= 0 {
            dp.int(1, *d);
        } else {
            dp.bytes(2, format!("{name}_d{i}").as_bytes());
        }
        shape.bytes(1, &dp.0);
    }
    let mut tensor = Enc::default();
    tensor.int(1, 1); // FLOAT
    tensor.bytes(2, &shape.0);
    let mut ty = Enc::default();
    ty.bytes(1, &tensor.0);
    let mut vi = Enc::default();
    vi.bytes(1, name.as_bytes());
    vi.bytes(2, &ty.0);
    vi.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onnx_proto::{node, Dim, GraphBuilder};
    use crate::session::{build_on, SessionOptions};
    use crate::Provider;
    use ort::value::Tensor;

    /// x → a = x·2 → b = a + 1 → c = b·3 → y = c + x; plus a dead branch d = x·5.
    fn chain() -> Vec<u8> {
        GraphBuilder::new()
            .input("x", vec![Dim::Fixed(1), Dim::Fixed(4)])
            .output("y", vec![Dim::Fixed(1), Dim::Fixed(4)])
            .init_f32("two", vec![1], vec![2.0])
            .init_f32("one", vec![1], vec![1.0])
            .init_f32("three", vec![1], vec![3.0])
            .init_f32("five", vec![1], vec![5.0])
            .node(node("Mul", &["x", "two"], &["a"]))
            .node(node("Add", &["a", "one"], &["b"]))
            .node(node("Mul", &["b", "three"], &["c"]))
            .node(node("Mul", &["x", "five"], &["d"]))
            .node(node("Add", &["c", "x"], &["y"]))
            .build()
    }

    fn run(model: &[u8], inputs: &[(&str, Vec<f32>)]) -> Vec<(String, Vec<f32>)> {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let k = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("ferraus-onnx-edit-{}-{k}.onnx", std::process::id()));
        std::fs::write(&path, model).unwrap();
        let mut s = build_on(&path, Provider::Cpu, &SessionOptions::default()).unwrap();
        let _ = std::fs::remove_file(&path);
        let names: Vec<String> = s.outputs().iter().map(|o| o.name().to_string()).collect();
        let vals: Vec<(String, ort::session::SessionInputValue<'static>)> = inputs
            .iter()
            .map(|(n, v)| (n.to_string(), Tensor::from_array((vec![1usize, v.len()], v.clone())).unwrap().into()))
            .collect();
        let out = s.run(vals).unwrap();
        names
            .iter()
            .map(|n| (n.clone(), out.get(n.as_str()).unwrap().try_extract_tensor::<f32>().unwrap().1.to_vec()))
            .collect()
    }

    #[test]
    fn cut_makes_inputs_and_outputs_and_drops_dead_nodes() {
        let full = chain();
        let m = Model::parse(&full).unwrap();
        assert_eq!(m.nodes.len(), 5);
        assert!(m.produces("b") && !m.produces("x"));
        // b becomes an input, c an output: only "Mul b·3" remains
        let cut = m.cut(&[("b", vec![1, 4])], &[("c", vec![1, 4])]).unwrap();
        let c = Model::parse(&cut).unwrap();
        assert_eq!(c.nodes.iter().map(|n| n.op.as_str()).collect::<Vec<_>>(), vec!["Mul"]);
        let out = run(&cut, &[("b", vec![1.0, 2.0, 3.0, 4.0])]);
        assert_eq!(out, vec![("c".to_string(), vec![3.0, 6.0, 9.0, 12.0])]);
        // two outputs, one of them the original output; x stays an input because y reads it
        let cut = m.cut(&[("a", vec![1, 4])], &[("y", vec![1, 4]), ("b", vec![1, -1])]).unwrap();
        let out = run(&cut, &[("x", vec![1.0, 1.0, 1.0, 1.0]), ("a", vec![0.0, 1.0, 2.0, 3.0])]);
        assert_eq!(out[0], ("y".to_string(), vec![4.0, 7.0, 10.0, 13.0]));
        assert_eq!(out[1], ("b".to_string(), vec![1.0, 2.0, 3.0, 4.0]));
        let c = Model::parse(&cut).unwrap();
        assert!(!c.nodes.iter().any(|n| n.outputs == ["d"]), "the dead branch is removed");
        // unchanged weights are copied, unused ones dropped
        assert!(cut.len() < full.len());
    }

    #[test]
    fn cut_rejects_impossible_requests() {
        let full = chain();
        let m = Model::parse(&full).unwrap();
        assert!(m.cut(&[("nope", vec![1])], &[("y", vec![1, 4])]).is_err());
        assert!(m.cut(&[], &[("x", vec![1, 4])]).is_err(), "graph inputs are not node outputs");
        assert!(Model::parse(&full[..full.len() / 2]).is_err());
    }
}
