// xvalue_duration.rs — 对齐 xvalue_duration.go

use crate::xvalue::{tlv_encode, XValue};

pub fn new_duration(v: &[i64]) -> XValue {
    XValue::Duration(v.to_vec())
}

pub fn decode_duration(body: &[u8]) -> Vec<i64> {
    body.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect()
}

pub fn encode_duration(data: &[i64]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| (v as u64).to_le_bytes()).collect();
    tlv_encode("duration", &raw, data.len() as i32)
}

/// duration_string 精确移植 Go time.Duration.String()：
/// 0 → "0s"；<1s 用 n/µ/m 单位；≥1s 用 h/m/s；负数前缀 "-"。
pub fn duration_string(d: i64) -> String {
    const SECOND: u64 = 1_000_000_000;
    const MILLISECOND: u64 = 1_000_000;
    const MICROSECOND: u64 = 1_000;

    let neg = d < 0;
    let u = if neg { (-(d as i128)) as u64 } else { d as u64 };

    if u < SECOND {
        if u == 0 {
            return "0s".to_string();
        }
        let (prec, unit): (u32, &str) = if u < MICROSECOND {
            (0, "n")
        } else if u < MILLISECOND {
            (3, "\u{00B5}")
        } else {
            (6, "m")
        };
        let (frac, int_part) = fmt_frac(u, prec);
        let mut s = int_part.to_string();
        s.push_str(&frac);
        s.push_str(unit);
        s.push('s');
        if neg {
            s.insert(0, '-');
        }
        s
    } else {
        let (frac, secs) = fmt_frac(u, 9);
        let mut secs = secs;
        let ss = secs % 60;
        secs /= 60;
        let mut body = format!("{}{}s", ss, frac);
        if secs > 0 {
            let mm = secs % 60;
            secs /= 60;
            body = format!("{}m{}", mm, body);
            if secs > 0 {
                body = format!("{}h{}", secs, body);
            }
        }
        if neg {
            body.insert(0, '-');
        }
        body
    }
}

/// fmt_frac 对齐 Go fmtFrac：返回 (fractional_part, integer_part)。
/// fractional_part 形如 ".123"（末尾 0 省略，无小数则 ""）。
fn fmt_frac(v: u64, prec: u32) -> (String, u64) {
    let pow = 10u64.pow(prec);
    let int_part = v / pow;
    let mut rem = v % pow;
    let mut digits = String::new();
    for _ in 0..prec {
        let digit = rem % 10;
        rem /= 10;
        if digit != 0 || !digits.is_empty() {
            digits.push((b'0' + digit as u8) as char);
        }
    }
    if digits.is_empty() {
        (String::new(), int_part)
    } else {
        let frac: String = digits.chars().rev().collect();
        (format!(".{}", frac), int_part)
    }
}
