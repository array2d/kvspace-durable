// s3/dsn.rs — s3:// 的 DSN 解析与脱敏。
//
// 形态：
//   s3://<AccessKeyId>:<AccessKeySecret>@<bucket>/<prefix>?endpoint=<host>&region=<region>
//
// 凭据放在 URI 的 userinfo 位置，与 `redis://:pass@host`、`https://user:pass@host`
// 是同一套写法。代价是 **DSN 从此不能原样打印** —— 见 `redact`。

/// 解析后的 s3 DSN。字段全部必填，没有默认值。
#[derive(Debug, Clone)]
pub struct S3Dsn {
    pub access_key: String,
    pub secret_key: String,
    pub bucket: String,
    /// 对象名前缀。**必填且强制以 `/` 结尾**——它同时是 `flush` 的安全边界。
    pub prefix: String,
    /// 接入点，如 `cos.ap-beijing.myqcloud.com`。不带 scheme。
    pub endpoint: String,
    /// 签名用，如 `ap-beijing`。
    pub region: String,
}

/// 解析失败时抛的错误文本。不返回 Result —— 与仓库其余部分一致：配置错就立刻崩，
/// 给出确切原因，不做默认值兜底。
fn bad(dsn: &str, why: &str) -> ! {
    panic!(
        "kvspace-s3: DSN 不合法（{}）。期望形态：\n  \
         s3://<AccessKeyId>:<AccessKeySecret>@<bucket>/<prefix>?endpoint=<host>&region=<region>\n  \
         实际：{}",
        why,
        redact(dsn)
    )
}

/// 百分号解码。凭据里常含 `+` `/` `=`，URI 里必须编码，所以这一步不能省。
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() {
                panic!("kvspace-s3: 百分号编码不完整: {}", s);
            }
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            match (hi, lo) {
                (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
                _ => panic!("kvspace-s3: 百分号编码非法: {}", s),
            }
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|e| panic!("kvspace-s3: 解码后不是 UTF-8: {}", e))
}

/// 解析 `s3://` 的 DSN。任何一段缺失或为空都 panic。
///
/// `s3://` 前缀可带可不带：`conn()` 按 scheme 分派时已经剥过一次，
/// 但直接调 `s3::connect` 的人会连着 scheme 一起给。
pub fn parse(dsn: &str) -> S3Dsn {
    let rest = dsn.strip_prefix("s3://").unwrap_or(dsn);

    // query 先切走，免得后面的 `?` 干扰路径切分
    let (head, query) = match rest.split_once('?') {
        Some((h, q)) => (h, q),
        None => bad(dsn, "缺少 query，至少要给 endpoint 与 region"),
    };

    // userinfo 与 host+path 的分界是**最后一个** `@`。
    // 不能取第一个：secret 若未编码而含 `@`，取第一个会把 secret 切断。
    let at = head
        .rfind('@')
        .unwrap_or_else(|| bad(dsn, "缺少 userinfo，凭据必须写在 @ 之前"));
    let (userinfo, hostpath) = head.split_at(at);
    let hostpath = &hostpath[1..]; // 去掉 '@'

    let (ak, sk) = userinfo
        .split_once(':')
        .unwrap_or_else(|| bad(dsn, "userinfo 里没有 ':'，需要 <AccessKeyId>:<AccessKeySecret>"));
    let (ak, sk) = (percent_decode(ak), percent_decode(sk));
    if ak.is_empty() {
        bad(dsn, "AccessKeyId 为空");
    }
    if sk.is_empty() {
        bad(dsn, "AccessKeySecret 为空");
    }

    let (bucket, prefix) = match hostpath.split_once('/') {
        Some((b, p)) => (b, p),
        None => (hostpath, ""),
    };
    if bucket.is_empty() {
        bad(dsn, "bucket 为空");
    }
    if prefix.is_empty() {
        bad(
            dsn,
            "prefix 为空。前缀是必需的——它同时是 flush 的安全边界，不允许配成桶根",
        );
    }
    if !prefix.ends_with('/') {
        bad(dsn, "prefix 必须以 '/' 结尾");
    }

    let mut endpoint = None;
    let mut region = None;
    for kv in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv
            .split_once('=')
            .unwrap_or_else(|| bad(dsn, "query 里有不带 '=' 的段"));
        let v = percent_decode(v);
        match k {
            "endpoint" => endpoint = Some(v),
            "region" => region = Some(v),
            _ => bad(dsn, "query 里有未知参数"),
        }
    }

    S3Dsn {
        access_key: ak,
        secret_key: sk,
        bucket: bucket.to_string(),
        prefix: prefix.to_string(),
        endpoint: endpoint.unwrap_or_else(|| bad(dsn, "query 里缺少 endpoint")),
        region: region.unwrap_or_else(|| bad(dsn, "query 里缺少 region")),
    }
}

