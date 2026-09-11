// xvalue.rs — 对齐 xvalue.go
// XValue 是所有值的统一枚举（Go 的 interface + 具体类型 → 枚举变体）。
// XValueHead + TLV 编解码（head + body）。

use crate::r#const::*;

// ── XValueHead（三正交轴 ref × storetype × langtype，对齐 kvspace/frontend.c 黄金基准）────────────
// head = [headlen u16 LE][ref u8][storetype u8][ro u8][vid u32 LE][body_len u32 LE]
//        [storetype 物理字段][langtype kindexpr 串（占至 headlen）]
// body = [body_len B raw]
//   ref       0=inline（body=值本体）/1=ptr（body=目标 key）/2=@ext（body=扩展定位符）
//   storetype 物理布局（codec 唯一分派）：NONE/ATOM/ARRAYND/index/extindex。
//             物理字段：ARRAYND / index / extindex 为 ndim u8 + dims[ndim] u32 LE
//             （index/extindex 的 dims=[len,cap,M]）；NONE / ATOM 无物理字段。
//   langtype  完整 kindexpr 串（含 [dims]、无前缀），恒为 head 最后一段（长度 = headlen − 当前偏移）。

pub const HEAD_PREFIX: usize = 13; // headlen(2)+ref(1)+storetype(1)+ro(1)+vid(4)+body_len(4)

pub const REF_INLINE: u8 = 0;
pub const REF_PTR: u8 = 1;
pub const REF_EXT: u8 = 2;

pub const STORETYPE_NONE: u8 = 0;
pub const STORETYPE_ATOM: u8 = 1;
pub const STORETYPE_ARRAYND: u8 = 2;
pub const STORETYPE_INDEX: u8 = 3;
pub const STORETYPE_EXTINDEX: u8 = 4;

/// ARRAYND / index / extindex 携带 ndim+dims 物理字段。
pub fn store_has_dims(st: u8) -> bool {
    st == STORETYPE_ARRAYND || st == STORETYPE_INDEX || st == STORETYPE_EXTINDEX
}

fn is_index_kind(kind: &str) -> bool {
    kind == KIND_INDEX || kind == KIND_EXT_INDEX || kind == "rwfunc" || kind == "def rwir"
}

/// 由 base 种类名（+ndim）推 storetype。
fn storetype_of(kind: &str, ndim: i32) -> u8 {
    if kind.is_empty() {
        return STORETYPE_NONE;
    }
    if kind == KIND_EXT_INDEX {
        return STORETYPE_EXTINDEX;
    }
    if is_index_kind(kind) || is_map_langtype(kind) {
        return STORETYPE_INDEX;
    }
    if ndim > 0 {
        return STORETYPE_ARRAYND;
    }
    STORETYPE_ATOM
}

/// map langtype：`{memitemkeylangtype}·{memitemvaluelangtype}`。值容器的物理布局恒 index
/// （成员名索引落兄弟槽 `{key}·`，见 [[map容器]]），与标量/张量截然不同。
pub fn is_map_langtype(kind: &str) -> bool {
    kind.contains(OBJ_SEP)
}

/// 指针 head 的 storetype = 目标语义 storetype（据目标完整 kindexpr 推；指针自身物理字段恒空）。
fn storetype_from_kindexpr(kx: &str) -> u8 {
    if kx.is_empty() {
        return STORETYPE_NONE;
    }
    let (has_dims, base) = if kx.starts_with('[') {
        match kx.find(']') {
            Some(e) => (true, &kx[e + 1..]),
            None => (false, kx),
        }
    } else {
        (false, kx)
    };
    if base == KIND_EXT_INDEX {
        return STORETYPE_EXTINDEX;
    }
    if is_index_kind(base) || base.starts_with('/') || base.contains(OBJ_SEP) {
        return STORETYPE_INDEX;
    }
    if has_dims {
        return STORETYPE_ARRAYND;
    }
    STORETYPE_ATOM
}

/// langtype 串（ARRAYND 含 [dims]，其余为裸种类名/路径）。
fn build_langtype(kind: &str, storetype: u8, dims: &[i32]) -> String {
    let mut s = String::new();
    if storetype == STORETYPE_ARRAYND && !dims.is_empty() {
        s.push('[');
        for (i, d) in dims.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&d.to_string());
        }
        s.push(']');
    }
    s.push_str(kind);
    s
}

