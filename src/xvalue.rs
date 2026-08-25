// xvalue.rs — 对齐 xvalue.go
// XValue 是所有值的统一枚举（Go 的 interface + 具体类型 → 枚举变体）。
// XValueHead + TLV 编解码（head + body）。

use crate::r#const::*;

// ── XValueHead ─────────────────────────────────────────────────────────────
// XValueHead = [1B kindexprlen][kindexpr 含 0x00 padding][1B ro][4B vid LE][4B body_len LE]
//   kindexpr 串首字节 * =软链接 / @ =扩展句柄 / 无 =内联，其后 [d0,d1]kind 承载 ndim+dims：
//   裸 kind=标量(ndim=0)、[n]kind=一维、[d0,d1]kind=多维。kindexprlen 为槽总长（含 padding），
//   reshape 时新 kindexpr 不超过槽长即可原地改写不搬 body；内容以首个 NUL 终止。

/// kindexpr 构建：ref 前缀(*/@) + [dims] + kind。
fn kindexpr_string(kind: &str, r#ref: i32, dims: &[i32]) -> String {
    let mut s = String::new();
    if r#ref == 1 {
        s.push('*');
    } else if r#ref == 2 {
        s.push('@');
    }
    if !dims.is_empty() {
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

/// kindexpr 解析 → (ref, dims, kind)：首字节 */@ 表 ref，[dims] 段表形状，其余为基 kind。
fn parse_kindexpr(s: &str) -> (i32, Vec<i32>, String) {
    let (r#ref, rest) = match s.as_bytes().first() {
        Some(b'*') => (1, &s[1..]),
        Some(b'@') => (2, &s[1..]),
        _ => (0, s),
    };
    if rest.starts_with('[') {
        match rest.find(']') {
            Some(end) => (
                r#ref,
                rest[1..end]
                    .split(',')
                    .filter(|d| !d.is_empty())
                    .map(|d| d.parse().unwrap_or(0))
                    .collect(),
                rest[end + 1..].to_string(),
            ),
            None => (r#ref, Vec::new(), rest.to_string()),
        }
    } else {
        (r#ref, Vec::new(), rest.to_string())
    }
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct XValueHead {
    pub kindexpr: String, // 内容（含 */@ 前缀与 [dims]，去 NUL/padding）
    pub kindexprlen: u8,  // wire 槽总长（内容 + NUL + padding）
    pub ro: bool,
    pub vid: u32,
    pub body_len: i32,
}

impl XValueHead {
    fn parse(&self) -> (i32, Vec<i32>, String) {
        parse_kindexpr(&self.kindexpr)
    }
    pub fn r#ref(&self) -> i32 {
        self.parse().0
    }
    pub fn is_ptr(&self) -> bool {
        self.parse().0 == 1
    }
    pub fn kind(&self) -> String {
        self.parse().2
    }
    pub fn dims(&self) -> Vec<i32> {
        self.parse().1
    }
    pub fn ndim(&self) -> i32 {
        self.parse().1.len() as i32
    }
    pub fn array_len(&self) -> i32 {
        let dims = self.parse().1;
        if dims.is_empty() {
            1
        } else {
            dims.iter().product()
        }
    }

    /// 返回 XValueHead（元数据）字节数，不含 body。
    pub fn head_len(&self) -> i32 {
        self.kindexprlen as i32 + 10
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
                kind: self.kind(),
                target: String::from_utf8_lossy(body).into_owned(),
                array_len: self.array_len(),
            });
        }
        let kind = self.kind();
        let dims = self.dims();
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
            KIND_OBJ => {
                if body.is_empty() {
                    XValue::Obj(Vec::new())
                } else {
                    XValue::Obj(crate::xvalue_index::decode_obj_index(body))
                }
            }
            KIND_MAP => XValue::Map(MapIndex {
                childs: crate::xvalue_index::decode_index(body),
                dims: dims.clone(),
            }),
            KIND_INDEX => XValue::Index(crate::xvalue_index::decode_index(body)),
            KIND_EXT_INDEX => XValue::ExtIndex(crate::xvalue_index::decode_ext_index(body)),
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
    Obj(Vec<String>),   // objindex（命名成员对象：键为任意字符串，值 json）
    Map(MapIndex), // strkeymapindex（散 key ndarray：键为坐标段 [s0,s1,...]，恒 ndim≥1）
    Index(Vec<String>), // index
    ExtIndex(ExtIndex), // extindex
    Opaque(Opaque),     // 未知 kind（如 kvlang 的 rwir/rwfunc/scope），原样存取
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
            XValue::Obj(d) => crate::xvalue_index::encode_index_raw(d).len() as i32,
            XValue::Map(m) => crate::xvalue_index::encode_index_raw(&m.childs).len() as i32,
            XValue::Index(d) => crate::xvalue_index::encode_index_raw(d).len() as i32,
            XValue::ExtIndex(e) => {
                crate::xvalue_index::encode_ext_index_raw(&e.ext_path, &e.childs).len() as i32
            }
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
            XValue::Map(m) => m.dims.iter().product(),
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
                let raw = crate::xvalue_index::encode_index_raw(d);
                tlv_encode(KIND_OBJ, &raw, 1)
            }
            XValue::Map(m) => {
                let raw = crate::xvalue_index::encode_index_raw(&m.childs);
                encode_head(KIND_MAP, 0, &m.dims, &raw)
            }
            XValue::Index(d) => {
                let raw = crate::xvalue_index::encode_index_raw(d);
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
            XValue::Char32(d) => d
                .data
                .iter()
                .map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}'))
                .collect(),
            XValue::Obj(d) => obj_value_string(d),
            XValue::Map(m) => format!(
                "map[{}]{{{}}}",
                m.dims
                    .iter()
                    .map(|d| d.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
                m.childs.len()
            ),
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
    pub kind: String,   // 目标类型
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

/// strkeymapindex：散 key ndarray。dims 是逻辑形状（恒 ndim≥1），childs 是实际存在的坐标段成员。
/// 二者可不一致：坐标可缺席，缺席坐标读为 None。
#[derive(Clone, Debug, PartialEq)]
pub struct MapIndex {
    pub childs: Vec<String>,
    pub dims: Vec<i32>,
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
    if b {
        "true".to_string()
    } else {
        "false".to_string()
    }
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

pub fn encode_head_perm(
    kind: &str,
    r#ref: i32,
    dims: &[i32],
    raw: &[u8],
    ro: bool,
    vid: u32,
) -> Vec<u8> {
    let kx = kindexpr_string(kind, r#ref, dims);
    let slot = (kx.len() + 1) as u8; // 内容 + 1 NUL（当前无额外 padding）
    let mut buf = vec![0u8; 1 + slot as usize + 1 + 4 + 4 + raw.len()];
    buf[0] = slot;
    buf[1..1 + kx.len()].copy_from_slice(kx.as_bytes());
    let o = 1 + slot as usize;
    buf[o] = ro as u8;
    buf[o + 1..o + 5].copy_from_slice(&vid.to_le_bytes());
    buf[o + 5..o + 9].copy_from_slice(&(raw.len() as u32).to_le_bytes());
    buf[o + 9..].copy_from_slice(raw);
    buf
}

pub fn decode_xvalue_head(data: &[u8]) -> XValueHead {
    if data.is_empty() {
        return XValueHead::default();
    }
    let slot = data[0] as usize;
    let o = 1 + slot;
    if data.len() < o + 9 {
        return XValueHead::default();
    }
    let kx_bytes = &data[1..o];
    let end = kx_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(kx_bytes.len());
    let body_len = u32::from_le_bytes(data[o + 5..o + 9].try_into().unwrap()) as i32;
    if data.len() < o + 9 + body_len as usize {
        return XValueHead::default();
    }
    XValueHead {
        kindexpr: String::from_utf8_lossy(&kx_bytes[..end]).into_owned(),
        kindexprlen: slot as u8,
        ro: data[o] != 0,
        vid: u32::from_le_bytes(data[o + 1..o + 5].try_into().unwrap()),
        body_len,
    }
}

/// 解析完整 XValue（head + body）为 XValue。
pub fn decode_xvalue(data: &[u8]) -> XValue {
    let h = decode_xvalue_head(data);
    if h.kindexpr.is_empty() {
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
    fn kindexpr_build_parse() {
        for (kind, r#ref, dims) in [
            ("int64", 0, vec![]),
            ("float32", 0, vec![5]),
            ("float64", 0, vec![2, 3]),
            ("char/utf32", 0, vec![0]),
            ("int64", 1, vec![]),
            ("rwir", 2, vec![]),
        ] {
            let s = kindexpr_string(kind, r#ref, &dims);
            let (r2, d2, k2) = parse_kindexpr(&s);
            assert_eq!(
                (r#ref, dims, kind.to_string()),
                (r2, d2, k2),
                "kindexpr {}",
                s
            );
        }
    }

    #[test]
    fn roundtrip_values() {
        roundtrip(&new_int64(&[42]));
        roundtrip(&new_float64(&[1.5]));
        roundtrip(&new_bool(&[true]));
        roundtrip(&new_char_byte(b"hello"));
        roundtrip(&new_char_byte(b""));
        roundtrip(&new_ptr("int64", "/x/y", 1));
        roundtrip(&XValue::Int32(Arr {
            data: vec![1, 2, 3, 4, 5, 6],
            dims: vec![2, 3],
        }));
        roundtrip(&XValue::Obj(vec!["a".to_string(), "b".to_string()]));
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
