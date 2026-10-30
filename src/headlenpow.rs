pub const PREFIX: usize = 8;

/// u48 len 的唯一定义点：wire 是 6 字节小端（offset 0-5），Rust 侧一律经
/// read_len/write_len 读写，禁止各处手写拼接。
#[inline]
pub fn read_len(data: &[u8]) -> Option<u64> {
    let b = data.get(0..6)?;
    let mut v = 0u64;
    for i in (0..6).rev() {
        v = (v << 8) | u64::from(b[i]);
    }
    Some(v)
}

#[inline]
pub fn write_len(data: &mut [u8], v: u64) {
    for i in 0..6 {
        data[i] = (v >> (8 * i)) as u8;
    }
}

#[derive(Debug, PartialEq)]
pub struct Head<'a> {
    pub storetype: u8,
    pub ptr: u8,
    pub len: u64,
    pub body: &'a [u8],
}

pub fn from_wire(data: &[u8]) -> Option<Head<'_>> {
    if data.len() < PREFIX {
        return None;
    }
    let storetype = data[6];
    let ptr = data[7];
    if storetype > 3 || ptr > 2 {
        return None;
    }
    let len = read_len(data)?;
    let blen = usize::try_from(len).ok()?;
    if PREFIX.checked_add(blen)? > data.len() {
        return None;
    }
    Some(Head {
        storetype,
        ptr,
        len,
        body: &data[PREFIX..PREFIX + blen],
    })
}

/// attr 区（#571）：body 后恒跟 [attr_len:u16 LE][records]，record = [tag:u8][len:u16][data]，
/// tag 1 = langtype，2+ 预留。attr_len 含自身 2 字节；空 attr = [2]。total = 8 + len + attr_len。
pub const ATTR_TAG_LANGTYPE: u8 = 1;

/// 构造 attr 区字节（含 attr_len 前缀）；langtype 空 → 空 attr `[2,0]`。
pub fn attr_bytes(langtype: &str) -> Option<Vec<u8>> {
    if langtype.is_empty() {
        return Some(vec![2, 0]);
    }
    let l = langtype.len();
    let alen = 2usize.checked_add(3)?.checked_add(l)?;
    let alen16 = u16::try_from(alen).ok()?;
    let mut v = Vec::with_capacity(5 + l);
    v.push((alen16 & 0xff) as u8);
    v.push((alen16 >> 8) as u8);
    v.push(ATTR_TAG_LANGTYPE);
    v.push((l & 0xff) as u8);
    v.push((l >> 8) as u8);
    v.extend_from_slice(langtype.as_bytes());
    Some(v)
}

/// 读 attr 总长（含自身 2 字节；空 attr = 2）。坏 wire → None。
#[inline]
pub fn attr_len(data: &[u8], body_len: usize) -> Option<u16> {
    let p = data.get(PREFIX.checked_add(body_len)?..PREFIX + body_len + 2)?;
    Some(u16::from_le_bytes([p[0], p[1]]))
}

/// 从 wire attr 区取 tag=1（langtype）记录 data；无/坏 attr → None。
pub fn langtype_from_wire(data: &[u8], body_len: usize) -> Option<&[u8]> {
    let alen = usize::from(attr_len(data, body_len)?);
    if alen < 2 {
        return None;
    }
    let mut p = PREFIX + body_len + 2;
    let end = PREFIX + body_len + alen;
    while p + 3 <= end {
        let tag = data[p];
        let rlen = usize::from(u16::from_le_bytes([data[p + 1], data[p + 2]]));
        if p + 3 + rlen > end {
            return None;
        }
        if tag == ATTR_TAG_LANGTYPE {
            return Some(&data[p + 3..p + 3 + rlen]);
        }
        p += 3 + rlen;
    }
    None
}

pub fn to_wire(storetype: u8, ptr: u8, body: &[u8], langtype: &str) -> Option<Vec<u8>> {
    if storetype > 3 || ptr > 2 {
        return None;
    }
    let attr = attr_bytes(langtype)?;
    let len = u64::try_from(body.len()).ok()?;
    let total = PREFIX.checked_add(body.len())?.checked_add(attr.len())?;
    let mut data = Vec::new();
    data.try_reserve_exact(total).ok()?;
    data.resize(total, 0);
    write_len(&mut data, len);
    data[6] = storetype;
    data[7] = ptr;
    data[PREFIX..PREFIX + body.len()].copy_from_slice(body);
    data[PREFIX + body.len()..].copy_from_slice(&attr);
    Some(data)
}

