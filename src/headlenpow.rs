//! XValue wire 的 head 层：16 字节固定头 + body + attr 区。
//!
//! ```text
//! wire: [bodylen:u64][attrslen:u16][padding:6]
//!       [body][attr records]
//! 总长度 = 16 + bodylen + attrslen
//! ```
//!
//! 长度只从 head 读；attr 区**不带**自带长度前缀（attrslen 已在 head 里，再来一个
//! 前缀就是第二个事实源）。wire 是小端，读写一律走 `from_le_bytes` /
//! `to_le_bytes`，不依赖主机字节序。旧 8 字节 head 不做兼容：遇到即失败。
//!
//! **head 不承载任何语义**：存储形态与引用性都从 langtype 推出
//! （`*T` = 指针、`@T` = 扩展、形状段 = 张量）。`bodylen` **永远是有效字节数**；
//! 容量是物理分配属性，不进 wire。

pub const PREFIX: usize = 16;

/// head 字段偏移（唯一定义点；外部一律经 `Head` 访问，不手写偏移）。
const OFF_BODYLEN: usize = 0;
const OFF_ATTRSLEN: usize = 8;
const OFF_PADDING: usize = 10;

const _: () = assert!(
    PREFIX == 16
        && OFF_BODYLEN + 8 == OFF_ATTRSLEN
        && OFF_ATTRSLEN + 2 == OFF_PADDING
        && OFF_PADDING + 6 == PREFIX,
    "head field offsets must tile exactly 16 bytes"
);

/// 引用性：**从 langtype 前缀解出**，不是 head 字段。
/// `from_wire` 唯一负责产出它；任何构造器或写入路径都不得单独设置——
/// 写回时 langtype 文本本身仍是唯一来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference {
    None,
    Pointer,
    Extension,
}

/// 解析出的 head。`body`/`attrs`/`langtype` 借自输入。
#[derive(Debug, PartialEq)]
pub struct Head<'a> {
    pub reference: Reference,
    pub langtype: &'a [u8],
    pub len: u64,
    pub attrs: &'a [u8],
    pub body: &'a [u8],
}

/// padding 必须为零：它是 ABI 的一部分，不是「随便填」。
#[inline]
fn padding_is_zero(data: &[u8]) -> bool {
    data.get(OFF_PADDING..PREFIX)
        .map(|s| s.iter().all(|b| *b == 0))
        == Some(true)
}

