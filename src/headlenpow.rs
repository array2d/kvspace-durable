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

pub fn to_wire(storetype: u8, ptr: u8, body: &[u8]) -> Option<Vec<u8>> {
    if storetype > 3 || ptr > 2 {
        return None;
    }
    let len = u64::try_from(body.len()).ok()?;
    let total = PREFIX.checked_add(body.len())?;
    let mut data = Vec::new();
    data.try_reserve_exact(total).ok()?;
    data.resize(total, 0);
    write_len(&mut data, len);
    data[6] = storetype;
    data[7] = ptr;
    data[PREFIX..].copy_from_slice(body);
    Some(data)
}

/// 预留：产 len 字节零 body 的固定头（后由调用方填充）。
pub fn reserve(storetype: u8, ptr: u8, content: usize) -> Option<Vec<u8>> {
    if storetype > 3 || ptr > 2 {
        return None;
    }
    let len = u64::try_from(content).ok()?;
    let total = PREFIX.checked_add(content)?;
    let mut data = Vec::new();
    data.try_reserve_exact(total).ok()?;
    data.resize(total, 0);
    write_len(&mut data, len);
    data[6] = storetype;
    data[7] = ptr;
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures() {
        let scalar = to_wire(0, 0, &42i64.to_le_bytes()).unwrap();
        assert_eq!(scalar.len(), 16);
        assert_eq!(&scalar[0..6], &[8, 0, 0, 0, 0, 0]); /* len=8 @0-5 */
        assert_eq!(scalar[6], 0); /* storetype @6 */
        assert_eq!(scalar[7], 0); /* ptr @7 */
        assert_eq!(from_wire(&scalar).unwrap().len, 8);
        assert_eq!(from_wire(&scalar).unwrap().body, &42i64.to_le_bytes());

        let ptr = to_wire(1, 1, b"/varx").unwrap();
        assert_eq!(from_wire(&ptr).unwrap().ptr, 1);
        assert_eq!(from_wire(&ptr).unwrap().body, b"/varx");

        let ext = to_wire(3, 2, b"s3://").unwrap();
        assert_eq!(from_wire(&ext).unwrap().ptr, 2);
        assert_eq!(from_wire(&ext).unwrap().len, 5);

        let text = to_wire(1, 0, "你😀".as_bytes()).unwrap();
        assert_eq!(from_wire(&text).unwrap().len, 7); /* 你(3B)+😀(4B) */
    }

    #[test]
    fn malformed() {
        assert!(to_wire(4, 0, b"").is_none()); /* storetype > 3 */
        assert!(to_wire(0, 3, b"").is_none()); /* ptr > 2 */
        let mut v = to_wire(1, 0, b"x").unwrap();
        v[6] = 4; /* storetype @6 越界 */
        assert!(from_wire(&v).is_none());
        let mut v2 = to_wire(1, 0, b"x").unwrap();
        v2[7] = 3; /* ptr @7 越界 */
        assert!(from_wire(&v2).is_none());
        let mut v3 = to_wire(1, 0, b"xyz").unwrap();
        v3[4] = 0xff; /* len 越界 */
        assert!(from_wire(&v3).is_none());
    }

    #[test]
    fn reserved_body() {
        let mut v = reserve(1, 0, 3).unwrap();
        v[8..11].copy_from_slice("éa".as_bytes());
        assert_eq!(from_wire(&v).unwrap().len, 3);
        assert_eq!(from_wire(&v).unwrap().body, b"\xc3\xa9a");
    }

    #[test]
    fn u48_roundtrip() {
        for len in [0u64, 1, 0xffffff, 0xffffffff, 0xffffffffffff] {
            let mut v = reserve(1, 0, 0).unwrap();
            write_len(&mut v, len);
            assert_eq!(read_len(&v), Some(len), "len={len:#x}");
        }
    }
}
