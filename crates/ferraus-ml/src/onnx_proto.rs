//! A minimal ONNX protobuf writer for tiny test models, so tests never need binary fixtures
//! or Python. It covers just enough of `onnx.proto` (ModelProto, GraphProto, NodeProto,
//! AttributeProto, TensorProto, ValueInfoProto) for float graphs of standard ops.

/// Protobuf wire encoder.
#[derive(Default)]
struct Pb(Vec<u8>);

impl Pb {
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
    fn key(&mut self, field: u32, wire: u8) {
        self.varint(((field as u64) << 3) | wire as u64);
    }
    fn int(&mut self, field: u32, v: i64) {
        self.key(field, 0);
        self.varint(v as u64);
    }
    fn bytes(&mut self, field: u32, b: &[u8]) {
        self.key(field, 2);
        self.varint(b.len() as u64);
        self.0.extend_from_slice(b);
    }
    fn str(&mut self, field: u32, s: &str) {
        self.bytes(field, s.as_bytes());
    }
    fn msg(&mut self, field: u32, m: Pb) {
        self.bytes(field, &m.0);
    }
    fn float(&mut self, field: u32, v: f32) {
        self.key(field, 5);
        self.0.extend_from_slice(&v.to_le_bytes());
    }
}

const FLOAT: i64 = 1;
const INT64: i64 = 7;

/// A dimension of a graph input/output.
#[derive(Clone, Debug)]
pub enum Dim {
    Fixed(i64),
    Named(&'static str),
}

/// An attribute value.
#[derive(Clone, Debug)]
pub enum Attr {
    Int(i64),
    Float(f32),
    Ints(Vec<i64>),
    Str(String),
}

#[derive(Clone, Debug)]
pub struct Node {
    pub op: &'static str,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub attrs: Vec<(&'static str, Attr)>,
}

pub fn node(op: &'static str, inputs: &[&str], outputs: &[&str]) -> Node {
    Node {
        op,
        inputs: inputs.iter().map(|s| s.to_string()).collect(),
        outputs: outputs.iter().map(|s| s.to_string()).collect(),
        attrs: Vec::new(),
    }
}

impl Node {
    pub fn attr(mut self, name: &'static str, a: Attr) -> Node {
        self.attrs.push((name, a));
        self
    }
}

#[derive(Clone, Debug)]
enum InitData {
    F32(Vec<f32>),
    I64(Vec<i64>),
}

/// A float graph under construction.
#[derive(Default)]
pub struct GraphBuilder {
    nodes: Vec<Node>,
    inits: Vec<(String, Vec<i64>, InitData)>,
    inputs: Vec<(String, Vec<Dim>)>,
    outputs: Vec<(String, Vec<Dim>)>,
}

impl GraphBuilder {
    pub fn new() -> GraphBuilder {
        GraphBuilder::default()
    }
    pub fn input(mut self, name: &str, dims: Vec<Dim>) -> Self {
        self.inputs.push((name.into(), dims));
        self
    }
    pub fn output(mut self, name: &str, dims: Vec<Dim>) -> Self {
        self.outputs.push((name.into(), dims));
        self
    }
    pub fn node(mut self, n: Node) -> Self {
        self.nodes.push(n);
        self
    }
    pub fn init_f32(mut self, name: &str, dims: Vec<i64>, data: Vec<f32>) -> Self {
        assert_eq!(dims.iter().product::<i64>() as usize, data.len(), "initializer {name}");
        self.inits.push((name.into(), dims, InitData::F32(data)));
        self
    }
    pub fn init_i64(mut self, name: &str, dims: Vec<i64>, data: Vec<i64>) -> Self {
        self.inits.push((name.into(), dims, InitData::I64(data)));
        self
    }

    /// Serialize as an ONNX model (IR 8, default-domain opset 17).
    pub fn build(self) -> Vec<u8> {
        let mut g = Pb::default();
        for (i, n) in self.nodes.iter().enumerate() {
            let mut p = Pb::default();
            for s in &n.inputs {
                p.str(1, s);
            }
            for s in &n.outputs {
                p.str(2, s);
            }
            p.str(3, &format!("n{i}"));
            p.str(4, n.op);
            for (name, a) in &n.attrs {
                let mut ap = Pb::default();
                ap.str(1, name);
                match a {
                    Attr::Float(v) => {
                        ap.float(2, *v);
                        ap.int(20, 1);
                    }
                    Attr::Int(v) => {
                        ap.int(3, *v);
                        ap.int(20, 2);
                    }
                    Attr::Str(s) => {
                        ap.str(4, s);
                        ap.int(20, 3);
                    }
                    Attr::Ints(vs) => {
                        for v in vs {
                            ap.int(8, *v);
                        }
                        ap.int(20, 7);
                    }
                }
                p.msg(5, ap);
            }
            g.msg(1, p);
        }
        g.str(2, "ferraus_test_graph");
        for (name, dims, data) in &self.inits {
            let mut t = Pb::default();
            for d in dims {
                t.int(1, *d);
            }
            match data {
                InitData::F32(v) => {
                    t.int(2, FLOAT);
                    t.str(8, name);
                    let raw: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
                    t.bytes(9, &raw);
                }
                InitData::I64(v) => {
                    t.int(2, INT64);
                    t.str(8, name);
                    let raw: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
                    t.bytes(9, &raw);
                }
            }
            g.msg(5, t);
        }
        for (field, list) in [(11u32, &self.inputs), (12u32, &self.outputs)] {
            for (name, dims) in list {
                g.msg(field, value_info(name, dims));
            }
        }
        let mut m = Pb::default();
        m.int(1, 8); // ir_version
        m.str(2, "ferraus-test");
        let mut opset = Pb::default();
        opset.str(1, "");
        opset.int(2, 17);
        m.msg(8, opset);
        m.msg(7, g);
        m.0
    }
}

