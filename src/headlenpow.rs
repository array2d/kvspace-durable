//! XValue wire 的 head 层：16 字节固定头 + body + attr 区。
//!
//! ```text
//! wire: [bodylen:u64][attrslen:u32][layout:u8][padding:3][body][attr records]
//! 总长度 = 16 + bodylen + attrslen
//! ```
//!
//! 长度只从 head 读；attr 区**不带**自带长度前缀（attrslen 已在 head 里，再来一个
//! 前缀就是第二个事实源）。wire 是小端，读写一律走 `from_le_bytes` /
//! `to_le_bytes`，不依赖主机字节序。旧 8 字节 head 不做兼容：遇到即失败。
//!
//! **没有 `storetype`/`ptr` 字段**：存储形态与引用性都从 langtype 推出
//! （`*T` = 指针、`@T` = 扩展、形状段 = 张量、其余按种类）。唯一推不出的是
//! 「bodylen 是容量还是有效长度」，故只保留 `layout` 一个布局标志。

pub const PREFIX: usize = 16;

/// head 字段偏移（唯一定义点；外部一律经 `Head` 访问，不手写偏移）。
const OFF_BODYLEN: usize = 0;
const OFF_ATTRSLEN: usize = 8;
const OFF_LAYOUT: usize = 12;
const OFF_PADDING: usize = 13;

const _: () = assert!(
    PREFIX == 16
        && OFF_BODYLEN + 8 == OFF_ATTRSLEN
        && OFF_ATTRSLEN + 4 == OFF_LAYOUT
        && OFF_LAYOUT + 1 == OFF_PADDING
        && OFF_PADDING + 3 == PREFIX,
    "head field offsets must tile exactly 16 bytes"
);

/// `layout`：bodylen 的语义。这是 head 里唯一的形态标志，其余都从 langtype 推。
pub const LAYOUT_LENGTH: u8 = 0;
pub const LAYOUT_CAPACITY: u8 = 1;

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
    pub layout: u8,
    pub reference: Reference,
    pub langtype: &'a [u8],
    pub len: u64,
    pub attrs: &'a [u8],
    pub body: &'a [u8],
}