/// langtype 解析 → (dims, kind)：`[dims]` 段表形状，其余为基 kind。
///
/// **map langtype 无形状段**：`{memitemkeylangtype}·{memitemvaluelangtype}`（见 [[map容器]]）里
/// `·` 之前的方括号是**键类型**，不是维度——`[int64]·[]char/utf32` 的键是 1 元坐标 `[int64]`，
/// `[float64,float64]·int32` 的键是标量元组，与 `[2]float64`（数组形状）截然不同。故含 `·` 者整串
/// 即基 kind，绝不剥前缀。非 map 串仍只把**纯数字/空/?**的方括号当形状。
pub(crate) fn parse_kindexpr(s: &str) -> (Vec<i32>, String) {
    if s.contains(OBJ_SEP) {
        return (Vec::new(), s.to_string());
    }
    if s.starts_with('[') {
        match s.find(']') {
            Some(end) => {
                let inner = &s[1..end];
                if inner.split(',').all(|d| {
                    d.trim().is_empty() || d.trim() == "?" || d.trim().parse::<i32>().is_ok()
                }) {
                    return (
                        inner
                            .split(',')
                            .filter(|d| !d.is_empty())
                            .map(|d| d.parse().unwrap_or(0))
                            .collect(),
                        s[end + 1..].to_string(),
                    );
                }
                (Vec::new(), s.to_string())
            }
            None => (Vec::new(), s.to_string()),
        }
    } else {
        (Vec::new(), s.to_string())
    }
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct XValueHead {
    pub headlen: u16,
    pub r#ref: u8,           // 存储位置：REF_*
    pub storetype: u8,       // 物理布局：STORETYPE_*
    pub langtype: String,    // 完整 kindexpr（含 [dims]、无前缀）
    pub phys_dims: Vec<i32>, // 物理字段 dims：ARRAYND=形状、index/extindex=[len,cap,M]
    pub ro: bool,
    pub vid: u32,
    pub body_len: i32,
}

impl XValueHead {
    pub fn r#ref(&self) -> i32 {
        self.r#ref as i32
    }
    pub fn is_ptr(&self) -> bool {
        self.r#ref == REF_PTR
    }
    pub fn kind(&self) -> String {
        parse_kindexpr(&self.langtype).1
    }
    pub fn dims(&self) -> Vec<i32> {
        self.phys_dims.clone()
    }
    pub fn ndim(&self) -> i32 {
        self.phys_dims.len() as i32
    }
    pub fn array_len(&self) -> i32 {
        if self.phys_dims.is_empty() {
            1
        } else {
            self.phys_dims.iter().product()
        }
    }

    /// 返回 XValueHead（元数据）字节数，不含 body。
    pub fn head_len(&self) -> i32 {
        self.headlen as i32
    }

    /// 从完整 XValue 字节 data 截取 body。
    pub fn body<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        let off = self.head_len() as usize;
        if off + self.body_len as usize > data.len() {
            return &[];
        }
        &data[off..off + self.body_len as usize]
    }

    /// 用 body 字节解码为 XValue。
    pub fn decode(&self, body: &[u8]) -> XValue {
        if self.is_ptr() {
            return XValue::Ptr(Ptr {
                target_kindexpr: self.langtype.clone(),
                target: String::from_utf8_lossy(body).into_owned(),
            });
        }
        let kind = self.kind();
        let dims = self.dims();
        // 值容器：langtype 即完整 map langtype（`{keylt}·{valt}lt`），storetype=index，主槽 body 空。
        if is_map_langtype(&kind) {
            return XValue::Map(MapValue {
                langtype: kind,
                dims,
            });
        }
        match kind.as_str() {
            KIND_BOOL => XValue::Bool(crate::xvalue_bool::decode_bool(body, &dims)),
            KIND_INT8 => XValue::Int8(crate::xvalue_int::decode_int8(body, &dims)),
            KIND_INT16 => XValue::Int16(crate::xvalue_int::decode_int16(body, &dims)),
            KIND_INT32 => XValue::Int32(crate::xvalue_int::decode_int32(body, &dims)),
            KIND_INT64 => XValue::Int64(crate::xvalue_int::decode_int64(body, &dims)),
            KIND_UINT8 => XValue::Uint8(crate::xvalue_uint::decode_uint8(body, &dims)),
            KIND_UINT16 => XValue::Uint16(crate::xvalue_uint::decode_uint16(body, &dims)),
            KIND_UINT32 => XValue::Uint32(crate::xvalue_uint::decode_uint32(body, &dims)),
            KIND_UINT64 => XValue::Uint64(crate::xvalue_uint::decode_uint64(body, &dims)),
            KIND_FLOAT32 => XValue::Float32(crate::xvalue_float::decode_float32(body, &dims)),
            KIND_FLOAT64 => XValue::Float64(crate::xvalue_float::decode_float64(body, &dims)),
            KIND_CHAR_UTF8 => XValue::CharByte(crate::xvalue_byte::decode_char_byte(body, &dims)),
            KIND_CHAR_ASCII => {
                XValue::CharAscii(crate::xvalue_byte::decode_char_ascii(body, &dims))
            }
            KIND_CHAR => XValue::Char32(crate::xvalue_byte::decode_char32(body, &dims)),
            KIND_MAP => XValue::Map(MapValue {
                langtype: kind.clone(),
                dims: dims.clone(),
            }),
            KIND_INDEX => XValue::Index(crate::xvalue_index::decode_index(body, &dims)),
            KIND_EXT_INDEX => XValue::ExtIndex(crate::xvalue_index::decode_ext_index(body, &dims)),
            _ => XValue::Opaque(Opaque {
                kind: kind.clone(),
                body: body.to_vec(),
                array_len: self.array_len(),
            }),
        }
    }
}

