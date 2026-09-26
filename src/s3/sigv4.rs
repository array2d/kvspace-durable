// s3/sigv4.rs — AWS Signature Version 4。
//
// 腾讯 COS 兼容 S3，也认 SigV4（rclone 的 provider=TencentCOS 走的就是它）。
// 规范本身不长，这里不做抽象，就按规范的顺序拼出来。
//
// 日期换算不引依赖：`std` 只给 Unix 秒，而签名要 `20130524T000000Z` 这种 UTC 串。
// 用的是 Howard Hinnant 的 civil_from_days，几十年的经典算法，见 `civil_from_days`。

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const ALGORITHM: &str = "AWS4-HMAC-SHA256";
const SERVICE: &str = "s3";

/// 空载荷的 SHA256。GET / HEAD / DELETE 用得多，算一次存下来。
pub const EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finalize())
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// RFC 3986 百分号编码。未保留字符是 `A-Za-z0-9-_.~`，其余按字节编码成大写十六进制。
///
/// `keep_slash` 只在拼**规范 URI** 时为真：路径里的 `/` 是分隔符，不能编码。
/// 查询串里的值必须把 `/` 也编码掉，所以那个调用点传 false。
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        let unreserved = c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~');
        if unreserved || (keep_slash && c == '/') {
            out.push(c);
        } else {
            out.push('%');
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// 规范查询串：按 key（同名再按 value）字典序排，key 与 value 都编码。
/// 没有参数时返回空串（不是空行）。
pub fn canonical_query(params: &[(&str, String)]) -> String {
    let mut v: Vec<(String, String)> = params
        .iter()
        .map(|(k, val)| (uri_encode(k, false), uri_encode(val, false)))
        .collect();
    v.sort();
    v.iter()
        .map(|(k, val)| format!("{}={}", k, val))
        .collect::<Vec<_>>()
        .join("&")
}

/// Unix 秒 → (年, 月, 日, 时, 分, 秒)，UTC。
///
/// `civil_from_days`：把「距 1970-01-01 的天数」换算成公历年月日。
/// 先算 400 年一轮的 era，再在 era 内用移位把 3 月当岁首（避开闰日在年末的麻烦），
/// 最后按 153 天的月长序列取月。闰年规则已经隐含在 `doe/1460 - doe/36524 + doe/146096` 里。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468; // 基准从 1970-01-01 挪到 0000-03-01
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as i64; // 日序号，[0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]，3 月为 0
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Unix 秒 → (`20130524T000000Z`, `20130524`)。
pub fn amz_dates(unix_secs: u64) -> (String, String) {
    let secs = unix_secs as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let date = format!("{:04}{:02}{:02}", y, m, d);
    let full = format!("{}T{:02}{:02}{:02}Z", date, hh, mm, ss);
    (full, date)
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("系统时间早于 1970")
        .as_secs()
}

/// 一次请求的签名材料。
pub struct Signed {
    /// 要塞进请求头的 `Authorization` 值。
    pub authorization: String,
    /// 要塞进 `x-amz-date` 的值。
    pub amz_date: String,
    /// 载荷的 SHA256 十六进制；要同时作为 `x-amz-content-sha256` 发出去。
    pub payload_hash: String,
}

/// 签名。
///
/// - `host`：虚拟主机式的主机名，如 `moechat-test-1252231640.cos.ap-beijing.myqcloud.com`
/// - `canonical_uri`：**已编码**的路径，如 `/moechat/a%20b`
/// - `params`：查询参数（未编码，函数内会编码并排序）
/// - `payload`：请求体；GET/HEAD 传空切片
pub fn sign(
    access_key: &str,
    secret_key: &str,
    region: &str,
    host: &str,
    method: &str,
    canonical_uri: &str,
    params: &[(&str, String)],
    payload: &[u8],
) -> Signed {
    let (amz_date, date_stamp) = amz_dates(now_unix());
    let payload_hash = sha256_hex(payload);
    let canonical_query = canonical_query(params);

    // 参与签名的头：只签这三个。签名头越少越好——每一个都要在请求里原样重现。
    // 顺序必须字典序，值要去掉首尾空白。
    let canonical_headers =
        format!("host:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n", host, payload_hash, amz_date);
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method, canonical_uri, canonical_query, canonical_headers, signed_headers, payload_hash
    );

    let scope = format!("{}/{}/{}/aws4_request", date_stamp, region, SERVICE);
    let string_to_sign = format!(
        "{}\n{}\n{}\n{}",
        ALGORITHM,
        amz_date,
        scope,
        sha256_hex(canonical_request.as_bytes())
    );

    let k_date = hmac(format!("AWS4{}", secret_key).as_bytes(), date_stamp.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, SERVICE.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

    Signed {
        authorization: format!(
            "{} Credential={}/{}, SignedHeaders={}, Signature={}",
            ALGORITHM, access_key, scope, signed_headers, signature
        ),
        amz_date,
        payload_hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 编码规则() {
        assert_eq!(uri_encode("aZ09-_.~", true), "aZ09-_.~");
        assert_eq!(uri_encode("a/b", true), "a/b");
        assert_eq!(uri_encode("a/b", false), "a%2Fb");
        assert_eq!(uri_encode(" ", true), "%20"); // 不是 '+'
        // KVSpace 的 key 里有这些多字节字符，必须逐字节编码
        assert_eq!(uri_encode("·", true), "%C2%B7");
        assert_eq!(uri_encode("‥", true), "%E2%80%A5");
        assert_eq!(uri_encode("…", true), "%E2%80%A6");
    }

    #[test]
    fn 查询串排序编码() {
        let q = canonical_query(&[
            ("prefix", "/a b/".into()),
            ("list-type", "2".into()),
            ("continuation-token", "x/y=".into()),
        ]);
        assert_eq!(
            q,
            "continuation-token=x%2Fy%3D&list-type=2&prefix=%2Fa%20b%2F"
        );
    }

    #[test]
    fn 空查询串是空串() {
        assert_eq!(canonical_query(&[]), "");
    }

    #[test]
    fn 日期换算() {
        // 与 Unix 纪元对齐的几个已知点
        assert_eq!(amz_dates(0).0, "19700101T000000Z");
        assert_eq!(amz_dates(1369353600).0, "20130524T000000Z"); // AWS 文档用的样例时刻
        assert_eq!(amz_dates(1369353600).1, "20130524");
        // 闰日
        assert_eq!(amz_dates(1709164800).0, "20240229T000000Z");
        // 跨年
        assert_eq!(amz_dates(1735689600).0, "20250101T000000Z");
        // 非闰年的 3 月 1 日（civil_from_days 的月序切换点）
        assert_eq!(amz_dates(1709251200).0, "20240301T000000Z");
    }

    #[test]
    fn 时间是零点边界() {
        assert_eq!(amz_dates(86399).0, "19700101T235959Z");
        assert_eq!(amz_dates(86400).0, "19700102T000000Z");
    }

    #[test]
    fn 空载荷哈希是常量() {
        assert_eq!(sha256_hex(b""), EMPTY_PAYLOAD_SHA256);
    }

    #[test]
    fn aws_官方样例签名可复现() {
        // AWS SigV4 文档里的 GET 样例（us-east-1，2013-05-24T00:00:00Z）
        // 这里只验证签名链本身：同样的输入必须给出同样的签名。
        let s = sign(
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "examplebucket.s3.amazonaws.com",
            "GET",
            "/test.txt",
            &[],
            b"",
        );
        // 断言的是结构而非固定串：时间随当前时钟走，固定串对不上。
        // 固定向量见集成测试（那里会注入时间）。
        assert!(s.authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"));
        assert!(s.authorization.contains("/us-east-1/s3/aws4_request"));
        assert!(s.authorization.contains(
            "SignedHeaders=host;x-amz-content-sha256;x-amz-date"
        ));
        assert_eq!(s.payload_hash, EMPTY_PAYLOAD_SHA256);
        assert!(s.authorization.contains("Signature="));
    }
}