/// padding 必须为零：它是 ABI 的一部分，不是「随便填」。
#[inline]
fn padding_is_zero(data: &[u8]) -> bool {
    data.get(OFF_PADDING) == Some(&0)
        && data.get(OFF_PADDING + 1) == Some(&0)
        && data.get(OFF_PADDING + 2) == Some(&0)
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
/// `layout` 越界、`16 + bodylen + attrslen` 溢出或超出输入长度。
pub fn from_wire(data: &[u8]) -> Option<Head<'_>> {
    if data.len() < PREFIX {
        return None;
    }
    if !padding_is_zero(data) {
        return None;
    }
    let layout = *data.get(OFF_LAYOUT)?;
    if layout > LAYOUT_CAPACITY {
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
    let attrs = &data[body_end..attrs_end];
    /*
     * tag 2 的归属校验。
     *
     * 未知 tag 一般**忽略**以保持前向兼容；**tag 2 是例外**：它已经分配给
     * `reallen`，而 `reallen` 只在 `layout = CAPACITY` 时有定义。
     *   - layout = CAPACITY：必须**有** tag 2，且 `reallen <= bodylen`（bodylen = 容量）；
     *   - 其它 layout：**不许**出现 tag 2。
     *
     * 「这个 tag 有定义、但在当前 layout 下无意义」与「这个 tag 我不认识」是两件事：
     * 前者说明写方与读方对格式的理解不一致，必须拒绝；后者才该忽略。
     */
    let (has_reallen, reallen) = scan_reallen(attrs)?;
    if layout == LAYOUT_CAPACITY {
        if !has_reallen || reallen > len {
            return None;
        }
    } else if has_reallen {
        return None;
    }
    let langtype = find_attr(attrs, ATTR_TAG_LANGTYPE).unwrap_or(&[]);
    let reference = match langtype.first() {
        Some(b'*') => Reference::Pointer,
        Some(b'@') => Reference::Extension,
        _ => Reference::None,
    };
    Some(Head {
        layout,
        reference,
        langtype,
        len,
        body: &data[body_start..body_end],
        attrs,
    })
}

/// attr 区（无前缀）：`record = [tag:u8][len:u16][data]`，tag 1 = langtype，tag 2 = reallen。
pub const ATTR_TAG_LANGTYPE: u8 = 1;

/// tag 2：**定容量变长**（`layout = CAPACITY`）的有效长度 `reallen`，data 恒 8 字节 u64 LE。
///
/// 归属是**排他的**：`layout = CAPACITY` 必须带它，其它 layout 必须不带它——同一份
/// 「有效长度」不许有两处表示（`head.bodylen` 在 CAPACITY 下是**容量**）。
pub const ATTR_TAG_REALLEN: u8 = 2;

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

/// 构造 `reallen` 记录（11 字节：`[0x02][0x08 0x00][u64 LE]`）。
pub fn reallen_record(reallen: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(3 + 8);
    v.push(ATTR_TAG_REALLEN);
    v.push(8);
    v.push(0);
    v.extend_from_slice(&reallen.to_le_bytes());
    v
}

/// 扫描 attr 区，返回 (tag2 是否存在, tag2 的 reallen)。缺 tag2 → `Some((false, 0))`。
/// 记录截断/畸形 → `None`（不 panic、不猜）。
fn scan_reallen(attrs: &[u8]) -> Option<(bool, u64)> {
    let mut rest = attrs;
    let mut found: Option<u64> = None;
    while let [tag, l0, l1, tail @ ..] = rest {
        let rlen = usize::from(u16::from_le_bytes([*l0, *l1]));
        let rec = tail.get(..rlen)?;
        if *tag == ATTR_TAG_REALLEN {
            /* 同一 tag 出现两次 = 两个事实源，拒绝。 */
            if found.is_some() {
                return None;
            }
            if rlen != 8 {
                return None;
            }
            let mut raw = [0u8; 8];
            raw.copy_from_slice(rec);
            found = Some(u64::from_le_bytes(raw));
        }
        rest = &tail[rlen..];
    }
    Some((found.is_some(), found.unwrap_or(0)))
}

/// 取 `layout = CAPACITY` 的 `reallen`；其它 layout / 缺 tag 2 / 畸形 → None。
pub fn reallen_from_wire(data: &[u8]) -> Option<u64> {
    let head = from_wire(data)?;
    if head.layout != LAYOUT_CAPACITY {
        return None;
    }
    scan_reallen(head.attrs).and_then(|(has, reallen)| if has { Some(reallen) } else { None })
}

/// 有效长度：`layout = CAPACITY` → `reallen`；其它 → `bodylen`（两者的**唯一读取入口**）。
pub fn effective_len(data: &[u8]) -> Option<u64> {
    let head = from_wire(data)?;
    if head.layout == LAYOUT_CAPACITY {
        reallen_from_wire(data)
    } else {
        Some(head.len)
    }
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
    if head.langtype.is_empty() {
        return None;
    }
    Some(head.langtype)
}

/// 在 attr 区找指定 tag 的 data。缺该 tag → `None`；记录畸形 → `None`（不 panic）。
/// `from_wire` 用它取 langtype 的前缀，`langtype_from_wire` 也经它绕一圈回到同一判据。
fn find_attr(attrs: &[u8], wanted: u8) -> Option<&[u8]> {
    let mut rest = attrs;
    while let [tag, l0, l1, tail @ ..] = rest {
        let rlen = usize::from(u16::from_le_bytes([*l0, *l1]));
        let rec = tail.get(..rlen)?;
        if *tag == wanted {
            return Some(rec);
        }
        rest = &tail[rlen..];
    }
    None
}

pub fn to_wire(body: &[u8], langtype: &str) -> Option<Vec<u8>> {
    let mut data = reserve(body.len(), langtype)?;
    data[PREFIX..PREFIX + body.len()].copy_from_slice(body);
    Some(data)
}

/// 写一个 **`layout = CAPACITY`（定容量变长）** 值：`capacity` 是 head.bodylen，
/// `reallen` 是有效长度（`<= capacity`，越界即错，不截断不扩容）。
/// body 前 `reallen` 字节有效，其余为容量余量（零填）。
pub fn to_wire_varlen(capacity: usize, reallen: u64, langtype: &str) -> Option<Vec<u8>> {
    if reallen > capacity as u64 {
        return None;
    }
    let mut attr = attr_bytes(langtype)?;
    attr.extend_from_slice(&reallen_record(reallen));
    let attrslen = u32::try_from(attr.len()).ok()?;
    let total = PREFIX.checked_add(capacity)?.checked_add(attr.len())?;
    let mut data = Vec::new();
    data.try_reserve_exact(total).ok()?;
    data.resize(total, 0);
    data[OFF_BODYLEN..OFF_BODYLEN + 8].copy_from_slice(&(capacity as u64).to_le_bytes());
    data[OFF_ATTRSLEN..OFF_ATTRSLEN + 4].copy_from_slice(&attrslen.to_le_bytes());
    data[OFF_LAYOUT] = LAYOUT_CAPACITY;
    data[PREFIX + capacity..].copy_from_slice(&attr);
    Some(data)
}

/// 预留：产 bodylen 字节零 body 的固定头 + attr 区（后由调用方填充 body）。
/// padding 显式写成 0——`resize` 已零填，这里再写一次是为了让「必须为零」这件事
/// 在代码里可见，而不是依赖初始化副作用。
pub fn reserve(content: usize, langtype: &str) -> Option<Vec<u8>> {
    let attr = attr_bytes(langtype)?;
    let attrslen = u32::try_from(attr.len()).ok()?;
    let total = PREFIX.checked_add(content)?.checked_add(attr.len())?;
    let mut data = Vec::new();
    data.try_reserve_exact(total).ok()?;
    data.resize(total, 0);
    data[OFF_BODYLEN..OFF_BODYLEN + 8].copy_from_slice(&(content as u64).to_le_bytes());
    data[OFF_ATTRSLEN..OFF_ATTRSLEN + 4].copy_from_slice(&attrslen.to_le_bytes());
    data[OFF_LAYOUT] = LAYOUT_LENGTH;
    data[OFF_PADDING] = 0;
    data[OFF_PADDING + 1] = 0;
    data[OFF_PADDING + 2] = 0;
    data[PREFIX + content..].copy_from_slice(&attr);
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures() {
        /* 新布局：bodylen@0-7, attrslen@8-11, layout@12, padding@13-15 */
        let scalar = to_wire(&42i64.to_le_bytes(), "int64").unwrap();
        assert_eq!(scalar.len(), 32); /* 16 head + 8 body + 8 attr（3 头 + "int64"） */
        assert_eq!(&scalar[0..8], &8u64.to_le_bytes()); /* bodylen @0-7 */
        assert_eq!(&scalar[8..12], &8u32.to_le_bytes()); /* attrslen @8-11 */
        assert_eq!(scalar[12], LAYOUT_LENGTH); /* layout @12 */
        assert_eq!(&scalar[13..16], &[0, 0, 0]); /* padding @13-15 */
        let h = from_wire(&scalar).unwrap();
        assert_eq!(h.len, 8);
        assert_eq!(h.layout, LAYOUT_LENGTH);
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
        let mut v = to_wire(b"x", "").unwrap();
        v[12] = 2; /* layout @12 越界（只允许 0/1） */
        assert!(from_wire(&v).is_none());
        let mut v3 = to_wire(b"xyz", "").unwrap();
        v3[0] = 0xff; /* bodylen 越界 */
        assert!(from_wire(&v3).is_none());
        let mut v4 = to_wire(b"xyz", "").unwrap();
        v4[8] = 0xff; /* attrslen 越界 */
        assert!(from_wire(&v4).is_none());

        /* padding 三个字节都必须为零 */
        for at in [13usize, 14, 15] {
            let mut p = to_wire(b"x", "").unwrap();
            p[at] = 1;
            assert!(from_wire(&p).is_none(), "padding byte {at} must be zero");
        }
    }

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
        let mut v = reserve(3, "char/utf8").unwrap();
        v[PREFIX..PREFIX + 3].copy_from_slice(b"\xc3\xa9a");
        assert_eq!(from_wire(&v).unwrap().len, 3);
        assert_eq!(from_wire(&v).unwrap().body, b"\xc3\xa9a");
        assert_eq!(langtype_from_wire(&v).unwrap(), b"char/utf8");
    }

    /// layout=CAPACITY 的最小闭环：写 → 解析 → reallen 正确；三种拒绝路径各一条。
    #[test]
    fn varlen_capacity_roundtrip_and_fail_closed_paths() {
        /* ① 闭环：容量 32、有效 5。body 前 5 字节是数据，其余是容量余量。 */
        let mut wire = to_wire_varlen(32, 5, "char/utf8").unwrap();
        wire[PREFIX..PREFIX + 5].copy_from_slice(b"hello");
        let head = from_wire(&wire).unwrap();
        assert_eq!(head.layout, LAYOUT_CAPACITY);
        assert_eq!(head.len, 32, "head.bodylen 是**容量**");
        assert_eq!(head.body.len(), 32);
        assert_eq!(reallen_from_wire(&wire), Some(5));
        assert_eq!(effective_len(&wire), Some(5), "有效长度取 reallen，不取容量");
        assert_eq!(langtype_from_wire(&wire).unwrap(), b"char/utf8");
        /* 有效区之外的容量余量不被当成数据。 */
        assert_eq!(&head.body[..5], b"hello");
        assert!(head.body[5..].iter().all(|b| *b == 0));

        /* reallen == 0 合法：容量已分配、当前无内容。 */
        let empty = to_wire_varlen(16, 0, "char/utf8").unwrap();
        assert_eq!(reallen_from_wire(&empty), Some(0));
        assert_eq!(effective_len(&empty), Some(0));

        /* ② 拒绝：storetype 4 缺 tag 2（用 reserve 造一个没有 tag 2 的 4 号 wire）。 */
        let mut missing = reserve(4, "").unwrap();
        missing[OFF_LAYOUT] = LAYOUT_CAPACITY; /* 改成 CAPACITY，但 attr 里没有 tag 2 */
        assert!(from_wire(&missing).is_none(), "CAPACITY 缺 tag 2 必须拒绝");

        /* ③ 拒绝：reallen > 容量（把 tag 2 的 8 字节改大）。 */
        let mut over = to_wire_varlen(8, 8, "").unwrap();
        let attrs_start = PREFIX + 8;
        /* attr = [0x02][0x08 0x00][u64]（无 langtype） */
        over[attrs_start + 3..attrs_start + 11].copy_from_slice(&9u64.to_le_bytes());
        assert!(from_wire(&over).is_none(), "reallen > bodylen 必须拒绝");

        /* ④ 拒绝：非 4 带 tag 2。 */
        let mut wrong = to_wire(b"x", "").unwrap();
        let attrs_at = PREFIX + 1;
        let record = reallen_record(1);
        wrong.extend_from_slice(&record);
        let attrslen = u32::try_from(record.len()).unwrap();
        wrong[OFF_ATTRSLEN..OFF_ATTRSLEN + 4].copy_from_slice(&attrslen.to_le_bytes());
        assert!(from_wire(&wrong).is_none(), "非 CAPACITY 带 tag 2 必须拒绝");
        let _ = attrs_at;

        /* ⑤ 拒绝：同一 tag 2 出现两次（两个事实源）。 */
        let mut dup = to_wire_varlen(8, 3, "").unwrap();
        let attrs_start = PREFIX + 8;
        let rec = reallen_record(3);
        dup.extend_from_slice(&rec);
        let old_attrslen = read_u32(&dup, OFF_ATTRSLEN).unwrap();
        let new_attrslen = old_attrslen + u32::try_from(rec.len()).unwrap();
        dup[OFF_ATTRSLEN..OFF_ATTRSLEN + 4].copy_from_slice(&new_attrslen.to_le_bytes());
        let _ = attrs_start;
        assert!(from_wire(&dup).is_none(), "重复 tag 2 必须拒绝");

        /* ⑥ 拒绝：写侧 reallen > capacity。 */
        assert!(to_wire_varlen(4, 5, "").is_none());
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
                let mut v = reserve(0, "").unwrap();
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
