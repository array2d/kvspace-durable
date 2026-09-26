// s3/store.rs — KVStore 的 S3 实现。
//
// 对象名 = DSN 里的前缀 + KVSpace key，**不做任何转义**：
// KVSpace 的 key 字符集（`/`、`·`、`‥`、`…` 与任意成员名）在 S3 对象名里全部合法。
// 前缀同时是 flush 的安全边界 —— 任何删除都只发生在前缀之下。

use std::time::Duration;

use super::dsn::{self, S3Dsn};
use super::sigv4::{self, uri_encode};
use crate::store::KVStore;

/// 5xx 与网络错误重试几次。4xx 一律不重试 —— 签名错、桶不存在这类问题重试一万次也不会好。
const MAX_ATTEMPTS: u32 = 3;

pub struct S3Store {
    dsn: S3Dsn,
    agent: ureq::Agent,
    /// 虚拟主机式主机名：`<bucket>.<endpoint>`
    host: String,
}

/// 按 DSN 建一个 S3 后端。与 `redis::connect`、`fs::connect` 同形。
pub fn connect(dsn: &str) -> S3Store {
    S3Store::connect(dsn)
}

/// 一次请求的签名结果与 URL。
struct Prepared {
    url: String,
    authorization: String,
    amz_date: String,
    payload_hash: String,
}

/// 请求结果。三点区分：成功、明确不存在、其他（一律 panic）。
enum Outcome {
    Ok(Vec<u8>),
    NotFound,
}

fn hex_of(body: &[u8]) -> String {
    sigv4::sha256_hex(body)
}

// ── 极简 XML 取值 ───────────────────────────────────────────────────────
//
// 只处理 S3 ListObjectsV2 的响应，拿 `<Key>` 与 `<NextContinuationToken>`。
// 不引 XML 依赖：S3 的列表响应结构固定，写一个按标签取值的提取器比拉一整个
// XML 库划算得多。**但它只认平标签**，遇到嵌套同名标签会错，所以只用在列表响应上。