// ── Arr：定长/多维数组的 shape 载体 ─────────────────────────────────────
/// 定长/多维数组 = 连续元素 + 形状。dims 空 = 标量（ndim 0），[n] = 一维，
/// [d0,d1] = 二维。decode 时从 head 透传，encode 时原样落盘，保证往返不丢 shape。
#[derive(Clone, Debug, PartialEq)]
pub struct Arr<T> {
    pub data: Vec<T>,
    pub dims: Vec<i32>,
}

/// 从元素数推导 dims（非 char）：>1 → [n]（一维），≤1 → []（标量）。
pub fn dims_from_len(n: usize) -> Vec<i32> {
    if n > 1 {
        vec![n as i32]
    } else {
        Vec::new()
    }
}

// ── XValue 枚举 ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum XValue {
    None,
    Ptr(Ptr),
    Bool(Arr<bool>),
    Int8(Arr<i8>),
    Int16(Arr<i16>),
    Int32(Arr<i32>),
    Int64(Arr<i64>),
    Uint8(Arr<u8>),
    Uint16(Arr<u16>),
    Uint32(Arr<u32>),
    Uint64(Arr<u64>),
    Float32(Arr<f32>),
    Float64(Arr<f64>),
    CharByte(Arr<u8>),  // char/utf8，1B×N
    CharAscii(Arr<u8>), // char/ascii，1B×N
    Char32(Arr<u32>),   // char/utf32，码点，4B×N
    Map(MapValue),      // stringkeymap 值容器：langtype 是完整 map langtype，成员在 memindex（p·）
    Index(Vec<String>), // index
    ExtIndex(ExtIndex), // extindex
    Opaque(Opaque),     // 未知 kind（如 kvlang 的 rwir/rwfunc/scope），原样存取
}

impl XValue {
    pub fn kind(&self) -> &str {
        match self {
            XValue::None => "",
            XValue::Ptr(p) => p.target_kindexpr.as_str(),
            XValue::Bool(_) => KIND_BOOL,
            XValue::Int8(_) => KIND_INT8,
            XValue::Int16(_) => KIND_INT16,
            XValue::Int32(_) => KIND_INT32,
            XValue::Int64(_) => KIND_INT64,
            XValue::Uint8(_) => KIND_UINT8,
            XValue::Uint16(_) => KIND_UINT16,
            XValue::Uint32(_) => KIND_UINT32,
            XValue::Uint64(_) => KIND_UINT64,
            XValue::Float32(_) => KIND_FLOAT32,
            XValue::Float64(_) => KIND_FLOAT64,
            XValue::CharByte(_) => KIND_CHAR_UTF8,
            XValue::CharAscii(_) => KIND_CHAR_ASCII,
            XValue::Char32(_) => KIND_CHAR,
            XValue::Map(m) => m.langtype.as_str(),
            XValue::Index(_) => KIND_INDEX,
            XValue::ExtIndex(_) => KIND_EXT_INDEX,
            XValue::Opaque(o) => o.kind.as_str(),
        }
    }

