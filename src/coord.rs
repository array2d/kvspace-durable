// coord.rs — strkeymapindex 坐标段 [s0,s1,...] 的构造、解析、校验与排序。
// 坐标段是父目录 `m.` 下的一个成员名，不是多级路径。详见 docs/strkeymapindex-ndarray.md。

use std::cmp::Ordering;

/// 构造坐标段：[s0,s1,...]，十进制、无空格。
pub fn format_coord(coords: &[i64]) -> String {
    let mut s = String::from("[");
    for (i, c) in coords.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&c.to_string());
    }
    s.push(']');
    s
}

/// 解析整数坐标段，严格匹配 \[[0-9]+(,[0-9]+)*\]，非整数返回 None。
pub fn parse_coord(name: &str) -> Option<Vec<i64>> {
    let inner = name.strip_prefix('[')?.strip_suffix(']')?;
    if inner.is_empty() {
        return None;
    }
    inner
        .split(',')
        .map(|p| {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                None
            } else {
                p.parse().ok()
            }
        })
        .collect()
}

/// 坐标段 = 任意非空 `[..]`（内容可含 `.`/`,`/字符串，如 [12.24,234.34]、[my,name]），
/// 仅禁止嵌套 `[` `]`。这是「成员名是坐标段」的结构判定，不要求整数。
pub fn is_coord(name: &str) -> bool {
    name.len() >= 3
        && name.starts_with('[')
        && name.ends_with(']')
        && !name[1..name.len() - 1].contains(['[', ']'])
}

/// row-major 排序：坐标段恒排在非坐标段之前；两个坐标段若都能按整数解析则数值升序，
/// 否则字典序（覆盖小数/字符串坐标）。
pub fn cmp_coord(a: &str, b: &str) -> Ordering {
    match (is_coord(a), is_coord(b)) {
        (false, false) => a.cmp(b),
        (false, true) => Ordering::Greater,
        (true, false) => Ordering::Less,
        (true, true) => match (parse_coord(a), parse_coord(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            _ => a.cmp(b),
        },
    }
}

/// 坐标是否落在 dims 内（维数相符且逐维小于）。
pub fn coord_in_dims(coords: &[i64], dims: &[i32]) -> bool {
    coords.len() == dims.len() && coords.iter().zip(dims).all(|(&c, &d)| c >= 0 && c < d as i64)
}

/// 一维 dims 至少容纳坐标 v（max(dims[0], v+1)），dims 为空时起算为 1。
pub fn grow_dim(dims: &[i32], v: i64) -> Vec<i32> {
    if dims.is_empty() {
        vec![v as i32 + 1]
    } else {
        vec![dims[0].max(v as i32 + 1)]
    }
}

/// 一组坐标段的维数：逐段取最大整数坐标，dims 为空时按首个整数坐标段长度起算；
/// 无任何整数坐标（纯小数/字符串坐标）时退化为 1 维、长度为成员数。
pub fn grow_coord_dims(dims: &[i32], names: &[String]) -> Vec<i32> {
    let mut d = dims.to_vec();
    for n in names {
        if let Some(co) = parse_coord(n) {
            if d.is_empty() {
                d = vec![0; co.len()];
            }
            if co.len() == d.len() {
                for (i, &v) in co.iter().enumerate() {
                    d[i] = d[i].max(v as i32 + 1);
                }
            }
        }
    }
    if d.is_empty() {
        d = vec![names.len() as i32];
    }
    d
}

/// objindex 成员名字符约束：禁 / . [ ] \n \r \0 ‥ … 与 ASCII 控制字符，禁空串。
pub fn valid_member_name(name: &str) -> bool {
    !name.is_empty()
        && !name.chars().any(|c| {
            matches!(
                c,
                '/' | '.' | '[' | ']' | '\n' | '\r' | '\0' | '\u{2025}' | '\u{2026}'
            ) || (c as u32) < 0x20
        })
}
