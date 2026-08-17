// xvalue.rs — 对齐 xvalue.go
// XValue 是所有值的统一枚举（Go 的 interface + 具体类型 → 枚举变体）。
// XValueHead + TLV 编解码（head + body）。

use crate::r#const::*;

// ── XValueHead ─────────────────────────────────────────────────────────────
// XValueHead = [1B kind_len][kind][1B ref][1B arr_flag][1B ndim][ndim×4B dims][4B raw_len]
#[derive(Default, Clone, Debug, PartialEq)]
pub struct XValueHead {
    pub kind: String,
    pub is_ptr: bool, // 派生：ref==1
    pub array_len: i32, // 派生：标量=1，定长=∏dims，变长=raw_len/elemSize
    pub r#ref: i32, // 0=内联 1=软链接(*) 2=扩展句柄(@)
    pub arr_flag: i32, // 0=标量 1=连续([]) 2=分离(<>)
    pub ndim: i32, // 0=变长，N=定长 N 维
    pub dims: Vec<i32>, // 各维长度
    pub body_len: i32, // body 字节数
}

impl XValueHead {
    /// 返回 XValueHead（元数据）字节数，不含 body。
    pub fn head_len(&self) -> i32 {
        1 + self.kind.len() as i32 + 1 + 1 + 1 + 4 * self.dims.len() as i32 + 4
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
            KIND_BOOL => XValue::Bool(crate::xvalue_bool::decode_bool(body)),
            KIND_INT8 => XValue::Int8(crate::xvalue_int::decode_int8(body)),
            KIND_INT16 => XValue::Int16(crate::xvalue_int::decode_int16(body)),
            KIND_INT32 => XValue::Int32(crate::xvalue_int::decode_int32(body)),
            KIND_INT64 => XValue::Int64(crate::xvalue_int::decode_int64(body)),
            KIND_UINT8 => XValue::Uint8(crate::xvalue_uint::decode_uint8(body)),
            KIND_UINT16 => XValue::Uint16(crate::xvalue_uint::decode_uint16(body)),
            KIND_UINT32 => XValue::Uint32(crate::xvalue_uint::decode_uint32(body)),
            KIND_UINT64 => XValue::Uint64(crate::xvalue_uint::decode_uint64(body)),
            KIND_FLOAT32 => XValue::Float32(crate::xvalue_float::decode_float32(body)),
            KIND_FLOAT64 => XValue::Float64(crate::xvalue_float::decode_float64(body)),
            KIND_CHAR_UTF8 => XValue::CharByte(crate::xvalue_byte::decode_char_byte(body)),
            KIND_CHAR_ASCII => XValue::CharAscii(crate::xvalue_byte::decode_char_ascii(body)),
            KIND_CHAR => XValue::Char32(crate::xvalue_byte::decode_char32(body)),
            KIND_DICT => {
                if body.is_empty() {
                    XValue::Dict(Vec::new())
                } else {
                    XValue::Dict(crate::xvalue_index::decode_dict_index(body))
                }
            }
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

// ── XValue 枚举 ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum XValue {
    None,
    Ptr(Ptr),
    Bool(Vec<bool>),
    Int8(Vec<i8>),
    Int16(Vec<i16>),
    Int32(Vec<i32>),
    Int64(Vec<i64>),
    Uint8(Vec<u8>),
    Uint16(Vec<u16>),
    Uint32(Vec<u32>),
    Uint64(Vec<u64>),
    Float32(Vec<f32>),
    Float64(Vec<f64>),
    CharByte(Vec<u8>), // char/utf8，1B×N
    CharAscii(Vec<u8>), // char/ascii，1B×N
    Char32(Vec<u32>), // char/utf32，码点，4B×N
    Dict(Vec<String>), // dict（空 = Dict{}，非空 = DictIndex）
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
            XValue::Dict(_) => KIND_DICT,
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
            XValue::Bool(d) => d.len() as i32,
            XValue::Int8(d) => d.len() as i32,
            XValue::Int16(d) => (d.len() * 2) as i32,
            XValue::Int32(d) => (d.len() * 4) as i32,
            XValue::Int64(d) => (d.len() * 8) as i32,
            XValue::Uint8(d) => d.len() as i32,
            XValue::Uint16(d) => (d.len() * 2) as i32,
            XValue::Uint32(d) => (d.len() * 4) as i32,
            XValue::Uint64(d) => (d.len() * 8) as i32,
            XValue::Float32(d) => (d.len() * 4) as i32,
            XValue::Float64(d) => (d.len() * 8) as i32,
            XValue::CharByte(d) => d.len() as i32,
            XValue::CharAscii(d) => d.len() as i32,
            XValue::Char32(d) => (d.len() * 4) as i32,
            XValue::Dict(d) => d.join(INDEX_VALUE_SEP).len() as i32,
            XValue::Index(d) => d.join(INDEX_VALUE_SEP).len() as i32,
            XValue::ExtIndex(e) => crate::xvalue_index::encode_ext_index_raw(&e.ext_path, &e.childs).len() as i32,
            XValue::Opaque(o) => o.body.len() as i32,
        }
    }

    pub fn array_len(&self) -> i32 {
        match self {
            XValue::None => 0,
            XValue::Ptr(p) => p.array_len,
            XValue::Bool(d) => d.len() as i32,
            XValue::Int8(d) => d.len() as i32,
            XValue::Int16(d) => d.len() as i32,
            XValue::Int32(d) => d.len() as i32,
            XValue::Int64(d) => d.len() as i32,
            XValue::Uint8(d) => d.len() as i32,
            XValue::Uint16(d) => d.len() as i32,
            XValue::Uint32(d) => d.len() as i32,
            XValue::Uint64(d) => d.len() as i32,
            XValue::Float32(d) => d.len() as i32,
            XValue::Float64(d) => d.len() as i32,
            XValue::CharByte(d) => d.len() as i32,
            XValue::CharAscii(d) => d.len() as i32,
            XValue::Char32(d) => d.len() as i32,
            XValue::Dict(_) => 1,
            XValue::Index(_) => 1,
            XValue::ExtIndex(_) => 1,
            XValue::Opaque(o) => o.array_len,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            XValue::None => Vec::new(),
            XValue::Ptr(p) => tlv_encode_ptr(&p.kind, p.target.as_bytes(), p.array_len),
            XValue::Bool(d) => crate::xvalue_bool::encode_bool(d),
            XValue::Int8(d) => crate::xvalue_int::encode_int8(d),
            XValue::Int16(d) => crate::xvalue_int::encode_int16(d),
            XValue::Int32(d) => crate::xvalue_int::encode_int32(d),
            XValue::Int64(d) => crate::xvalue_int::encode_int64(d),
            XValue::Uint8(d) => crate::xvalue_uint::encode_uint8(d),
            XValue::Uint16(d) => crate::xvalue_uint::encode_uint16(d),
            XValue::Uint32(d) => crate::xvalue_uint::encode_uint32(d),
            XValue::Uint64(d) => crate::xvalue_uint::encode_uint64(d),
            XValue::Float32(d) => crate::xvalue_float::encode_float32(d),
            XValue::Float64(d) => crate::xvalue_float::encode_float64(d),
            XValue::CharByte(d) => crate::xvalue_byte::encode_char_byte(d),
            XValue::CharAscii(d) => crate::xvalue_byte::encode_char_ascii(d),
            XValue::Char32(d) => crate::xvalue_byte::encode_char32(d),
            XValue::Dict(d) => {
                let raw = d.join(INDEX_VALUE_SEP).into_bytes();
                tlv_encode(KIND_DICT, &raw, 1)
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
            XValue::Bool(d) => bool_string(d[0]),
            XValue::Int8(d) => (d[0] as i64).to_string(),
            XValue::Int16(d) => (d[0] as i64).to_string(),
            XValue::Int32(d) => (d[0] as i64).to_string(),
            XValue::Int64(d) => d[0].to_string(),
            XValue::Uint8(d) => (d[0] as u64).to_string(),
            XValue::Uint16(d) => (d[0] as u64).to_string(),
            XValue::Uint32(d) => (d[0] as u64).to_string(),
            XValue::Uint64(d) => d[0].to_string(),
            XValue::Float32(d) => fmt_float(d[0] as f64),
            XValue::Float64(d) => fmt_float(d[0]),
            XValue::CharByte(d) => String::from_utf8_lossy(d).into_owned(),
            XValue::CharAscii(d) => String::from_utf8_lossy(d).into_owned(),
            XValue::Char32(d) => d.iter().map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}')).collect(),
            XValue::Dict(d) => dict_value_string(d),
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

fn dict_value_string(childs: &[String]) -> String {
    if childs.len() == 1 && childs[0].is_empty() {
        "{empty}".to_string()
    } else if childs.is_empty() {
        KIND_DICT.to_string()
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
// XValueHead = [1B kind_len][kind][1B ref][1B arr_flag][1B ndim][ndim×4B dims][4B raw_len]
// body       = [raw]，offset = head_len()。
// ref: 0=内联 1=软链接(*) 2=扩展句柄(@)；ref=1 时 body 为目标 key 路径。None 编码为 nil。

pub fn tlv_encode(kind: &str, raw: &[u8], array_len: i32) -> Vec<u8> {
    let array_len = if array_len <= 0 { 1 } else { array_len };
    let (arr_flag, dims) = array_to_header(array_len);
    encode_head(kind, 0, arr_flag, &dims, raw)
}

pub fn tlv_encode_ptr(kind: &str, raw: &[u8], array_len: i32) -> Vec<u8> {
    let array_len = if array_len <= 0 { 1 } else { array_len };
    let (arr_flag, dims) = array_to_header(array_len);
    encode_head(kind, 1, arr_flag, &dims, raw)
}

/// arrayToHeader：将 arraylen 映射为 (arr_flag, dims)：<=1 标量，>1 连续一维数组。
fn array_to_header(array_len: i32) -> (i32, Vec<i32>) {
    if array_len <= 1 {
        (0, Vec::new())
    } else {
        (1, vec![array_len])
    }
}

pub fn encode_head(kind: &str, r#ref: i32, arr_flag: i32, dims: &[i32], raw: &[u8]) -> Vec<u8> {
    let ndim = dims.len() as i32;
    let mut buf = vec![0u8; 1 + kind.len() + 1 + 1 + 1 + 4 * dims.len() + 4 + raw.len()];
    buf[0] = kind.len() as u8;
    buf[1..1 + kind.len()].copy_from_slice(kind.as_bytes());
    let o = 1 + kind.len();
    buf[o] = r#ref as u8;
    buf[o + 1] = arr_flag as u8;
    buf[o + 2] = ndim as u8;
    for (i, d) in dims.iter().enumerate() {
        let off = o + 3 + 4 * i;
        buf[off..off + 4].copy_from_slice(&(*d as u32).to_le_bytes());
    }
    let raw_len_off = o + 3 + 4 * dims.len();
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
    if data.len() < o + 3 + 4 {
        return XValueHead::default();
    }
    let kind = String::from_utf8_lossy(&data[1..o]).into_owned();
    let r#ref = data[o] as i32;
    let arr_flag = data[o + 1] as i32;
    let ndim = data[o + 2] as i32;
    if data.len() < o + 3 + 4 * ndim as usize + 4 {
        return XValueHead::default();
    }
    let mut dims = Vec::with_capacity(ndim as usize);
    for i in 0..ndim as usize {
        let off = o + 3 + 4 * i;
        dims.push(i32::from_le_bytes(data[off..off + 4].try_into().unwrap()));
    }
    let raw_len_off = o + 3 + 4 * ndim as usize;
    let raw_len = u32::from_le_bytes(data[raw_len_off..raw_len_off + 4].try_into().unwrap()) as i32;
    let start = raw_len_off + 4;
    if data.len() < start + raw_len as usize {
        return XValueHead::default();
    }
    let array_len = header_array_len(arr_flag, ndim, &dims, raw_len, &kind);
    XValueHead {
        kind,
        is_ptr: r#ref == 1,
        array_len,
        r#ref,
        arr_flag,
        ndim,
        dims,
        body_len: raw_len,
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

/// headerArrayLen 从 arr_flag/ndim/dims 推导 arraylength。
fn header_array_len(arr_flag: i32, ndim: i32, dims: &[i32], raw_len: i32, kind: &str) -> i32 {
    if arr_flag == 0 {
        1
    } else if ndim > 0 {
        dims.iter().product()
    } else {
        let es = elem_size(kind);
        if es > 0 {
            raw_len / es
        } else {
            0
        }
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