    pub fn is_ptr(&self) -> bool {
        matches!(self, XValue::Ptr(_))
    }

    pub fn array_len(&self) -> i32 {
        match self {
            XValue::None => 0,
            XValue::Ptr(_) => 1,
            XValue::Bool(d) => d.data.len() as i32,
            XValue::Int8(d) => d.data.len() as i32,
            XValue::Int16(d) => d.data.len() as i32,
            XValue::Int32(d) => d.data.len() as i32,
            XValue::Int64(d) => d.data.len() as i32,
            XValue::Uint8(d) => d.data.len() as i32,
            XValue::Uint16(d) => d.data.len() as i32,
            XValue::Uint32(d) => d.data.len() as i32,
            XValue::Uint64(d) => d.data.len() as i32,
            XValue::Float32(d) => d.data.len() as i32,
            XValue::Float64(d) => d.data.len() as i32,
            XValue::CharByte(d) => d.data.len() as i32,
            XValue::CharAscii(d) => d.data.len() as i32,
            XValue::Char32(d) => d.data.len() as i32,
            XValue::Map(m) => m.dims.iter().product(),
            XValue::Index(_) => 1,
            XValue::ExtIndex(_) => 1,
            XValue::Opaque(o) => o.array_len,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            XValue::None => Vec::new(),
            XValue::Ptr(p) => tlv_encode_ptr(&p.target_kindexpr, p.target.as_bytes()),
            XValue::Bool(d) => crate::xvalue_bool::encode_bool(&d.data, &d.dims),
            XValue::Int8(d) => crate::xvalue_int::encode_int8(&d.data, &d.dims),
            XValue::Int16(d) => crate::xvalue_int::encode_int16(&d.data, &d.dims),
            XValue::Int32(d) => crate::xvalue_int::encode_int32(&d.data, &d.dims),
            XValue::Int64(d) => crate::xvalue_int::encode_int64(&d.data, &d.dims),
            XValue::Uint8(d) => crate::xvalue_uint::encode_uint8(&d.data, &d.dims),
            XValue::Uint16(d) => crate::xvalue_uint::encode_uint16(&d.data, &d.dims),
            XValue::Uint32(d) => crate::xvalue_uint::encode_uint32(&d.data, &d.dims),
            XValue::Uint64(d) => crate::xvalue_uint::encode_uint64(&d.data, &d.dims),
            XValue::Float32(d) => crate::xvalue_float::encode_float32(&d.data, &d.dims),
            XValue::Float64(d) => crate::xvalue_float::encode_float64(&d.data, &d.dims),
            XValue::CharByte(d) => crate::xvalue_byte::encode_char_byte(&d.data, &d.dims),
            XValue::CharAscii(d) => crate::xvalue_byte::encode_char_ascii(&d.data, &d.dims),
            XValue::Char32(d) => crate::xvalue_byte::encode_char32(&d.data, &d.dims),
            XValue::Map(m) => encode_head(&m.langtype, 0, &m.dims, &[]),
            XValue::Index(d) => {
                let (dims, body) = crate::xvalue_index::encode_index(d);
                encode_head(KIND_INDEX, 0, &dims, &body)
            }
            XValue::ExtIndex(e) => {
                let (dims, body) = crate::xvalue_index::encode_ext_index(&e.ext_path, &e.childs);
                encode_head(KIND_EXT_INDEX, 0, &dims, &body)
            }
            XValue::Opaque(o) => tlv_encode(&o.kind, &o.body, o.array_len),
        }
    }