fn xml_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'&' {
            if let Some(semi) = s[i..].find(';') {
                let ent = &s[i + 1..i + semi];
                let rep = match ent {
                    "amp" => Some('&'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => ent
                        .strip_prefix('#')
                        .and_then(|n| n.parse::<u32>().ok())
                        .and_then(char::from_u32),
                };
                if let Some(c) = rep {
                    out.push(c);
                    i += semi + 1;
                    continue;
                }
            }
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

/// 取出所有 `<tag>值</tag>`，按出现顺序。
fn xml_tag_values(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(a) = rest.find(&open) {
        let after = &rest[a + open.len()..];
        match after.find(&close) {
            Some(b) => {
                out.push(xml_unescape(&after[..b]));
                rest = &after[b + close.len()..];
            }
            None => break,
        }
    }
    out
}

impl S3Store {
    /// 从 DSN 建连接。DSN 任何一段不合法都在这里 panic（见 `dsn::parse`）。
    pub fn connect(dsn_str: &str) -> Self {
        let dsn = dsn::parse(dsn_str);
        // 虚拟主机式：用户给的访问地址就是 https://<bucket>.cos.ap-beijing.myqcloud.com
        // 若 endpoint 里已经带了桶名前缀，就不再加一次。
        let prefix = format!("{}.", dsn.bucket);
        let host = if dsn.endpoint.starts_with(&prefix) {
            dsn.endpoint.clone()
        } else {
            format!("{}{}", prefix, dsn.endpoint)
        };
        S3Store {
            dsn,
            agent: ureq::Agent::new_with_defaults(),
            host,
        }
    }

    /// 拼 URL 并签名。`key` 是 KVSpace key（以 `/` 开头）。
    ///
    /// **`key` 传空串表示「桶级操作」**：路径用 `/`，目标由 `prefix` 这类参数指定。
    /// ListObjects 就是这样——它列的是整个桶（再按 prefix 筛），不是某个对象。
    /// KVSpace 的合法 key 一定以 `/` 开头，所以空串这个哨兵不会与真实 key 撞。
    fn prepare(&self, method: &str, key: &str, params: &[(&str, String)], body: &[u8]) -> Prepared {
        let canonical_uri = if key.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", uri_encode(&dsn::object_key(&self.dsn.prefix, key), true))
        };
        let query = sigv4::canonical_query(params);
        let url = if query.is_empty() {
            format!("https://{}{}", self.host, canonical_uri)
        } else {
            format!("https://{}{}?{}", self.host, canonical_uri, query)
        };
        let s = sigv4::sign(
            &self.dsn.access_key,
            &self.dsn.secret_key,
            &self.dsn.region,
            &self.host,
            method,
            &canonical_uri,
            params,
            body,
        );
        Prepared {
            url,
            authorization: s.authorization,
            amz_date: s.amz_date,
            payload_hash: s.payload_hash,
        }
    }

    /// 带体重试地发一次请求。
    fn send(&self, method: &str, key: &str, params: &[(&str, String)], body: &[u8]) -> Outcome {
        let mut last: Option<String> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            let p = self.prepare(method, key, params, body);
            let rb = match method {
                "PUT" => self.agent.put(&p.url),
                "POST" => self.agent.post(&p.url),
                _ => panic!("kvspace-s3: send 不支持 {}（无体请求走 send_empty）", method),
            }
            .header("authorization", &p.authorization)
            .header("x-amz-date", &p.amz_date)
            .header("x-amz-content-sha256", &p.payload_hash);

            let r = rb.send(body);
            match r {
                // PUT / POST 的响应体是空的，不需要读。
                Ok(_) => return Outcome::Ok(Vec::new()),
                Err(ureq::Error::StatusCode(code)) => {
                    // 4xx：立刻崩。签名错、桶不存在、参数错，重试没有意义。
                    panic!(
                        "kvspace-s3: {} {} 返回 {}（4xx 不重试）。{}",
                        method,
                        dsn::redact(key),
                        code,
                        snippet(body)
                    );
                }
                Err(e) => {
                    last = Some(format!("{}", e));
                    if attempt < MAX_ATTEMPTS {
                        std::thread::sleep(Duration::from_millis(100 * 4u64.pow(attempt - 1)));
                    }
                }
            }
        }
        panic!(
            "kvspace-s3: {} 重试 {} 次仍失败: {}",
            method,
            MAX_ATTEMPTS,
            last.unwrap_or_default()
        );
    }

    /// 无体请求（GET / HEAD / DELETE / POST-?delete 的枚举）。
    fn send_empty(
        &self,
        method: &str,
        key: &str,
        params: &[(&str, String)],
        range: Option<&str>,
    ) -> Outcome {
        let mut last: Option<String> = None;
        for attempt in 1..=MAX_ATTEMPTS {
            let p = self.prepare(method, key, params, b"");
            let mut rb = match method {
                "GET" => self.agent.get(&p.url),
                "HEAD" => self.agent.head(&p.url),
                "DELETE" => self.agent.delete(&p.url),
                _ => panic!("kvspace-s3: send_empty 不支持 {}", method),
            }
            .header("authorization", &p.authorization)
            .header("x-amz-date", &p.amz_date)
            .header("x-amz-content-sha256", &p.payload_hash);

            if let Some(r) = range {
                rb = rb.header("range", r);
            }

            match rb.call() {
                Ok(mut resp) => {
                    let body = resp
                        .body_mut()
                        .read_to_vec()
                        .unwrap_or_else(|e| panic!("kvspace-s3: 读响应体失败: {}", e));
                    return Outcome::Ok(body);
                }
                // 404 是「不存在」，不是错误 —— S3 就是用 404 表达这个意思的。
                Err(ureq::Error::StatusCode(404)) => return Outcome::NotFound,
                // HEAD 没有体，ureq 对 HEAD 的 404 也走这里
                Err(ureq::Error::StatusCode(code)) if code >= 400 && code < 500 => {
                    panic!(
                        "kvspace-s3: {} {} 返回 {}（4xx 不重试）",
                        method,
                        dsn::redact(key),
                        code
                    );
                }
                Err(e) => {
                    last = Some(format!("{}", e));
                    if attempt < MAX_ATTEMPTS {
                        std::thread::sleep(Duration::from_millis(100 * 4u64.pow(attempt - 1)));
                    }
                }
            }
        }
        panic!(
            "kvspace-s3: {} 重试 {} 次仍失败: {}",
            method,
            MAX_ATTEMPTS,
            last.unwrap_or_default()
        );
    }

    /// 列出前缀下的全部对象名（含前缀，已剥掉 DSN 前缀）。
    fn list_all(&self, kv_prefix: &str) -> Vec<String> {
        let object_prefix = dsn::object_key(&self.dsn.prefix, kv_prefix);
        let mut keys: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut params: Vec<(&str, String)> = vec![
                ("list-type", "2".to_string()),
                ("prefix", object_prefix.clone()),
                ("max-keys", "1000".to_string()),
            ];
            if let Some(t) = &token {
                params.push(("continuation-token", t.clone()));
            }
            let body = match self.send_empty("GET", "", &params, None) {
                Outcome::Ok(b) => b,
                Outcome::NotFound => break,
            };
            let xml = String::from_utf8(body)
                .unwrap_or_else(|e| panic!("kvspace-s3: 列表响应不是 UTF-8: {}", e));
            for k in xml_tag_values(&xml, "Key") {
                // 剥回 KVSpace key（补上前导 `/`）
                let stripped = k
                    .strip_prefix(&self.dsn.prefix)
                    .unwrap_or_else(|| panic!("kvspace-s3: 列表返回了前缀之外的对象: {}", k));
                keys.push(format!("/{}", stripped));
            }
            if xml_tag_values(&xml, "IsTruncated").first().map(|s| s.as_str()) != Some("true") {
                break;
            }
            token = xml_tag_values(&xml, "NextContinuationToken").into_iter().next();
            if token.is_none() {
                break;
            }
        }
        keys
    }
}