fn value_info(name: &str, dims: &[Dim]) -> Pb {
    let mut shape = Pb::default();
    for d in dims {
        let mut dp = Pb::default();
        match d {
            Dim::Fixed(v) => dp.int(1, *v),
            Dim::Named(s) => dp.str(2, s),
        }
        shape.msg(1, dp);
    }
    let mut tensor = Pb::default();
    tensor.int(1, FLOAT);
    tensor.msg(2, shape);
    let mut ty = Pb::default();
    ty.msg(1, tensor);
    let mut vi = Pb::default();
    vi.str(1, name);
    vi.msg(2, ty);
    vi
}

/// `y = x * gain`, shape `[1, C, T]`.
pub fn gain_model(channels: i64, gain: f32) -> Vec<u8> {
    GraphBuilder::new()
        .input("x", vec![Dim::Fixed(1), Dim::Fixed(channels), Dim::Named("T")])
        .output("y", vec![Dim::Fixed(1), Dim::Fixed(channels), Dim::Named("T")])
        .init_f32("g", vec![1], vec![gain])
        .node(node("Mul", &["x", "g"], &["y"]))
        .build()
}

/// Depthwise 1-D convolution: each of `channels` channels filtered by `kernel` ('same'
/// zero padding). Input/output `[1, C, T]`.
pub fn conv_model(channels: i64, kernel: &[f32]) -> Vec<u8> {
    let k = kernel.len() as i64;
    assert!(k % 2 == 1, "odd kernel");
    let w: Vec<f32> = (0..channels).flat_map(|_| kernel.iter().copied()).collect();
    GraphBuilder::new()
        .input("x", vec![Dim::Fixed(1), Dim::Fixed(channels), Dim::Named("T")])
        .output("y", vec![Dim::Fixed(1), Dim::Fixed(channels), Dim::Named("T")])
        .init_f32("w", vec![channels, 1, k], w)
        .node(
            node("Conv", &["x", "w"], &["y"])
                .attr("group", Attr::Int(channels))
                .attr("kernel_shape", Attr::Ints(vec![k]))
                .attr("pads", Attr::Ints(vec![k / 2, k / 2])),
        )
        .build()
}

/// A toy stereo separator with the Demucs I/O convention: `mix [1, 2, T]` →
/// `stems [1, S, 2, T]`, where stem `s` is the mix filtered by `kernels[s]` (odd length,
/// 'same' padding). Choose kernels that sum to a unit impulse for a perfect separator.
pub fn separator_model(kernels: &[Vec<f32>]) -> Vec<u8> {
    let s = kernels.len() as i64;
    let k = kernels[0].len() as i64;
    assert!(k % 2 == 1 && kernels.iter().all(|x| x.len() as i64 == k));
    // Conv with group=2: output channel g*S + s = mix[g] * kernel_s
    let mut w = Vec::new();
    for _g in 0..2 {
        for ker in kernels {
            w.extend_from_slice(ker);
        }
    }
    GraphBuilder::new()
        .input("mix", vec![Dim::Fixed(1), Dim::Fixed(2), Dim::Named("T")])
        .output(
            "stems",
            vec![Dim::Fixed(1), Dim::Fixed(s), Dim::Fixed(2), Dim::Named("T")],
        )
        .init_f32("w", vec![2 * s, 1, k], w)
        .init_i64("shape", vec![4], vec![1, 2, s, -1])
        .node(
            node("Conv", &["mix", "w"], &["y"])
                .attr("group", Attr::Int(2))
                .attr("kernel_shape", Attr::Ints(vec![k]))
                .attr("pads", Attr::Ints(vec![k / 2, k / 2])),
        )
        .node(node("Reshape", &["y", "shape"], &["r"]))
        .node(node("Transpose", &["r"], &["stems"]).attr("perm", Attr::Ints(vec![0, 2, 1, 3])))
        .build()
}
