pub const PREFIX: usize = 18;

#[derive(Debug, PartialEq)]
pub struct Head<'a> {
    pub pow: u8,
    pub flags: u8,
    pub a: u64,
    pub b: u64,
    pub langtype: &'a str,
    pub body: &'a [u8],
    pub content_len: usize,
    pub total: usize,
}

fn scalar_width(s: &str) -> Option<usize> {
    match s {
        "bool" | "int8" | "uint8" | "float8/e4m3" | "float8/e5m2" => Some(1),
        "int16" | "uint16" | "float16" | "bfloat16" => Some(2),
        "int32" | "uint32" | "float32" => Some(4),
        "int64" | "uint64" | "float64" => Some(8),
        _ => None,
    }
}

fn short_width(s: &str) -> Option<usize> {
    scalar_width(s).or_else(|| match s {
        "" | "def struct" | "lib" | "rwfunc" => Some(0),
        "def rwir" => Some(5),
        "time" | "duration" => Some(8),
        _ if s.starts_with('/') || s.contains('·') => Some(0),
        _ => None,
    })
}

fn min_pow(n: usize) -> Option<u8> {
    (5..=31).find(|&p| (n as u64 + PREFIX as u64) <= (1u64 << p))
}

fn decimal(s: &str) -> Option<u64> {
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    s.parse().ok()
}

fn slack_count(s: &str, body: &[u8]) -> Option<u64> {
    match s {
        "char/utf8" => Some(std::str::from_utf8(body).ok()?.chars().count() as u64),
        "byte" => Some(body.len() as u64),
        "char/ascii" if body.is_ascii() => Some(body.len() as u64),
        "char/utf32" if body.len() % 4 == 0 => {
            for chunk in body.chunks_exact(4) {
                let cp = u32::from_le_bytes(chunk.try_into().ok()?);
                char::from_u32(cp)?;
            }
            Some((body.len() / 4) as u64)
        }
        _ => None,
    }
}

fn valid_slack_type(langtype: &str, body: &[u8]) -> bool {
    let Some(end) = langtype.find(']') else {
        return false;
    };
    if !langtype.starts_with('[') {
        return false;
    }
    let Some(expected) = decimal(&langtype[1..end]) else {
        return false;
    };
    slack_count(&langtype[end + 1..], body) == Some(expected)
}

fn tensor_width(langtype: &str) -> Option<(u64, u64)> {
    let end = langtype.find(']')?;
    if !langtype.starts_with('[') {
        return None;
    }
    let dims = &langtype[1..end];
    if dims.is_empty() {
        return None;
    }
    let mut numel = 1u64;
    for dim in dims.split(',') {
        numel = numel.checked_mul(decimal(dim)?)?;
    }
    let width = scalar_width(&langtype[end + 1..])? as u64;
    Some((numel, width))
}

pub fn decode(data: &[u8]) -> Option<Head<'_>> {
    let pow = *data.first()?;
    if !(5..=31).contains(&pow) {
        return None;
    }
    let headlen = 1usize.checked_shl(pow as u32)?;
    if data.len() < headlen {
        return None;
    }
    let flags = data[1];
    if flags & !7 != 0 {
        return None;
    }
    let a = u64::from_le_bytes(data[2..10].try_into().ok()?);
    let b = u64::from_le_bytes(data[10..18].try_into().ok()?);
    let type_region = &data[PREFIX..headlen];
    let type_len = type_region
        .iter()
        .position(|&v| v == 0)
        .unwrap_or(type_region.len());
    let langtype = std::str::from_utf8(&type_region[..type_len]).ok()?;
    let (content, cap) = match flags {
        0 => {
            if a != 0 || b != 0 || min_pow(type_len)? != pow {
                return None;
            }
            let width = short_width(langtype)? as u64;
            (width, width)
        }
        1 => {
            if a > b {
                return None;
            }
            let code = langtype == "rwir" || langtype == "def langtype" || langtype == "rwfunc";
            if pow != if code { 5 } else { 6 } {
                return None;
            }
            if langtype == "rwir" && a < 5 {
                return None;
            }
            if langtype == "rwfunc" && a < 5 {
                return None;
            }
            (a, b)
        }
        2 => {
            if pow != 7 {
                return None;
            }
            let (numel, width) = tensor_width(langtype)?;
            if a != numel || b != width {
                return None;
            }
            let size = a.checked_mul(b)?;
            (size, size)
        }
        3 | 5 => {
            if langtype.is_empty() || a == 0 || a > b || min_pow(type_len)? != pow {
                return None;
            }
            (a, b)
        }
        _ => return None,
    };
    let content_len = usize::try_from(content).ok()?;
    let cap = usize::try_from(cap).ok()?;
    let total = headlen.checked_add(cap)?;
    if total > data.len() {
        return None;
    }
    let body = &data[headlen..total];
    if flags == 0 && langtype == "bool" && body[0] > 1 {
        return None;
    }
    if flags == 1
        && langtype != "rwir"
        && langtype != "def langtype"
        && langtype != "rwfunc"
        && !valid_slack_type(langtype, &body[..content_len])
    {
        return None;
    }
    if (flags == 3 || flags == 5)
        && (body[..content_len].contains(&0) || std::str::from_utf8(&body[..content_len]).is_err())
    {
        return None;
    }
    Some(Head {
        pow,
        flags,
        a,
        b,
        langtype,
        body,
        content_len,
        total,
    })
}