fn read_u16(data: &[u8], at: usize) -> Option<u16> {
    let b = data.get(at..at + 2)?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

fn read_u64(data: &[u8], at: usize) -> Option<u64> {
    let b = data.get(at..at + 8)?;
    Some(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

/// 解析 wire。任何一项不成立都返回 `None`：head 不足 16 字节、padding 非零、
/// `16 + bodylen + attrslen` 溢出或超出输入长度、attr 区畸形。
pub fn from_wire(data: &[u8]) -> Option<Head<'_>> {
    if data.len() < PREFIX || !padding_is_zero(data) {
        return None;
    }
    let len = read_u64(data, OFF_BODYLEN)?;
    let attrslen = read_u16(data, OFF_ATTRSLEN)?;
    let body_start = PREFIX;
    let body_end = body_start.checked_add(usize::try_from(len).ok()?)?;
    let attrs_end = body_end.checked_add(usize::from(attrslen))?;
    if attrs_end > data.len() {
        return None;
    }
    let attrs = &data[body_end..attrs_end];
    /* 记录必须整体合法（截断/畸形即拒绝）；tag 2 已删除，没有第二个长度量可校验。 */
    let langtype = scan_attrs(attrs)?.unwrap_or(&[]);
    let reference = match langtype.first() {
        Some(b'*') => Reference::Pointer,
        Some(b'@') => Reference::Extension,
        _ => Reference::None,
    };
    Some(Head {
        reference,
        langtype,
        len,
        body: &data[body_start..body_end],
        attrs,
    })
}

/// attr 区（无前缀）：`record = [tag:u8][len:u16][data]`，tag 1 = langtype，tag 2+ 预留。
pub const ATTR_TAG_LANGTYPE: u8 = 1;

/// 已删除的 `reallen` tag：写方还在用时必须拒绝（第二事实源）。
const ATTR_TAG_REALLEN_DELETED: u8 = 2;

/// 构造 attr 区字节（**不含**任何长度前缀）；langtype 空 → 空 attr（零字节）。
pub fn attr_bytes(langtype: &str) -> Option<Vec<u8>> {
    if langtype.is_empty() {
        return Some(Vec::new());
    }
    let l = langtype.len();
    if l > usize::from(u16::MAX) - 3 {
        return None;
    }
    let mut v = Vec::with_capacity(3 + l);
    v.push(ATTR_TAG_LANGTYPE);
    v.push((l & 0xff) as u8);
    v.push((l >> 8) as u8);
    v.extend_from_slice(langtype.as_bytes());
    Some(v)
}

/// 扫描 attr 区：逐条走完确认不截断，返回 tag=1 的 langtype 记录。
/// 外层 `None` = 记录畸形（拒绝）；内层 `None` = 无 langtype 记录（合法）。
///
/// tag 2 **已删除**：出现即拒绝——`bodylen` 恒为有效长度，再带一条长度记录
/// 就是第二个事实源，说明写方还在按旧契约写。
fn scan_attrs(attrs: &[u8]) -> Option<Option<&[u8]>> {
    let mut rest = attrs;
    let mut langtype: Option<&[u8]> = None;
    while let [tag, l0, l1, tail @ ..] = rest {
        let rlen = usize::from(u16::from_le_bytes([*l0, *l1]));
        let rec = tail.get(..rlen)?;
        if *tag == ATTR_TAG_REALLEN_DELETED {
            return None;
        }
        if *tag == ATTR_TAG_LANGTYPE && langtype.is_none() {
            langtype = Some(rec);
        }
        rest = &tail[rlen..];
    }
    Some(langtype)
}

/// 有效长度 = `bodylen`（容量是物理分配属性，不在 wire 里）。
pub fn effective_len(data: &[u8]) -> Option<u64> {
    Some(from_wire(data)?.len)
}

/// attr 区总字节数（u16，从 head 读，不含任何前缀；空 attr = 0）。
/// padding 非零或 wire 过短 → None。
#[inline]
pub fn attr_len(data: &[u8]) -> Option<u16> {
    if data.len() < PREFIX || !padding_is_zero(data) {
        return None;
    }
    read_u16(data, OFF_ATTRSLEN)
}

/// 从 attr 区取 tag=1（langtype）记录 data；无/坏 attr → None。
/// 只扫 head 声明的 attr 范围内实际存在的字节（与 C `kvspaceXvalueHeadLangtype`
/// 对齐），截断或畸形记录返回 `None` 而不 panic。
pub fn langtype_from_wire(data: &[u8]) -> Option<&[u8]> {
    let head = from_wire(data)?;
    if head.langtype.is_empty() {
        return None;
    }
    Some(head.langtype)
}


pub fn to_wire(body: &[u8], langtype: &str) -> Option<Vec<u8>> {
    let mut data = to_wire_zeroed(body.len(), langtype)?;
    data[PREFIX..PREFIX + body.len()].copy_from_slice(body);
    Some(data)
}

/// `content` 字节零 body + 固定头 + attr：bodylen 就是有效长度，容量不进 wire
/// （后由调用方填充前 `body.len()` 字节）。
pub fn to_wire_zeroed(content: usize, langtype: &str) -> Option<Vec<u8>> {
    build_wire(content, attr_bytes(langtype)?)
}

/// head + 零填 body + attr 区的唯一构造点（padding 由 `resize` 的零填保证）。
fn build_wire(content: usize, attr: Vec<u8>) -> Option<Vec<u8>> {
    let attrslen = u16::try_from(attr.len()).ok()?;
    let total = PREFIX.checked_add(content)?.checked_add(attr.len())?;
    let mut data = Vec::new();
    data.try_reserve_exact(total).ok()?;
    data.resize(total, 0);
    data[OFF_BODYLEN..OFF_BODYLEN + 8].copy_from_slice(&(content as u64).to_le_bytes());
    data[OFF_ATTRSLEN..OFF_ATTRSLEN + 2].copy_from_slice(&attrslen.to_le_bytes());
    data[PREFIX + content..].copy_from_slice(&attr);
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures() {
        /* 新布局：bodylen@0-7, attrslen@8-9, padding@10-15 */
        let scalar = to_wire(&42i64.to_le_bytes(), "int64").unwrap();
        assert_eq!(scalar.len(), 32); /* 16 head + 8 body + 8 attr（3 头 + "int64"） */
        assert_eq!(&scalar[0..8], &8u64.to_le_bytes()); /* bodylen @0-7 */
        assert_eq!(&scalar[8..10], &8u16.to_le_bytes()); /* attrslen @8-9 */
        assert_eq!(&scalar[10..16], &[0; 6]); /* padding @10-15 */
        let h = from_wire(&scalar).unwrap();
        assert_eq!(h.len, 8);
        assert_eq!(h.reference, Reference::None);
        assert_eq!(h.body, &42i64.to_le_bytes());
        assert_eq!(langtype_from_wire(&scalar).unwrap(), b"int64");

        /* 指针：head 没有 ptr 字段，指针性来自 langtype 的 `*` 前缀。 */
        let ptr = to_wire(b"/varx", "*int64").unwrap();
        assert_eq!(from_wire(&ptr).unwrap().reference, Reference::Pointer);
        assert_eq!(from_wire(&ptr).unwrap().body, b"/varx");
        assert_eq!(langtype_from_wire(&ptr).unwrap(), b"*int64");

        /* 扩展：`@` 前缀。 */
        let ext = to_wire(b"s3://", "@").unwrap();
        assert_eq!(from_wire(&ext).unwrap().reference, Reference::Extension);
        assert_eq!(from_wire(&ext).unwrap().len, 5);
        assert_eq!(attr_len(&ext).unwrap(), 4); /* record 头 3 字节 + "@" */

        let text = to_wire("你😀".as_bytes(), "char/utf8").unwrap();
        assert_eq!(from_wire(&text).unwrap().len, 7); /* 你(3B)+😀(4B) */
        assert_eq!(langtype_from_wire(&text).unwrap(), b"char/utf8");
    }

    #[test]
    fn malformed() {
        let mut v3 = to_wire(b"xyz", "").unwrap();
        v3[0] = 0xff; /* bodylen 越界 */
        assert!(from_wire(&v3).is_none());
        let mut v4 = to_wire(b"xyz", "").unwrap();
        v4[8] = 0xff; /* attrslen 越界 */
        assert!(from_wire(&v4).is_none());

        /* padding 六个字节都必须为零 */
        for at in 10usize..16 {
            let mut p = to_wire(b"x", "").unwrap();
            p[at] = 1;
            assert!(from_wire(&p).is_none(), "padding byte {at} must be zero");
            assert!(attr_len(&p).is_none(), "attr_len rejects padding byte {at}");
        }

        /* attrslen 是 u16：3 字节记录头 + langtype 数据不得超过 UINT16_MAX。 */
        let fits = "a".repeat(usize::from(u16::MAX) - 3);
        assert!(
            attr_bytes(&fits).is_some(),
            "attr region exactly UINT16_MAX fits"
        );
        let over = "a".repeat(usize::from(u16::MAX) - 2);
        assert!(
            attr_bytes(&over).is_none(),
            "attr region > UINT16_MAX rejected"
        );
    }

    #[test]
    fn old_eight_byte_head_is_not_accepted_as_a_legal_wire() {
        /* 情形 A：旧 wire 的 body 使新视角下 attrslen 巨大 → 总长越界 → 拒绝。 */
        let mut overrun = vec![0u8; 8 + 8 + 10];
        overrun[0] = 8; /* 旧 len=8 */
        overrun[8] = 0xff; /* 新视角 attrslen=255，仍超出 26 字节输入 */
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
    fn zeroed_body() {
        let mut v = to_wire_zeroed(3, "char/utf8").unwrap();
        assert_eq!(&v[OFF_PADDING..PREFIX], &[0; 6], "padding 必须为零");
        v[PREFIX..PREFIX + 3].copy_from_slice(b"\xc3\xa9a");
        assert_eq!(from_wire(&v).unwrap().len, 3);
        assert_eq!(from_wire(&v).unwrap().body, b"\xc3\xa9a");
        assert_eq!(langtype_from_wire(&v).unwrap(), b"char/utf8");
    }

    /// 零填 body 的最小闭环 + tag 2 已删除的三条判据。
    #[test]
    fn zeroed_body_and_deleted_tag_two_paths() {
        /* ① 闭环：4 字节零 body、langtype "[]char/utf8"；bodylen 就是有效长度。 */
        let mut wire = to_wire_zeroed(4, "[]char/utf8").unwrap();
        wire[PREFIX..PREFIX + 4].copy_from_slice(b"helo");
        let head = from_wire(&wire).unwrap();
        assert_eq!(head.len, 4, "bodylen 是有效长度");
        assert_eq!(head.body, b"helo");
        assert_eq!(effective_len(&wire), Some(4), "有效长度恒等于 bodylen");
        assert_eq!(langtype_from_wire(&wire).unwrap(), b"[]char/utf8");

        /* bodylen == 0 合法：空 body。 */
        let empty = to_wire_zeroed(0, "[]char/utf8").unwrap();
        assert_eq!(effective_len(&empty), Some(0));

        /* ② 拒绝：attr 里出现 tag 2（旧 reallen 契约，第二事实源）。 */
        let mut stale = to_wire_zeroed(4, "").unwrap();
        stale.extend_from_slice(&[2, 8, 0, 4, 0, 0, 0, 0, 0, 0, 0]);
        stale[OFF_ATTRSLEN..OFF_ATTRSLEN + 2].copy_from_slice(&11u16.to_le_bytes());
        assert!(from_wire(&stale).is_none(), "tag 2 已删除，出现即拒绝");

        /* ③ 拒绝：attr 记录截断（声明长度超出 attr 区）。 */
        let mut cut = to_wire_zeroed(4, "[]char/utf8").unwrap();
        cut[OFF_ATTRSLEN..OFF_ATTRSLEN + 2].copy_from_slice(&7u16.to_le_bytes());
        assert!(from_wire(&cut).is_none(), "attr 记录截断必须拒绝");

        /* ④ 拒绝：未知 tag 记录本身合法时不影响解析（前向兼容），
           但纳入 attrslen 后总长必须自洽——这里只验合法未知 tag 被跳过。 */
        let mut unknown = to_wire_zeroed(0, "").unwrap();
        unknown.extend_from_slice(&[9, 1, 0, 0xee]);
        unknown[OFF_ATTRSLEN..OFF_ATTRSLEN + 2].copy_from_slice(&4u16.to_le_bytes());
        assert!(from_wire(&unknown).is_some(), "未知 tag 忽略，不影响解析");
    }

    #[test]
    fn byte_order_is_little_endian() {
        /* 用非对称值盯住字节序：读出反了就不可能相等。 */
        let body = vec![0xAAu8; 6];
        let wire = to_wire(&body, "int8").unwrap();
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
                let mut v = to_wire_zeroed(0, "").unwrap();
                v[OFF_BODYLEN..OFF_BODYLEN + 8].copy_from_slice(&len.to_le_bytes());
                assert_eq!(read_u64(&v, OFF_BODYLEN), Some(len), "len={len:#x}");
                continue;
            }
            let body = vec![0x5Au8; n];
            let v = to_wire(&body, "").unwrap();
            assert_eq!(read_u64(&v, OFF_BODYLEN), Some(len), "len={len:#x}");
            let h = from_wire(&v).unwrap();
            assert_eq!(h.len, len, "len={len:#x}");
            assert_eq!(h.body.len(), n, "len={len:#x}");
        }

        /* 声明长度超出实际数据必须拒绝，且不越界读。 */
        let mut short = to_wire(b"abc", "").unwrap();
        short[OFF_BODYLEN..OFF_BODYLEN + 8].copy_from_slice(&99u64.to_le_bytes());
        assert!(from_wire(&short).is_none());
    }
}
