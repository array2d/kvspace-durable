//! XValue wire 的 head 层：16 字节固定头 + body + attr 区。
//!
//! ```text
//! wire: [bodylen:u64][attrslen:u32][storetype:u8][ptr:u8][pad:2][body][attr records]
//! 总长度 = 16 + bodylen + attrslen
//! ```
//!
//! 长度只从 head 读；attr 区**不带**自带长度前缀（attrslen 已在 head 里，再来一个
//! 前缀就是第二个事实源）。wire 是小端，读写一律走 `from_le_bytes` /
//! `to_le_bytes`，不依赖主机字节序。旧 8 字节 head 不做兼容：遇到即失败。

pub const PREFIX: usize = 16;

/// head 字段偏移（唯一定义点；外部一律经 `Head` 访问，不手写偏移）。
const OFF_BODYLEN: usize = 0;
const OFF_ATTRSLEN: usize = 8;
const OFF_STORETYPE: usize = 12;
const OFF_PTR: usize = 13;
const OFF_PADDING: usize = 14;

const _: () = assert!(
    PREFIX == 16
        && OFF_BODYLEN + 8 == OFF_ATTRSLEN
        && OFF_ATTRSLEN + 4 == OFF_STORETYPE
        && OFF_STORETYPE + 1 == OFF_PTR
        && OFF_PTR + 1 == OFF_PADDING
        && OFF_PADDING + 2 == PREFIX,
    "head field offsets must tile exactly 16 bytes"
);

/// 解析出的 head。`body` 借自输入；`attrs` 是 attr 区原始字节（记录序列，无前缀）。
#[derive(Debug, PartialEq)]
pub struct Head<'a> {
    pub storetype: u8,
    pub ptr: u8,
    pub len: u64,
    pub attrs: &'a [u8],
    pub body: &'a [u8],
}