    pub fn value_string(&self) -> String {
        match self {
            XValue::None => KIND_NONE.to_string(),
            XValue::Ptr(p) => format!("→{}", p.target),
            XValue::Bool(d) => bool_string(d.data[0]),
            XValue::Int8(d) => (d.data[0] as i64).to_string(),
            XValue::Int16(d) => (d.data[0] as i64).to_string(),
            XValue::Int32(d) => (d.data[0] as i64).to_string(),
            XValue::Int64(d) => d.data[0].to_string(),
            XValue::Uint8(d) => (d.data[0] as u64).to_string(),
            XValue::Uint16(d) => (d.data[0] as u64).to_string(),
            XValue::Uint32(d) => (d.data[0] as u64).to_string(),
            XValue::Uint64(d) => d.data[0].to_string(),
            XValue::Float32(d) => fmt_float(d.data[0] as f64),
            XValue::Float64(d) => fmt_float(d.data[0]),
            XValue::CharByte(d) => String::from_utf8_lossy(&d.data).into_owned(),
            XValue::CharAscii(d) => String::from_utf8_lossy(&d.data).into_owned(),
            XValue::Char32(d) => d
                .data
                .iter()
                .map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}'))
                .collect(),
            XValue::Map(m) => format!(
                "map[{}]",
                m.dims
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            XValue::Index(d) => index_value_string(d),
            XValue::ExtIndex(e) => e.value_string(),
            XValue::Opaque(o) => String::from_utf8_lossy(&o.body).into_owned(),
        }
    }

    pub fn code_string(&self) -> String {
        match self {
            XValue::None => KIND_NONE.to_string(),
            XValue::Ptr(p) => format!("→{}:{}", p.target, p.target_kindexpr),
            _ => format!("{}:{}", self.kind(), self.value_string()),
        }
    }
}

impl std::fmt::Display for XValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code_string())
    }
}

// ── Map 值容器 ─────────────────────────────────────────────────────────────
/// stringkeymap 值容器：`langtype` = 完整 map langtype（`{memitemkeylangtype}·{memitemvaluelangtype}`，
/// 见 [[map容器]]），恒非空；`dims` = 逻辑形状（无形状的空容器为 [0]），成员名索引落兄弟槽 `{key}·`。
#[derive(Clone, Debug, PartialEq)]
pub struct MapValue {
    pub langtype: String,
    pub dims: Vec<i32>,
}

// ── Ptr ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct Ptr {
    pub target_kindexpr: String, // 目标的完整 kindexpr（含其自身的 */@/[dims]）
    pub target: String,          // 目标 key 路径
}

pub fn new_ptr(target_kindexpr: &str, target: &str) -> XValue {
    XValue::Ptr(Ptr {
        target_kindexpr: target_kindexpr.to_string(),
        target: target.to_string(),
    })
}

pub fn ptr_target(v: &XValue) -> String {
    if let XValue::Ptr(p) = v {
        p.target.clone()
    } else {
        String::new()
    }
}

// ── Dict / Index / ExtIndex 结构 ──────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct ExtIndex {
    pub childs: Vec<String>,
    pub ext_path: String,
}

impl ExtIndex {
    pub fn value_string(&self) -> String {
        if !self.ext_path.is_empty() {
            format!("({}) …{}", self.childs.len(), self.ext_path)
        } else if self.childs.is_empty() {
            "(empty ext)".to_string()
        } else {
            format!("({})", self.childs.len())
        }
    }
}

/// 未知 kind（非标准 XValue）的原样字节，供上层自定义 kind（如 kvlang 的 rwir/rwfunc）存取值。
#[derive(Clone, Debug, PartialEq)]
pub struct Opaque {
    pub kind: String,
    pub body: Vec<u8>,
    pub array_len: i32,
}

fn index_value_string(childs: &[String]) -> String {
    if childs.len() == 1 && childs[0].is_empty() {
        "(empty)".to_string()
    } else {
        format!("({})", childs.len())
    }
}

// ── 工具函数 ──────────────────────────────────────────────────────────────

pub fn is_none(v: &XValue) -> bool {
    matches!(v, XValue::None)
}

pub fn is_ptr(v: &XValue) -> bool {
    matches!(v, XValue::Ptr(_))
}

/// Go 的 fmtFloat：格式化成最短十进制，无小数点时补 ".0"。
fn fmt_float(v: f64) -> String {
    let s = format!("{}", v);
    if s.contains('.') {
        s
    } else {
        format!("{}.0", s)
    }
}

