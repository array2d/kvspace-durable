// xvalue.rs — 对齐 xvalue.go
// XValue 是所有值的统一枚举（Go 的 interface + 具体类型 → 枚举变体）。
// XValueHead + TLV 编解码（head + body）。

use crate::r#const::*;

// ── XValueHead ─────────────────────────────────────────────────────────────
// XValueHead = [1B kind_len][kind][1B ref|ro][1B ndim][4B vid LE][ndim×4B dims][padding][4B raw_len]
//   ref|ro 字节：bit[1:0]=ref（0=内联 1=软链接 2=@扩展句柄），bit[2]=ro（1=只读），bit[7:3] 保留。
//   vid：vthread id（u32 LE，默认 0），后续可设计父子继承。
//   shape 段 = dims + padding，ndim≥1 时恒 X_MAX_NDIM×4（32B），使 body_offset 与 ndim 无关，
//   xv.reshape 可原地改写 dims 不搬 body；ndim=0 标量无 shape 段（padding=0）。

/// 形状段最大维数（对齐 kvspace-c 的 X_MAX_NDIM）。
pub const X_MAX_NDIM: i32 = 8;

/// shape 段字节数：标量(ndim=0)=0，数组(ndim≥1)=X_MAX_NDIM×4。
fn shape_seg(ndim: i32) -> i32 {
    if ndim == 0 { 0 } else { X_MAX_NDIM * 4 }
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct XValueHead {
    pub kind: String,
    pub is_ptr: bool, // 派生：ref==1
    pub array_len: i32, // 派生：标量=1，定长=∏dims
    pub r#ref: i32, // 0=内联 1=软链接(*) 2=扩展句柄(@)
    pub ndim: i32, // 0=标量，N=N 维数组（唯一「是否数组」标志）
    pub dims: Vec<i32>, // 各维长度
    pub body_len: i32, // body 字节数
    pub ro: bool, // 只读：1=只读，0=可写（默认）
    pub vid: u32, // vthread id（默认 0）
}

impl XValueHead {
    /// 返回 XValueHead（元数据）字节数，不含 body。
    pub fn head_len(&self) -> i32 {
        1 + self.kind.len() as i32 + 1 + 1 + 4 + shape_seg(self.dims.len() as i32) + 4
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
        if self.is_ptr {
            return XValue::Ptr(Ptr {
                kind: self.kind.clone(),
                target: String::from_utf8_lossy(body).into_owned(),
                array_len: self.array_len,
            });
        }
        match self.kind.as_str() {
            KIND_BOOL => XValue::Bool(crate::xvalue_bool::decode_bool(body, &self.dims)),
            KIND_INT8 => XValue::Int8(crate::xvalue_int::decode_int8(body, &self.dims)),
            KIND_INT16 => XValue::Int16(crate::xvalue_int::decode_int16(body, &self.dims)),
            KIND_INT32 => XValue::Int32(crate::xvalue_int::decode_int32(body, &self.dims)),
            KIND_INT64 => XValue::Int64(crate::xvalue_int::decode_int64(body, &self.dims)),
            KIND_UINT8 => XValue::Uint8(crate::xvalue_uint::decode_uint8(body, &self.dims)),
            KIND_UINT16 => XValue::Uint16(crate::xvalue_uint::decode_uint16(body, &self.dims)),
            KIND_UINT32 => XValue::Uint32(crate::xvalue_uint::decode_uint32(body, &self.dims)),
            KIND_UINT64 => XValue::Uint64(crate::xvalue_uint::decode_uint64(body, &self.dims)),
            KIND_FLOAT32 => XValue::Float32(crate::xvalue_float::decode_float32(body, &self.dims)),
            KIND_FLOAT64 => XValue::Float64(crate::xvalue_float::decode_float64(body, &self.dims)),
            KIND_CHAR_UTF8 => XValue::CharByte(crate::xvalue_byte::decode_char_byte(body, &self.dims)),
            KIND_CHAR_ASCII => XValue::CharAscii(crate::xvalue_byte::decode_char_ascii(body, &self.dims)),
            KIND_CHAR => XValue::Char32(crate::xvalue_byte::decode_char32(body, &self.dims)),
            KIND_OBJ => {
                if body.is_empty() {
                    XValue::Obj(Vec::new())
                } else {
                    XValue::Obj(crate::xvalue_index::decode_obj_index(body))
                }
            }
            KIND_MAP => XValue::Map(crate::xvalue_index::decode_index(body)),
            KIND_INDEX => XValue::Index(crate::xvalue_index::decode_index(body)),
            KIND_EXT_INDEX => XValue::ExtIndex(crate::xvalue_index::decode_ext_index(body)),
            _ => XValue::Opaque(Opaque {
                kind: self.kind.clone(),
                body: body.to_vec(),
                array_len: self.array_len,
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
    if n > 1 { vec![n as i32] } else { Vec::new() }
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
    CharByte(Arr<u8>), // char/utf8，1B×N
    CharAscii(Arr<u8>), // char/ascii，1B×N
    Char32(Arr<u32>), // char/utf32，码点，4B×N
    Obj(Vec<String>), // obj（空 = Obj{}，非空 = ObjIndex）
    Map(Vec<String>), // map（同构 map：key 恒 char 字符串，value 固定 kind；child 名=带中括号索引串）
    Index(Vec<String>), // index
    ExtIndex(ExtIndex), // extindex
    Opaque(Opaque), // 未知 kind（如 kvlang 的 rwir/rwfunc/scope），原样存取
}

impl XValue {
    pub fn kind(&self) -> &str {
        match self {
            XValue::None => "",
            XValue::Ptr(p) => p.kind.as_str(),
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
            XValue::Obj(_) => KIND_OBJ,
            XValue::Map(_) => KIND_MAP,
            XValue::Index(_) => KIND_INDEX,
            XValue::ExtIndex(_) => KIND_EXT_INDEX,
            XValue::Opaque(o) => o.kind.as_str(),
        }
    }

    pub fn is_ptr(&self) -> bool {
        matches!(self, XValue::Ptr(_))
    }

    pub fn byte_len(&self) -> i32 {
        match self {
            XValue::None => 0,
            XValue::Ptr(p) => p.target.len() as i32,
            XValue::Bool(d) => d.data.len() as i32,
            XValue::Int8(d) => d.data.len() as i32,
            XValue::Int16(d) => (d.data.len() * 2) as i32,
            XValue::Int32(d) => (d.data.len() * 4) as i32,
            XValue::Int64(d) => (d.data.len() * 8) as i32,
            XValue::Uint8(d) => d.data.len() as i32,
            XValue::Uint16(d) => (d.data.len() * 2) as i32,
            XValue::Uint32(d) => (d.data.len() * 4) as i32,
            XValue::Uint64(d) => (d.data.len() * 8) as i32,
            XValue::Float32(d) => (d.data.len() * 4) as i32,
            XValue::Float64(d) => (d.data.len() * 8) as i32,
            XValue::CharByte(d) => d.data.len() as i32,
            XValue::CharAscii(d) => d.data.len() as i32,
            XValue::Char32(d) => (d.data.len() * 4) as i32,
            XValue::Obj(d) => d.join(INDEX_VALUE_SEP).len() as i32,
            XValue::Map(d) => d.join(INDEX_VALUE_SEP).len() as i32,
            XValue::Index(d) => d.join(INDEX_VALUE_SEP).len() as i32,
            XValue::ExtIndex(e) => crate::xvalue_index::encode_ext_index_raw(&e.ext_path, &e.childs).len() as i32,
            XValue::Opaque(o) => o.body.len() as i32,
        }
    }

    pub fn array_len(&self) -> i32 {
        match self {
            XValue::None => 0,
            XValue::Ptr(p) => p.array_len,
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
            XValue::Obj(_) => 1,
            XValue::Map(_) => 1,
            XValue::Index(_) => 1,
            XValue::ExtIndex(_) => 1,
            XValue::Opaque(o) => o.array_len,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            XValue::None => Vec::new(),
            XValue::Ptr(p) => tlv_encode_ptr(&p.kind, p.target.as_bytes(), p.array_len),
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
            XValue::Obj(d) => {
                let raw = d.join(INDEX_VALUE_SEP).into_bytes();
                tlv_encode(KIND_OBJ, &raw, 1)
            }
            XValue::Map(d) => {
                let raw = d.join(INDEX_VALUE_SEP).into_bytes();
                tlv_encode(KIND_MAP, &raw, 1)
            }
            XValue::Index(d) => {
                let raw = d.join(INDEX_VALUE_SEP).into_bytes();
                tlv_encode(KIND_INDEX, &raw, 1)
            }
            XValue::ExtIndex(e) => {
                let raw = crate::xvalue_index::encode_ext_index_raw(&e.ext_path, &e.childs);
                tlv_encode(KIND_EXT_INDEX, &raw, 1)
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
            XValue::Char32(d) => d.data.iter().map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}')).collect(),
            XValue::Obj(d) => obj_value_string(d),
            XValue::Map(d) => format!("map{{{}}}", d.len()),
            XValue::Index(d) => index_value_string(d),
            XValue::ExtIndex(e) => e.value_string(),
            XValue::Opaque(o) => String::from_utf8_lossy(&o.body).into_owned(),
        }
    }

    pub fn code_string(&self) -> String {
        match self {
            XValue::None => KIND_NONE.to_string(),
            XValue::Ptr(p) => format!("→{}:{}", p.target, p.kind),
            _ => format!("{}:{}", self.kind(), self.value_string()),
        }
    }
}

impl std::fmt::Display for XValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code_string())
    }
}

// ── Ptr ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct Ptr {
    pub kind: String, // 目标类型
    pub target: String, // 目标 key 路径
    pub array_len: i32,
}

pub fn new_ptr(kind: &str, target: &str, array_len: i32) -> XValue {
    XValue::Ptr(Ptr {
        kind: kind.to_string(),
        target: target.to_string(),
        array_len,
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

fn obj_value_string(childs: &[String]) -> String {
    if childs.len() == 1 && childs[0].is_empty() {
        "{empty}".to_string()
    } else if childs.is_empty() {
        KIND_OBJ.to_string()
    } else {
        format!("{{{}}}", childs.len())
    }
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
    if b { "true".to_string() } else { "false".to_string() }
}

// ── TLV 编解码 ─────────────────────────────────────────────────────────────
// XValue = XValueHead + body。
// XValueHead = [1B kind_len][kind][1B ref|ro][1B ndim][4B vid LE][ndim×4B dims][padding][4B raw_len]
// body       = [raw]，offset = head_len()。
// ref: 0=内联 1=软链接(*) 2=扩展句柄(@)；ref=1 时 body 为目标 key 路径。None 编码为 nil。

pub fn tlv_encode(kind: &str, raw: &[u8], array_len: i32) -> Vec<u8> {
    encode_head(kind, 0, &array_to_header(kind, array_len), raw)
}

pub fn tlv_encode_ptr(kind: &str, raw: &[u8], array_len: i32) -> Vec<u8> {
    let array_len = if array_len <= 0 { 1 } else { array_len };
    encode_head(kind, 1, &array_to_header(kind, array_len), raw)
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

pub fn encode_head_perm(kind: &str, r#ref: i32, dims: &[i32], raw: &[u8], ro: bool, vid: u32) -> Vec<u8> {
    let ndim = dims.len() as i32;
    let seg = shape_seg(ndim) as usize;
    // padding 段（seg - 4*ndim 字节）由 vec! 初始化为 0。
    let mut buf = vec![0u8; 1 + kind.len() + 1 + 1 + 4 + seg + 4 + raw.len()];
    buf[0] = kind.len() as u8;
    buf[1..1 + kind.len()].copy_from_slice(kind.as_bytes());
    let o = 1 + kind.len();
    buf[o] = (r#ref as u8 & 0x03) | if ro { 0x04 } else { 0 };
    buf[o + 1] = ndim as u8;
    buf[o + 2..o + 6].copy_from_slice(&vid.to_le_bytes());
    for (i, d) in dims.iter().enumerate() {
        let off = o + 6 + 4 * i;
        buf[off..off + 4].copy_from_slice(&(*d as u32).to_le_bytes());
    }
    let raw_len_off = o + 6 + seg;
    buf[raw_len_off..raw_len_off + 4].copy_from_slice(&(raw.len() as u32).to_le_bytes());
    buf[raw_len_off + 4..].copy_from_slice(raw);
    buf
}

pub fn decode_xvalue_head(data: &[u8]) -> XValueHead {
    if data.is_empty() {
        return XValueHead::default();
    }
    let kind_len = data[0] as usize;
    let o = 1 + kind_len;
    if data.len() < o + 10 {
        return XValueHead::default();
    }
    let kind = String::from_utf8_lossy(&data[1..o]).into_owned();
    let mode = data[o];
    let r#ref = (mode & 0x03) as i32;
    let ro = (mode & 0x04) != 0;
    let ndim = data[o + 1] as i32;
    if ndim > X_MAX_NDIM {
        return XValueHead::default();
    }
    let vid = u32::from_le_bytes(data[o + 2..o + 6].try_into().unwrap());
    let seg = shape_seg(ndim) as usize;
    if data.len() < o + 6 + seg + 4 {
        return XValueHead::default();
    }
    let mut dims = Vec::with_capacity(ndim as usize);
    for i in 0..ndim as usize {
        let off = o + 6 + 4 * i;
        dims.push(i32::from_le_bytes(data[off..off + 4].try_into().unwrap()));
    }
    let raw_len_off = o + 6 + seg;
    let raw_len = u32::from_le_bytes(data[raw_len_off..raw_len_off + 4].try_into().unwrap()) as i32;
    let start = raw_len_off + 4;
    if data.len() < start + raw_len as usize {
        return XValueHead::default();
    }
    let array_len = header_array_len(ndim, &dims);
    XValueHead {
        kind,
        is_ptr: r#ref == 1,
        array_len,
        r#ref,
        ndim,
        dims,
        body_len: raw_len,
        ro,
        vid,
    }
}

/// 解析完整 XValue（head + body）为 XValue。
pub fn decode_xvalue(data: &[u8]) -> XValue {
    let h = decode_xvalue_head(data);
    if h.kind.is_empty() {
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

/// headerArrayLen 从 ndim/dims 推导 arraylength：ndim=0 标量(1)，否则 ∏dims。
fn header_array_len(ndim: i32, dims: &[i32]) -> i32 {
    if ndim <= 0 {
        1
    } else {
        dims.iter().product()
    }
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