/// 把 userinfo 里的凭据换成 `***`，其余原样保留。
///
/// **一切打印 DSN 的地方都必须过这个函数**，否则密钥会进日志。
/// host / path / query 不是秘密，保留才便于排查。
pub fn redact(dsn: &str) -> String {
    let (scheme, rest) = match dsn.find("://") {
        Some(i) => (&dsn[..i + 3], &dsn[i + 3..]),
        None => return dsn.to_string(),
    };
    // 只看 authority 段（到第一个 '/' 或 '?' 为止）里有没有 '@'
    let authority_end = rest
        .find(['/', '?'])
        .unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    match authority.rfind('@') {
        Some(at) => {
            let (userinfo, host) = authority.split_at(at);
            // userinfo 里若有 ':'，把冒号两边的段数保留，只换内容，便于看出是「有凭据但被隐去」
            let masked = if userinfo.contains(':') {
                "***:***"
            } else {
                "***"
            };
            format!("{}{}{}{}", scheme, masked, host, &rest[authority_end..])
        }
        None => dsn.to_string(),
    }
}

/// 对象名前缀 + KVSpace key → S3 对象名。
/// 不做任何转义：KVSpace 的 key 字符集（`/` `·` `‥` `…` 与任意成员名）在 S3 对象名里全部合法。
pub fn object_key(prefix: &str, key: &str) -> String {
    debug_assert!(prefix.ends_with('/'), "prefix 必须以 / 结尾（parse 已保证）");
    // key 以 '/' 开头（KVSpace 的绝对路径），拼的时候去掉一个，避免出现 '//'
    format!("{}{}", prefix, key.strip_prefix('/').unwrap_or(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DSN: &str = "s3://AKIDEXAMPLE:SECRETKEY@moechat-test-1252231640/moechat/?endpoint=cos.ap-beijing.myqcloud.com&region=ap-beijing";

    #[test]
    fn 解析完整_dsn() {
        let d = parse(DSN);
        assert_eq!(d.access_key, "AKIDEXAMPLE");
        assert_eq!(d.secret_key, "SECRETKEY");
        assert_eq!(d.bucket, "moechat-test-1252231640");
        assert_eq!(d.prefix, "moechat/");
        assert_eq!(d.endpoint, "cos.ap-beijing.myqcloud.com");
        assert_eq!(d.region, "ap-beijing");
    }

    #[test]
    fn 脱敏遮住凭据但保留其余() {
        let r = redact(DSN);
        assert!(!r.contains("AKIDEXAMPLE"), "AK 没被遮住: {}", r);
        assert!(!r.contains("SECRETKEY"), "SK 没被遮住: {}", r);
        assert!(r.contains("***:***@moechat-test-1252231640"), "{}", r);
        assert!(r.contains("endpoint=cos.ap-beijing.myqcloud.com"), "{}", r);
    }

    #[test]
    fn 脱敏对没有凭据的_dsn_原样返回() {
        assert_eq!(redact("redis://127.0.0.1:6379"), "redis://127.0.0.1:6379");
        assert_eq!(redact("fs:///tmp/kvspace"), "fs:///tmp/kvspace");
    }

    #[test]
    fn 凭据做百分号解码() {
        let d = parse(
            "s3://AK%2FID:sec%3Dret%2Bx@bkt/pfx/?endpoint=e.com&region=r",
        );
        assert_eq!(d.access_key, "AK/ID");
        assert_eq!(d.secret_key, "sec=ret+x");
    }

    #[test]
    fn secret_里的_at_不会被切断() {
        // 未编码的 '@'：取最后一个 '@' 才切得对
        let d = parse("s3://AKID:se@cret@bkt/pfx/?endpoint=e.com&region=r");
        assert_eq!(d.access_key, "AKID");
        assert_eq!(d.secret_key, "se@cret");
        assert_eq!(d.bucket, "bkt");
    }

    #[test]
    fn key_拼接去掉重复斜杠() {
        assert_eq!(object_key("moechat/", "/a/b"), "moechat/a/b");
        assert_eq!(object_key("moechat/", "a/b"), "moechat/a/b");
        assert_eq!(object_key("moechat/", "/"), "moechat/");
    }

    #[test]
    #[should_panic(expected = "prefix 为空")]
    fn 拒绝空前缀() {
        parse("s3://A:B@bkt?endpoint=e.com&region=r");
    }

    #[test]
    #[should_panic(expected = "必须以 '/' 结尾")]
    fn 拒绝不以斜杠结尾的前缀() {
        parse("s3://A:B@bkt/pfx?endpoint=e.com&region=r");
    }

    #[test]
    #[should_panic(expected = "缺少 endpoint")]
    fn 拒绝缺_endpoint() {
        parse("s3://A:B@bkt/pfx/?region=r");
    }

    #[test]
    #[should_panic(expected = "AccessKeySecret 为空")]
    fn 拒绝空密钥() {
        parse("s3://A:@bkt/pfx/?endpoint=e.com&region=r");
    }
}