fn bool_string(b: bool) -> String {
    if b {
        "true".to_string()
    } else {
        "false".to_string()
    }
}

// ── 三正交轴编解码（byte-identical 于 kvspace/frontend.c）───────────────────────────────
// head = [headlen u16 LE][ref u8][storetype u8][ro u8][vid u32 LE][body_len u32 LE]
//        [storetype 物理字段（ARRAYND/index/extindex 为 ndim u8 + dims[ndim] u32 LE）][langtype]
// body = [body_len B raw]，offset = headlen。None 编码为 nil。

pub fn tlv_encode(kind: &str, raw: &[u8], array_len: i32) -> Vec<u8> {
    encode_head(kind, 0, &array_to_header(kind, array_len), raw)
}

/// 指针编码：langtype = 目标完整 kindexpr（含其 [dims]/引用性），storetype = 目标语义 storetype，
/// 物理字段恒空，body = 目标 key 路径。
pub fn tlv_encode_ptr(target_kindexpr: &str, raw: &[u8]) -> Vec<u8> {
    encode_head(target_kindexpr, 1, &[], raw)
}

/// array_len → dims：char/* 恒一维（含空串/单字符）；其余标量(≤1)=0 维、多元素=1 维。
fn array_to_header(kind: &str, array_len: i32) -> Vec<i32> {
    if kind.starts_with("char/") {
        vec![array_len.max(0)]
    } else if array_len > 1 {
        vec![array_len]
    } else {
        Vec::new()
    }
}

pub fn encode_head(kind: &str, r#ref: i32, dims: &[i32], raw: &[u8]) -> Vec<u8> {
    encode_head_perm(kind, r#ref, dims, raw, false, 0)
}

pub fn encode_head_perm(
    kind: &str,
    r#ref: i32,
    dims: &[i32],
    raw: &[u8],
    ro: bool,
    vid: u32,
) -> Vec<u8> {
    // ptr：kind 参数即目标完整 kindexpr，storetype 从其推、物理字段恒空、langtype 原样。
    let (storetype, langtype, phys): (u8, String, Vec<i32>) = if r#ref == 1 {
        (storetype_from_kindexpr(kind), kind.to_string(), Vec::new())
    } else {
        let st = storetype_of(kind, dims.len() as i32);
        let lt = build_langtype(kind, st, dims);
        let pd = if store_has_dims(st) {
            dims.to_vec()
        } else {
            Vec::new()
        };
        (st, lt, pd)
    };
    let lt_bytes = langtype.as_bytes();
    let phys_bytes = if store_has_dims(storetype) {
        1 + 4 * phys.len()
    } else {
        0
    };
    let headlen = HEAD_PREFIX + phys_bytes + lt_bytes.len();
    let mut buf = vec![0u8; headlen + raw.len()];
    buf[0..2].copy_from_slice(&(headlen as u16).to_le_bytes());
    buf[2] = r#ref as u8;
    buf[3] = storetype;
    buf[4] = ro as u8;
    buf[5..9].copy_from_slice(&vid.to_le_bytes());
    buf[9..13].copy_from_slice(&(raw.len() as u32).to_le_bytes());
    let mut o = HEAD_PREFIX;
    if store_has_dims(storetype) {
        buf[o] = phys.len() as u8;
        o += 1;
        for d in &phys {
            buf[o..o + 4].copy_from_slice(&(*d as u32).to_le_bytes());
            o += 4;
        }
    }
    buf[o..o + lt_bytes.len()].copy_from_slice(lt_bytes);
    buf[headlen..].copy_from_slice(raw);
    buf
}

