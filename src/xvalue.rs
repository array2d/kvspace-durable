// xvalue.rs — 对齐 xvalue.go
// XValue 是所有值的统一枚举（Go 的 interface + 具体类型 → 枚举变体）。
// XValueHead + TLV 编解码（head + body）。

use crate::r#const::*;

pub const REF_INLINE: u8 = 0;
pub const REF_PTR: u8 = 1;
pub const REF_EXT: u8 = 2;

pub fn is_map_langtype(kind: &str) -> bool {
    kind.contains(OBJ_SEP)
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
    pub r#ref: u8,
    pub storetype: u8, // Storage class.
    pub langtype: String,
    pub phys_dims: Vec<i32>,
    pub ro: bool,
    pub vid: u32,
    pub body_len: i32,
    pub body_cap: u64,
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
        if self.r#ref == REF_EXT {
            return XValue::Ext(ExtHandle {
                langtype: self.langtype.clone(),
                locator: String::from_utf8_lossy(body).into_owned(),
            });
        }
        let kind = self.kind();
        let dims = self.dims();
        // Map members live under the physical member prefix.
        if is_map_langtype(&kind) {
            return XValue::Map(MapValue { langtype: kind });
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
    Map(MapValue),
    Ext(ExtHandle),
    Opaque(Opaque),
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
            XValue::Ext(e) => e.langtype.as_str(),
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
            XValue::Map(_) | XValue::Ext(_) => 1,
            XValue::Opaque(o) => o.array_len,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            XValue::None => encode_head("None", 0, &[], &[]),
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
            XValue::Map(m) => encode_head(&m.langtype, 0, &[], &[]),
            XValue::Ext(e) => encode_head(&e.langtype, 2, &[], e.locator.as_bytes()),
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
            XValue::Map(_) => "map".to_string(),
            XValue::Ext(e) => e.locator.clone(),
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
/// A map value stores only its full key/value langtype.
#[derive(Clone, Debug, PartialEq)]
pub struct MapValue {
    pub langtype: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExtHandle {
    pub langtype: String,
    pub locator: String,
}

pub fn new_map_langtype(langtype: &str) -> XValue {
    XValue::Map(MapValue {
        langtype: langtype.to_string(),
    })
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

/// 未知 kind（非标准 XValue）的原样字节，供上层自定义 kind（如 kvlang 的 rwir/rwfunc）存取值。
#[derive(Clone, Debug, PartialEq)]
pub struct Opaque {
    pub kind: String,
    pub body: Vec<u8>,
    pub array_len: i32,
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

// Headlenpow XValue encoding.

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
    let (pow, flags, a, b, langtype) = if r#ref == 1 || r#ref == 2 {
        let pow = (5..=31).find(|&p| kind.len() + crate::headlenpow::PREFIX <= 1usize << p);
        let Some(pow) = pow else { return Vec::new() };
        (
            pow,
            if r#ref == 1 { 5 } else { 3 },
            raw.len() as u64,
            raw.len() as u64,
            kind.to_string(),
        )
    } else if r#ref != 0 {
        return Vec::new();
    } else if kind.is_empty() || kind == "None" {
        (5, 0, 0, 0, String::new())
    } else if matches!(kind, "char/utf8" | "char/ascii" | "char/utf32") {
        let count = match kind {
            "char/utf8" => std::str::from_utf8(raw).ok().map(|s| s.chars().count()),
            "char/ascii" if raw.is_ascii() => Some(raw.len()),
            "char/utf32"
                if raw.len() % 4 == 0
                    && raw.chunks_exact(4).all(|c| {
                        char::from_u32(u32::from_le_bytes(c.try_into().unwrap())).is_some()
                    }) =>
            {
                Some(raw.len() / 4)
            }
            _ => None,
        };
        let Some(count) = count else {
            return Vec::new();
        };
        (
            6,
            1,
            raw.len() as u64,
            raw.len() as u64,
            format!("[{count}]{kind}"),
        )
    } else if matches!(kind, "rwir" | "rwfunc") && raw.len() >= 5 || kind == "def langtype" {
        (5, 1, raw.len() as u64, raw.len() as u64, kind.to_string())
    } else if !dims.is_empty() && !kind.contains('·') && !kind.starts_with('/') {
        let Some(numel) = dims.iter().try_fold(1u64, |n, &d| {
            u64::try_from(d).ok().and_then(|d| n.checked_mul(d))
        }) else {
            return Vec::new();
        };
        let width = elem_size(kind);
        if width <= 0 {
            return Vec::new();
        }
        (
            7,
            2,
            numel,
            width as u64,
            format!(
                "[{}]{kind}",
                dims.iter()
                    .map(i32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        )
    } else {
        let pow = (5..=31).find(|&p| kind.len() + crate::headlenpow::PREFIX <= 1usize << p);
        let Some(pow) = pow else { return Vec::new() };
        (pow, 0, 0, 0, kind.to_string())
    };
    crate::headlenpow::encode(pow, flags, a, b, &langtype, raw, raw.len()).unwrap_or_default()
}

pub fn decode_xvalue_head(data: &[u8]) -> XValueHead {
    let Some(h) = crate::headlenpow::decode(data) else {
        return XValueHead::default();
    };
    if h.total != data.len() {
        return XValueHead::default();
    }
    let (Ok(headlen), Ok(body_len)) =
        (u16::try_from(1usize << h.pow), i32::try_from(h.content_len))
    else {
        return XValueHead::default();
    };
    let dims = if h.flags == 2 || (h.flags == 1 && h.langtype.starts_with('[')) {
        parse_kindexpr(h.langtype).0
    } else {
        Vec::new()
    };
    XValueHead {
        headlen,
        r#ref: if h.flags == 5 {
            REF_PTR
        } else if h.flags == 3 {
            REF_EXT
        } else {
            REF_INLINE
        },
        storetype: h.flags & 3,
        langtype: h.langtype.to_string(),
        phys_dims: dims,
        ro: false,
        vid: 0,
        body_len,
        body_cap: (h.total - headlen as usize) as u64,
    }
}

/// 解析完整 XValue（head + body）为 XValue。
pub fn decode_xvalue(data: &[u8]) -> XValue {
    let h = decode_xvalue_head(data);
    // An empty inline langtype denotes None.
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
            let s = if dims.is_empty() {
                kind.to_string()
            } else {
                format!(
                    "[{}]{kind}",
                    dims.iter()
                        .map(i32::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                )
            };
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
    fn directory_and_extension_wire() {
        let dir = encode_head("lib", 0, &[], &[]);
        let head = crate::headlenpow::decode(&dir).unwrap();
        assert_eq!((head.flags, head.langtype, head.content_len), (0, "lib", 0));
        let ext = XValue::Ext(ExtHandle {
            langtype: "rwfunc".into(),
            locator: "/lib/f/".into(),
        })
        .encode();
        let head = crate::headlenpow::decode(&ext).unwrap();
        assert_eq!(
            (head.flags, head.langtype, head.body),
            (3, "rwfunc", b"/lib/f/".as_slice())
        );
        assert_eq!(
            decode_xvalue(&ext),
            XValue::Ext(ExtHandle {
                langtype: "rwfunc".into(),
                locator: "/lib/f/".into()
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