/// 出错时截一段请求体帮助定位（删除请求的体里就是 key 列表）。
fn snippet(body: &[u8]) -> String {
    let s = String::from_utf8_lossy(body);
    let t: String = s.chars().take(200).collect();
    format!("请求体前 200 字符: {}", t)
}

impl KVStore for S3Store {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        match self.send_empty("GET", key, &[], None) {
            Outcome::Ok(b) => Some(b),
            Outcome::NotFound => None,
        }
    }

    fn set(&self, key: &str, val: &[u8]) {
        let _ = hex_of(val); // 载荷哈希在 prepare/sign 里算，这里只留个语义锚点
        self.send("PUT", key, &[], val);
    }

    fn del(&self, keys: &[&str]) {
        // 逐个 DELETE，没用 S3 的多对象删除接口。
        // 后者要求 `Content-MD5`，而算 MD5 要再引一个依赖 —— 对当前用量不值当。
        // 删除不是热路径；真成问题时再加。
        for k in keys {
            match self.send_empty("DELETE", k, &[], None) {
                Outcome::Ok(_) | Outcome::NotFound => {}
            }
        }
    }

    fn exists(&self, key: &str) -> bool {
        // HEAD 不传 body，比整读便宜得多。这是覆盖默认实现最值的一处。
        match self.send_empty("HEAD", key, &[], None) {
            Outcome::Ok(_) => true,
            Outcome::NotFound => false,
        }
    }

    fn get_part(&self, key: &str, off: u32, len: u32) -> Option<Vec<u8>> {
        if len == 0 {
            return self.get(key).map(|_| Vec::new());
        }
        // S3 原生支持 Range，不用整读。
        let range = format!("bytes={}-{}", off, off + len - 1);
        match self.send_empty("GET", key, &[], Some(&range)) {
            Outcome::Ok(b) => Some(b),
            Outcome::NotFound => None,
        }
    }

    fn flush(&self) {
        // 安全边界：只删 DSN 前缀之下的对象。
        // `dsn::parse` 已保证前缀非空且以 `/` 结尾，所以这里不可能删到别人的数据。
        // 这条不变量要是被改动，整个桶都可能被清空 —— 改 parse 时务必留意。
        debug_assert!(self.dsn.prefix.ends_with('/') && !self.dsn.prefix.is_empty());
        let keys = self.list_all("/");
        for k in &keys {
            match self.send_empty("DELETE", k, &[], None) {
                Outcome::Ok(_) | Outcome::NotFound => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_取值与反转义() {
        let xml = "<ListBucketResult>\
                   <Contents><Key>a/b.txt</Key></Contents>\
                   <Contents><Key>c&amp;d</Key></Contents>\
                   <Contents><Key>e%20f</Key></Contents>\
                   <IsTruncated>false</IsTruncated>\
                   </ListBucketResult>";
        let keys = xml_tag_values(xml, "Key");
        assert_eq!(keys, vec!["a/b.txt", "c&d", "e%20f"]);
        assert_eq!(xml_tag_values(xml, "IsTruncated"), vec!["false"]);
    }

    #[test]
    fn xml_取分页令牌() {
        let xml = "<x><NextContinuationToken>1/x+y=</NextContinuationToken></x>";
        assert_eq!(
            xml_tag_values(xml, "NextContinuationToken"),
            vec!["1/x+y="]
        );
    }

    #[test]
    fn xml_数字实体() {
        assert_eq!(xml_unescape("&#22909;"), "好");
        assert_eq!(xml_unescape("&amp;lt;"), "&lt;"); // 只解一层
    }

    #[test]
    fn 主机名不重复加桶名前缀() {
        let a = S3Store::connect(
            "s3://A:B@bkt/p/?endpoint=cos.ap-beijing.myqcloud.com&region=ap-beijing",
        );
        assert_eq!(a.host, "bkt.cos.ap-beijing.myqcloud.com");
        let b = S3Store::connect(
            "s3://A:B@bkt/p/?endpoint=bkt.cos.ap-beijing.myqcloud.com&region=ap-beijing",
        );
        assert_eq!(b.host, "bkt.cos.ap-beijing.myqcloud.com");
    }

    #[test]
    fn 列表请求的_url_形状() {
        let s = S3Store::connect(
            "s3://A:B@bkt/moechat/?endpoint=cos.ap-beijing.myqcloud.com&region=ap-beijing",
        );
        let p = s.prepare(
            "GET",
            "",
            &[("list-type", "2".into()), ("prefix", "moechat/a b/".into())],
            b"",
        );
        assert!(p.url.starts_with("https://bkt.cos.ap-beijing.myqcloud.com/?"), "{}", p.url);
        assert!(p.url.contains("list-type=2"), "{}", p.url);
        assert!(p.url.contains("prefix=moechat%2Fa%20b%2F"), "{}", p.url);
        assert!(!p.url.contains("moechat/a b/"), "路径没编码: {}", p.url);
    }

    #[test]
    fn 多字节_key_的_url() {
        let s = S3Store::connect(
            "s3://A:B@bkt/moechat/?endpoint=cos.ap-beijing.myqcloud.com&region=ap-beijing",
        );
        let p = s.prepare("GET", "/x·y‥z…w", &[], b"");
        // 规范 URI 里这些多字节字符必须逐字节编码，不能原样
        assert!(!p.url.contains('·'), "{}", p.url);
        assert!(p.url.contains("%C2%B7"), "{}", p.url);
        assert!(p.url.contains("%E2%80%A5"), "{}", p.url);
        assert!(p.url.contains("%E2%80%A6"), "{}", p.url);
    }
}