/// 只解 head、**不要求 body 到齐**：供 `kvspaceGetHead` 这类只读值前缀的调用点用
/// （它按约定只取前若干字节，body 本就不在手上）。整值调用点仍走 [`decode_xvalue_head`]，
/// 那里的「headlen + body_len ≤ data.len()」是防截断校验。
pub fn decode_xvalue_head_prefix(data: &[u8]) -> XValueHead {
    if data.len() < HEAD_PREFIX {
        return XValueHead::default();
    }
    let headlen = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;
    let r#ref = data[2];
    let storetype = data[3];
    let ro = data[4] != 0;
    let vid = u32::from_le_bytes(data[5..9].try_into().unwrap());
    let body_len = u32::from_le_bytes(data[9..13].try_into().unwrap()) as i32;
    if headlen < HEAD_PREFIX || data.len() < headlen {
        return XValueHead::default();
    }
    let mut o = HEAD_PREFIX;
    let mut phys_dims = Vec::new();
    if store_has_dims(storetype) {
        let ndim = data[o] as usize;
        o += 1;
        for _ in 0..ndim {
            if o + 4 > headlen {
                break;
            }
            phys_dims.push(i32::from_le_bytes(data[o..o + 4].try_into().unwrap()));
            o += 4;
        }
    }
    let langtype = String::from_utf8_lossy(&data[o..headlen]).into_owned();
    XValueHead {
        headlen: headlen as u16,
        r#ref,
        storetype,
        langtype,
        phys_dims,
        ro,
        vid,
        body_len,
    }
}

pub fn decode_xvalue_head(data: &[u8]) -> XValueHead {
    if data.len() < HEAD_PREFIX {
        return XValueHead::default();
    }
    let headlen = u16::from_le_bytes(data[0..2].try_into().unwrap()) as usize;
    let r#ref = data[2];
    let storetype = data[3];
    let ro = data[4] != 0;
    let vid = u32::from_le_bytes(data[5..9].try_into().unwrap());
    let body_len = u32::from_le_bytes(data[9..13].try_into().unwrap()) as i32;
    if headlen < HEAD_PREFIX || data.len() < headlen {
        return XValueHead::default();
    }
    let mut o = HEAD_PREFIX;
    let mut phys_dims = Vec::new();
    if store_has_dims(storetype) {
        let ndim = data[o] as usize;
        o += 1;
        for _ in 0..ndim {
            if o + 4 > headlen {
                break;
            }
            phys_dims.push(i32::from_le_bytes(data[o..o + 4].try_into().unwrap()));
            o += 4;
        }
    }
    let langtype = String::from_utf8_lossy(&data[o..headlen]).into_owned();
    if data.len() < headlen + body_len as usize {
        return XValueHead::default();
    }
    XValueHead {
        headlen: headlen as u16,
        r#ref,
        storetype,
        langtype,
        phys_dims,
        ro,
        vid,
        body_len,
    }
}

/// 解析完整 XValue（head + body）为 XValue。
pub fn decode_xvalue(data: &[u8]) -> XValue {
    let h = decode_xvalue_head(data);
    // 空 TLV / 空 langtype 的**非指针**值 = None（写 None 落 1 字节空 kind TLV）。
    // 指针（ref=1）例外：它的 langtype 可能为空（如实参为无值容器时 runtime 推不出类型），
    // 但空 langtype 的 Ptr 仍是 Ptr——引用一个键，不是"无值"。否则指针会被读成 None。
    if h.langtype.is_empty() && !h.is_ptr() {
        return XValue::None;
    }
    h.decode(h.body(data))
}

/// 返回 XValue 的 body 字节。
pub fn body_bytes(v: &XValue) -> Vec<u8> {
    if is_none(v) {
        return Vec::new();
    }
    let data = v.encode();
    let h = decode_xvalue_head(&data);
    h.body(&data).to_vec()
}

/// ElemSize 返回 kind 的单元素字节数；≤0 表示非定长类型（非 byte 派生）。
pub fn elem_size(kind: &str) -> i32 {
    match kind {
        KIND_INT8 | KIND_UINT8 | KIND_CHAR_UTF8 | KIND_CHAR_ASCII | KIND_BOOL => 1,
        KIND_INT16 | KIND_UINT16 => 2,
        KIND_INT32 | KIND_UINT32 | KIND_FLOAT32 | KIND_CHAR => 4,
        KIND_INT64 | KIND_UINT64 | KIND_FLOAT64 | "time" | "duration" => 8,
        _ => 0,
    }
}

/// Format 返回规范表示（对齐 Go 的 Format）。
pub fn format(v: &XValue) -> String {
    if is_none(v) {
        KIND_NONE.to_string()
    } else {
        v.code_string()
    }
}

