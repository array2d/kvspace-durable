// xvalue_time.rs — 对齐 xvalue_time.go

use crate::xvalue::{tlv_encode, XValue};

pub fn new_time(v: &[i64]) -> XValue {
    XValue::Time(v.to_vec())
}

pub fn decode_time(body: &[u8]) -> Vec<i64> {
    body.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect()
}

pub fn encode_time(data: &[i64]) -> Vec<u8> {
    let raw: Vec<u8> = data.iter().flat_map(|&v| (v as u64).to_le_bytes()).collect();
    tlv_encode("time", &raw, data.len() as i32)
}

/// formatTime 用 "2006/01/02 15:04:05"（YYYY/MM/DD HH:MM:SS）格式化 time 值。
/// Go: time.Unix(0, ns).Format("2006/01/02 15:04:05")，本地时区。
/// 此处按 UTC 换算（divergence：忽略本地时区偏移），仅影响显示、不影响 TLV 字节。
pub fn format_time(ns: i64) -> String {
    let total_sec = ns.div_euclid(1_000_000_000);
    let days = total_sec.div_euclid(86_400);
    let sod = total_sec.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let h = sod / 3600;
    let mi = (sod % 3600) / 60;
    let s = sod % 60;
    format!("{:04}/{:02}/{:02} {:02}:{:02}:{:02}", y, m, d, h, mi, s)
}

/// civil_from_days：days since 1970-01-01 → (year, month, day)。Howard Hinnant 算法。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