/// 预留：产 len 字节零 body 的固定头 + attr 区（后由调用方填充 body）。
pub fn reserve(storetype: u8, ptr: u8, content: usize, langtype: &str) -> Option<Vec<u8>> {
    if storetype > 3 || ptr > 2 {
        return None;
    }
    let attr = attr_bytes(langtype)?;
    let len = u64::try_from(content).ok()?;
    let total = PREFIX.checked_add(content)?.checked_add(attr.len())?;
    let mut data = Vec::new();
    data.try_reserve_exact(total).ok()?;
    data.resize(total, 0);
    write_len(&mut data, len);
    data[6] = storetype;
    data[7] = ptr;
    data[PREFIX + content..].copy_from_slice(&attr);
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures() {
        let scalar = to_wire(0, 0, &42i64.to_le_bytes(), "int64").unwrap();
        assert_eq!(scalar.len(), 26); /* 8 + 8 body + 10 attr */
        assert_eq!(&scalar[0..6], &[8, 0, 0, 0, 0, 0]); /* len=8 @0-5 */
        assert_eq!(scalar[6], 0); /* storetype @6 */
        assert_eq!(scalar[7], 0); /* ptr @7 */
        assert_eq!(from_wire(&scalar).unwrap().len, 8);
        assert_eq!(from_wire(&scalar).unwrap().body, &42i64.to_le_bytes());
        assert_eq!(langtype_from_wire(&scalar, 8).unwrap(), b"int64");

        let ptr = to_wire(1, 1, b"/varx", "int64").unwrap();
        assert_eq!(from_wire(&ptr).unwrap().ptr, 1);
        assert_eq!(from_wire(&ptr).unwrap().body, b"/varx");
        assert_eq!(langtype_from_wire(&ptr, 5).unwrap(), b"int64");

        let ext = to_wire(3, 2, b"s3://", "").unwrap();
        assert_eq!(from_wire(&ext).unwrap().ptr, 2);
        assert_eq!(from_wire(&ext).unwrap().len, 5);
        assert_eq!(attr_len(&ext, 5).unwrap(), 2); /* 空 attr */

        let text = to_wire(1, 0, "你😀".as_bytes(), "char/utf8").unwrap();
        assert_eq!(from_wire(&text).unwrap().len, 7); /* 你(3B)+😀(4B) */
        assert_eq!(langtype_from_wire(&text, 7).unwrap(), b"char/utf8");
    }

    #[test]
    fn malformed() {
        assert!(to_wire(4, 0, b"", "").is_none()); /* storetype > 3 */
        assert!(to_wire(0, 3, b"", "").is_none()); /* ptr > 2 */
        let mut v = to_wire(1, 0, b"x", "").unwrap();
        v[6] = 4; /* storetype @6 越界 */
        assert!(from_wire(&v).is_none());
        let mut v2 = to_wire(1, 0, b"x", "").unwrap();
        v2[7] = 3; /* ptr @7 越界 */
        assert!(from_wire(&v2).is_none());
        let mut v3 = to_wire(1, 0, b"xyz", "").unwrap();
        v3[4] = 0xff; /* len 越界 */
        assert!(from_wire(&v3).is_none());
    }

    #[test]
    fn reserved_body() {
        let mut v = reserve(1, 0, 3, "char/utf8").unwrap();
        v[8..11].copy_from_slice("éa".as_bytes());
        assert_eq!(from_wire(&v).unwrap().len, 3);
        assert_eq!(from_wire(&v).unwrap().body, b"\xc3\xa9a");
        assert_eq!(langtype_from_wire(&v, 3).unwrap(), b"char/utf8");
    }

    #[test]
    fn u48_roundtrip() {
        for len in [0u64, 1, 0xffffff, 0xffffffff, 0xffffffffffff] {
            let mut v = reserve(1, 0, 0, "").unwrap();
            write_len(&mut v, len);
            assert_eq!(read_len(&v), Some(len), "len={len:#x}");
        }
    }
}