/// padding 必须为零：它是 ABI 的一部分，不是「随便填」。
#[inline]
fn padding_is_zero(data: &[u8]) -> bool {
    data.get(OFF_PADDING) == Some(&0) && data.get(OFF_PADDING + 1) == Some(&0)
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    let b = data.get(at..at + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_u64(data: &[u8], at: usize) -> Option<u64> {
    let b = data.get(at..at + 8)?;
    Some(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

/// 解析 wire。任何一项不成立都返回 `None`：head 不足 16 字节、padding 非零、
/// storetype/ptr 越界、`16 + bodylen + attrslen` 溢出或超出输入长度。
pub fn from_wire(data: &[u8]) -> Option<Head<'_>> {
    if data.len() < PREFIX {
        return None;
    }
    if !padding_is_zero(data) {
        return None;
    }
    let storetype = *data.get(OFF_STORETYPE)?;
    let ptr = *data.get(OFF_PTR)?;
    if storetype > 3 || ptr > 2 {
        return None;
    }
    let len = read_u64(data, OFF_BODYLEN)?;
    let attrslen = read_u32(data, OFF_ATTRSLEN)?;
    let body_start = PREFIX;
    let body_end = body_start.checked_add(usize::try_from(len).ok()?)?;
    let attrs_end = body_end.checked_add(usize::try_from(attrslen).ok()?)?;
    if attrs_end > data.len() {
        return None;
    }
    Some(Head {
        storetype,
        ptr,
        len,
        body: &data[body_start..body_end],
        attrs: &data[body_end..attrs_end],
    })
}

/// attr 区（无前缀）：`record = [tag:u8][len:u16][data]`，tag 1 = langtype，2+ 预留。
pub const ATTR_TAG_LANGTYPE: u8 = 1;

/// 构造 attr 区字节（**不含**任何长度前缀）；langtype 空 → 空 attr（零字节）。
pub fn attr_bytes(langtype: &str) -> Option<Vec<u8>> {
    if langtype.is_empty() {
        return Some(Vec::new());
    }
    let l = langtype.len();
    if l > usize::from(u16::MAX) {
        return None;
    }
    let mut v = Vec::with_capacity(3 + l);
    v.push(ATTR_TAG_LANGTYPE);
    v.push((l & 0xff) as u8);
    v.push((l >> 8) as u8);
    v.extend_from_slice(langtype.as_bytes());
    Some(v)
}

/// attr 区总字节数（从 head 读，不含任何前缀；空 attr = 0）。坏 wire → None。
#[inline]
pub fn attr_len(data: &[u8]) -> Option<u32> {
    if data.len() < PREFIX {
        return None;
    }
    read_u32(data, OFF_ATTRSLEN)
}

/// 从 attr 区取 tag=1（langtype）记录 data；无/坏 attr → None。
/// 只扫 head 声明的 attr 范围内实际存在的字节（与 C `kvspaceXvalueHeadLangtype`
/// 对齐），截断或畸形记录返回 `None` 而不 panic。
pub fn langtype_from_wire(data: &[u8]) -> Option<&[u8]> {
    let head = from_wire(data)?;
    let mut rest = head.attrs;
    while let [tag, l0, l1, tail @ ..] = rest {
        let rlen = usize::from(u16::from_le_bytes([*l0, *l1]));
        let rec = tail.get(..rlen)?;
        if *tag == ATTR_TAG_LANGTYPE {
            return Some(rec);
        }
        rest = &tail[rlen..];
    }
    None
}

pub fn to_wire(storetype: u8, ptr: u8, body: &[u8], langtype: &str) -> Option<Vec<u8>> {
    let mut data = reserve(storetype, ptr, body.len(), langtype)?;
    data[PREFIX..PREFIX + body.len()].copy_from_slice(body);
    Some(data)
}

/// 预留：产 bodylen 字节零 body 的固定头 + attr 区（后由调用方填充 body）。
/// padding 显式写成 0——`resize` 已零填，这里再写一次是为了让「必须为零」这件事
/// 在代码里可见，而不是依赖初始化副作用。
pub fn reserve(storetype: u8, ptr: u8, content: usize, langtype: &str) -> Option<Vec<u8>> {
    if storetype > 3 || ptr > 2 {
        return None;
    }
    let attr = attr_bytes(langtype)?;
    let attrslen = u32::try_from(attr.len()).ok()?;
    let total = PREFIX.checked_add(content)?.checked_add(attr.len())?;
    let mut data = Vec::new();
    data.try_reserve_exact(total).ok()?;
    data.resize(total, 0);
    data[OFF_BODYLEN..OFF_BODYLEN + 8].copy_from_slice(&(content as u64).to_le_bytes());
    data[OFF_ATTRSLEN..OFF_ATTRSLEN + 4].copy_from_slice(&attrslen.to_le_bytes());
    data[OFF_STORETYPE] = storetype;
    data[OFF_PTR] = ptr;
    data[OFF_PADDING] = 0;
    data[OFF_PADDING + 1] = 0;
    data[PREFIX + content..].copy_from_slice(&attr);
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures() {
        let scalar = to_wire(0, 0, &42i64.to_le_bytes(), "int64").unwrap();
        /* 16 head + 8 body + 8 attr（3 头 + "int64"） */
        assert_eq!(scalar.len(), 32);
        assert_eq!(&scalar[0..8], &8u64.to_le_bytes()); /* bodylen @0-7 */
        assert_eq!(&scalar[8..12], &8u32.to_le_bytes()); /* attrslen @8-11 */
        assert_eq!(scalar[12], 0); /* storetype @12 */
        assert_eq!(scalar[13], 0); /* ptr @13 */
        assert_eq!(&scalar[14..16], &[0, 0]); /* padding @14-15 */
        let h = from_wire(&scalar).unwrap();
        assert_eq!(h.len, 8);
        assert_eq!(h.body, &42i64.to_le_bytes());
        assert_eq!(langtype_from_wire(&scalar).unwrap(), b"int64");

        let ptr = to_wire(1, 1, b"/varx", "int64").unwrap();
        assert_eq!(from_wire(&ptr).unwrap().ptr, 1);
        assert_eq!(from_wire(&ptr).unwrap().body, b"/varx");
        assert_eq!(langtype_from_wire(&ptr).unwrap(), b"int64");

        let ext = to_wire(3, 2, b"s3://", "").unwrap();
        assert_eq!(from_wire(&ext).unwrap().ptr, 2);
        assert_eq!(from_wire(&ext).unwrap().len, 5);
        assert_eq!(attr_len(&ext).unwrap(), 0); /* 空 attr = 0 字节，不是一个前缀 */
        assert_eq!(ext.len(), 16 + 5); /* 没有 attr 区 */

        let text = to_wire(1, 0, "你😀".as_bytes(), "char/utf8").unwrap();
        assert_eq!(from_wire(&text).unwrap().len, 7); /* 你(3B)+😀(4B) */
        assert_eq!(langtype_from_wire(&text).unwrap(), b"char/utf8");
    }

    #[test]
    fn malformed() {
        assert!(to_wire(4, 0, b"", "").is_none()); /* storetype > 3 */
        assert!(to_wire(0, 3, b"", "").is_none()); /* ptr > 2 */

        let mut v = to_wire(1, 0, b"x", "").unwrap();
        v[12] = 4; /* storetype @12 越界 */
        assert!(from_wire(&v).is_none());
        let mut v2 = to_wire(1, 0, b"x", "").unwrap();
        v2[13] = 3; /* ptr @13 越界 */
        assert!(from_wire(&v2).is_none());
        let mut v3 = to_wire(1, 0, b"xyz", "").unwrap();
        v3[0] = 0xff; /* bodylen 越界 */
        assert!(from_wire(&v3).is_none());
        let mut v4 = to_wire(1, 0, b"xyz", "").unwrap();
        v4[8] = 0xff; /* attrslen 越界 */
        assert!(from_wire(&v4).is_none());

        /* padding 非零即非法（两个字节都测） */
        for at in [14usize, 15] {
            let mut p = to_wire(1, 0, b"x", "").unwrap();
            p[at] = 1;
            assert!(from_wire(&p).is_none(), "padding byte {at} must be zero");
        }
    }

    /// 旧 8 字节 head 不做兼容：**旧 wire 不得被当作合法新 wire 接受**
    /// ——但必须如实记录这条保证的边界。
    ///
    /// 无版本号的定长布局无法从字节本身分辨「旧 head」与「新 head」：旧 wire 的
    /// body、attr 前缀会被读成新 head 的 bodylen/attrslen/padding。只有当这些
    /// 字节恰好凑出「padding 为零 + storetype/ptr 合法 + 总长不越界」时，解析器
    /// **无法拒绝**——因为那个字节串在语法上确实是一个合法的新 wire。
    ///
    /// 所以裁决 §4.4 的「不兼容」实际含义是：**不写迁移代码、不双读旧值**，
    /// 而不是「旧 wire 必然被识别并拒绝」。本测试把两种情形都钉住，避免将来有人
    /// 以为有强保证。
    #[test]
    fn old_eight_byte_head_is_not_accepted_as_a_legal_wire() {
        /* 情形 A：旧 wire 的 body 使新视角下 attrslen 巨大 → 总长越界 → 拒绝。 */
        let mut overrun = vec![0u8; 8 + 8 + 10];
        overrun[0] = 8; /* 旧 len=8 */
        overrun[8] = 0xff; /* 新视角 attrslen 低字节 → 巨大 */
        overrun[24] = 10; /* 旧 attr_len=10 */
        assert!(
            from_wire(&overrun).is_none(),
            "旧 wire 若被解读成越界长度必须拒绝"
        );

        /* 情形 B（已知边界，不是缺陷）：旧 wire 的 body 全零时，新视角恰好得到
        bodylen=8、attrslen=0、padding=0、总长 24 <= 26，于是**会被当成合法
        新 wire**。任何无版本号的定长头都有这个性质。测试在此显式记录，
        以免「旧 wire 一律被拒」的误解传播。 */
        let mut alias = vec![0u8; 8 + 8 + 10];
        alias[0] = 8;
        alias[24] = 10;
        let parsed = from_wire(&alias);
        assert!(
            parsed.is_some(),
            "全零 body 的旧 wire 会被误认成新 wire —— 无版本号布局的已知边界"
        );
        /* 而且解析结果是"合理"的：bodylen/attrslen 都取自真实字节，不是垃圾。 */
        let h = parsed.unwrap();
        assert_eq!(h.len, 8);
        assert_eq!(h.body.len(), 8);
        assert_eq!(h.attrs.len(), 0);
    }

    #[test]
    fn reserved_body() {
        let mut v = reserve(1, 0, 3, "char/utf8").unwrap();
        v[PREFIX..PREFIX + 3].copy_from_slice(b"\xc3\xa9a");
        assert_eq!(from_wire(&v).unwrap().len, 3);
        assert_eq!(from_wire(&v).unwrap().body, b"\xc3\xa9a");
        assert_eq!(langtype_from_wire(&v).unwrap(), b"char/utf8");
    }

    #[test]
    fn byte_order_is_little_endian() {
        /* 用非对称值盯住字节序：读出反了就不可能相等。 */
        let body = vec![0xAAu8; 6];
        let wire = to_wire(1, 0, &body, "int8").unwrap();
        assert_eq!(&wire[0..8], &6u64.to_le_bytes());
        let head = from_wire(&wire).unwrap();
        assert_eq!(head.len, 6);
        assert_eq!(head.body.len(), 6);
    }

    #[test]
    fn u64_bodylen_roundtrip() {
        for len in [0u64, 1, 0xffff, 0xffff_ffff, 0xffff_ffff_ffff] {
            /* 造够 len 字节的 wire，验证 u64 bodylen 能完整往返（旧 u48 只能到
            2^48-1，这里上限是 u64::MAX）。太大的值跳过实际分配。 */
            let n = usize::try_from(len).unwrap_or(0);
            if n > 1 << 20 {
                /* 只验编码：head 里写出的字节必须与 to_le_bytes 一致。 */
                let mut v = reserve(1, 0, 0, "").unwrap();
                v[OFF_BODYLEN..OFF_BODYLEN + 8].copy_from_slice(&len.to_le_bytes());
                assert_eq!(read_u64(&v, OFF_BODYLEN), Some(len), "len={len:#x}");
                continue;
            }
            let body = vec![0x5Au8; n];
            let v = to_wire(1, 0, &body, "").unwrap();
            assert_eq!(read_u64(&v, OFF_BODYLEN), Some(len), "len={len:#x}");
            let h = from_wire(&v).unwrap();
            assert_eq!(h.len, len, "len={len:#x}");
            assert_eq!(h.body.len(), n, "len={len:#x}");
        }

        /* 声明长度超出实际数据必须拒绝，且不越界读。 */
        let mut short = to_wire(1, 0, b"abc", "").unwrap();
        short[OFF_BODYLEN..OFF_BODYLEN + 8].copy_from_slice(&99u64.to_le_bytes());
        assert!(from_wire(&short).is_none());
    }
}