/// Plain 返回明文表示（对齐 Go 的 Plain）。
pub fn plain(v: &XValue) -> String {
    if is_none(v) {
        KIND_NONE.to_string()
    } else {
        v.value_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xvalue_bool::new_bool;
    use crate::xvalue_byte::new_char_byte;
    use crate::xvalue_float::new_float64;
    use crate::xvalue_int::new_int64;

    fn roundtrip(v: &XValue) {
        let bytes = v.encode();
        assert_eq!(*v, decode_xvalue(&bytes), "roundtrip {:?}", v);
    }

    #[test]
    fn langtype_build_parse() {
        for (kind, dims) in [
            ("int64", vec![]),
            ("float32", vec![5]),
            ("float64", vec![2, 3]),
            ("char/utf32", vec![0]),
        ] {
            let st = storetype_of(kind, dims.len() as i32);
            let s = build_langtype(kind, st, &dims);
            let (d2, k2) = parse_kindexpr(&s);
            assert_eq!((dims, kind.to_string()), (d2, k2), "langtype {}", s);
        }
    }

    #[test]
    fn roundtrip_values() {
        roundtrip(&new_int64(&[42]));
        roundtrip(&new_float64(&[1.5]));
        roundtrip(&new_bool(&[true]));
        roundtrip(&new_char_byte(b"hello"));
        roundtrip(&new_char_byte(b""));
        roundtrip(&new_ptr("int64", "/x/y"));
        roundtrip(&XValue::Int32(Arr {
            data: vec![1, 2, 3, 4, 5, 6],
            dims: vec![2, 3],
        }));
    }

    #[test]
    fn index_matrix_roundtrip() {
        use crate::xvalue_index::{matrix_at, matrix_count};
        // 乱序输入 → encode 规范排序（坐标数值序，非字节序：[2] 在 [10] 前）。
        let v = XValue::Index(vec!["[10]".into(), "[2]".into(), "[1]".into()]);
        let bytes = v.encode();
        let h = decode_xvalue_head(&bytes);
        assert_eq!(h.kind(), KIND_INDEX);
        assert_eq!(h.dims(), vec![3, 3, 8]); // len=3, cap=3, M=align8(len("[10]")=4)=8
        let decoded = decode_xvalue(&bytes);
        assert_eq!(
            decoded,
            XValue::Index(vec!["[1]".into(), "[2]".into(), "[10]".into()])
        );
        // O(1) 原语走 head+body。
        let body = h.body(&bytes);
        assert_eq!(matrix_count(&h.dims()), 3);
        assert_eq!(matrix_at(body, 8, 0).as_deref(), Some("[1]"));
        assert_eq!(matrix_at(body, 8, 2).as_deref(), Some("[10]"));
        assert_eq!(matrix_at(body, 8, 3), None);
    }

    #[test]
    fn index_empty() {
        let bytes = XValue::Index(vec![]).encode();
        let h = decode_xvalue_head(&bytes);
        assert_eq!(h.dims(), vec![0, 0, 0]);
        assert_eq!(decode_xvalue(&bytes), XValue::Index(vec![]));
    }

    #[test]
    fn ext_index_roundtrip() {
        // ext_path 置 body 头部、childs 尾部矩阵（规范排序）。
        let v = XValue::ExtIndex(ExtIndex {
            childs: vec!["b".into(), "a".into()],
            ext_path: "/lib/main·add/".into(),
        });
        let bytes = v.encode();
        let h = decode_xvalue_head(&bytes);
        assert_eq!(h.kind(), KIND_EXT_INDEX);
        assert_eq!(h.dims(), vec![2, 2, 8]); // len=2, cap=2, M=align8(1)=8
        assert_eq!(
            decode_xvalue(&bytes),
            XValue::ExtIndex(ExtIndex {
                childs: vec!["a".into(), "b".into()],
                ext_path: "/lib/main·add/".into(),
            })
        );
    }

    #[test]
    fn head_fields() {
        let bytes = new_float64(&[1.0, 2.0, 3.0]).encode();
        let h = decode_xvalue_head(&bytes);
        assert_eq!(h.kind(), "float64");
        assert_eq!(h.dims(), vec![3]);
        assert_eq!(h.array_len(), 3);
        assert!(!h.is_ptr());
        assert_eq!(h.r#ref(), 0);
        assert_eq!(h.head_len() as usize + h.body_len as usize, bytes.len());
    }
}