pub fn encode(
    pow: u8,
    flags: u8,
    a: u64,
    b: u64,
    langtype: &str,
    body: &[u8],
    cap: usize,
) -> Option<Vec<u8>> {
    if langtype.contains('\0') || body.len() > cap || !(5..=31).contains(&pow) {
        return None;
    }
    let headlen = 1usize.checked_shl(pow as u32)?;
    if langtype.len() > headlen.checked_sub(PREFIX)? {
        return None;
    }
    let total = headlen.checked_add(cap)?;
    let mut data = Vec::new();
    data.try_reserve_exact(total).ok()?;
    data.resize(total, 0);
    data[0] = pow;
    data[1] = flags;
    data[2..10].copy_from_slice(&a.to_le_bytes());
    data[10..18].copy_from_slice(&b.to_le_bytes());
    data[PREFIX..PREFIX + langtype.len()].copy_from_slice(langtype.as_bytes());
    data[headlen..headlen + body.len()].copy_from_slice(body);
    if decode(&data)?.content_len != body.len() {
        return None;
    }
    Some(data)
}

pub fn reserve(flags: u8, langtype: &str, content: usize, cap: usize) -> Option<Vec<u8>> {
    if langtype.contains('\0') || content > cap {
        return None;
    }
    let (pow, a, b) = match flags {
        0 if short_width(langtype)? == content && cap == content => {
            (min_pow(langtype.len())?, 0, 0)
        }
        1 => {
            let code = matches!(langtype, "rwir" | "def langtype" | "rwfunc");
            if code {
                if (langtype == "rwir" && content < 5) || (langtype == "rwfunc" && content < 5) {
                    return None;
                }
            } else {
                let end = langtype.find(']')?;
                if !langtype.starts_with('[')
                    || decimal(&langtype[1..end]).is_none()
                    || !matches!(
                        &langtype[end + 1..],
                        "char/utf8" | "byte" | "char/utf32" | "char/ascii"
                    )
                {
                    return None;
                }
            }
            (if code { 5 } else { 6 }, content as u64, cap as u64)
        }
        2 => {
            let (numel, width) = tensor_width(langtype)?;
            if numel.checked_mul(width)? != content as u64 || cap != content {
                return None;
            }
            (7, numel, width)
        }
        3 | 5 if !langtype.is_empty() && content > 0 => {
            (min_pow(langtype.len())?, content as u64, cap as u64)
        }
        _ => return None,
    };
    let headlen = 1usize.checked_shl(pow as u32)?;
    let total = headlen.checked_add(cap)?;
    let mut data = Vec::new();
    data.try_reserve_exact(total).ok()?;
    data.resize(total, 0);
    data[0] = pow;
    data[1] = flags;
    data[2..10].copy_from_slice(&a.to_le_bytes());
    data[10..18].copy_from_slice(&b.to_le_bytes());
    data[PREFIX..PREFIX + langtype.len()].copy_from_slice(langtype.as_bytes());
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures() {
        let scalar = encode(5, 0, 0, 0, "int64", &42i64.to_le_bytes(), 8).unwrap();
        assert_eq!(scalar.len(), 40);
        assert_eq!(&scalar[..2], &[5, 0]);
        assert_eq!(&scalar[18..23], b"int64");
        assert_eq!(decode(&scalar).unwrap().content_len, 8);

        let text = "你😀".as_bytes();
        let utf8 = encode(6, 1, text.len() as u64, 16, "[2]char/utf8", text, 16).unwrap();
        assert_eq!(utf8.len(), 80);
        assert_eq!(decode(&utf8).unwrap().content_len, text.len());

        let map_type = "[int64]·[]char/utf32";
        let map = encode(6, 0, 0, 0, map_type, &[], 0).unwrap();
        assert_eq!(decode(&map).unwrap().langtype, map_type);

        let def = encode(5, 0, 0, 0, "def rwir", &[0; 5], 5).unwrap();
        assert_eq!(decode(&def).unwrap().content_len, 5);

        let time = encode(5, 0, 0, 0, "time", &[1; 8], 8).unwrap();
        assert_eq!(decode(&time).unwrap().content_len, 8);

        let ptr = encode(5, 5, 5, 8, "int64", b"/varx", 8).unwrap();
        assert_eq!(decode(&ptr).unwrap().body.len(), 8);

        let ext = encode(5, 3, 5, 8, "int64", b"s3://", 8).unwrap();
        assert_eq!(decode(&ext).unwrap().content_len, 5);

        let rwir = encode(5, 1, 8, 8, "rwir", &[0; 8], 8).unwrap();
        assert_eq!(decode(&rwir).unwrap().content_len, 8);

        let def_type = encode(5, 1, 3, 3, "def langtype", b"int", 3).unwrap();
        assert_eq!(decode(&def_type).unwrap().content_len, 3);

        let func = encode(5, 0, 0, 0, "rwfunc", &[], 0).unwrap();
        let anchor = encode(5, 1, 5, 8, "rwfunc", &[0; 5], 8).unwrap();
        assert_eq!(decode(&func).unwrap().content_len, 0);
        assert_eq!(decode(&anchor).unwrap().content_len, 5);
        let call = encode(5, 1, 6, 6, "rwfunc", &[0, 0, 0, 0, 0, b'f'], 6).unwrap();
        assert_eq!(decode(&call).unwrap().body, &[0, 0, 0, 0, 0, b'f']);

        let tensor = encode(7, 2, 6, 4, "[2,3]float32", &[0; 24], 24).unwrap();
        assert_eq!(decode(&tensor).unwrap().total, 152);
    }

    #[test]
    fn malformed() {
        assert!(encode(5, 0, 0, 0, "int64", &[0; 7], 7).is_none());
        assert!(encode(5, 0, 0, 0, "bool", &[2], 1).is_none());
        assert!(encode(6, 1, 2, 2, "[2]char/utf8", b"\xc0\xaf", 2).is_none());
        assert!(encode(5, 5, 5, 8, "int64", b"/a\0bc", 8).is_none());
        assert!(encode(5, 3, 5, 8, "int64", b"s3\0//", 8).is_none());
        assert!(encode(5, 3, 0, 8, "int64", b"", 8).is_none());
        assert!(encode(5, 3, 1, 8, "int64", b"\xff", 8).is_none());
        assert!(encode(5, 1, 4, 8, "rwir", &[0; 4], 8).is_none());
        assert!(encode(5, 1, 4, 8, "rwfunc", &[0; 4], 8).is_none());
        assert!(encode(6, 1, 1, 1, "[1]char/ascii", &[0x80], 1).is_none());
        assert!(encode(6, 1, 4, 4, "[1]char/utf32", &0xd800u32.to_le_bytes(), 4).is_none());
        assert!(encode(6, 1, 4, 4, "[1]char/utf32", &0x110000u32.to_le_bytes(), 4).is_none());
        let mut ptr = encode(5, 5, 5, 8, "int64", b"/varx", 8).unwrap();
        ptr[1] |= 0x80;
        assert!(decode(&ptr).is_none());
    }

    #[test]
    fn reserved_body() {
        let mut v = reserve(1, "[2]char/utf8", 3, 9).unwrap();
        v[64..67].copy_from_slice("éa".as_bytes());
        let h = decode(&v).unwrap();
        assert_eq!((h.a, h.b, h.total), (3, 9, 73));
        assert!(reserve(2, "[2,3]float32", 23, 23).is_none());
        assert!(reserve(1, "rwfunc", 4, 5).is_none());
    }
}
