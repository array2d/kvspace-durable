// redis/store.rs — Redis 存储原语（最小 RESP 客户端，仅 GET/SET/DEL/SCAN/FLUSHDB）。

use std::cell::RefCell;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

use crate::store::{is_subtree_key, KVStore};

type Conn = BufReader<TcpStream>;

pub struct RedisStore {
    addr: String,
    /// Set to `None` on I/O error or timeout so the next command reconnects instead of
    /// reading a delayed response left in the stream.
    stream: RefCell<Option<Conn>>,
}

pub fn connect(addr: &str) -> RedisStore {
    let addr = if addr.is_empty() {
        "127.0.0.1:6379"
    } else {
        addr
    };
    RedisStore::new(addr)
}

impl RedisStore {
    pub fn new(addr: &str) -> Self {
        let conn =
            Self::open(addr).unwrap_or_else(|e| panic!("kvspace-redis: connect {}: {}", addr, e));
        RedisStore {
            addr: addr.to_string(),
            stream: RefCell::new(Some(conn)),
        }
    }

    fn open(addr: &str) -> io::Result<Conn> {
        let stream = TcpStream::connect(addr)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(std::time::Duration::from_secs(3)))?;
        stream.set_nodelay(true)?; // disable Nagle for request/reply round-trips
        Ok(BufReader::new(stream)) // buffer RESP reads to avoid per-byte syscalls
    }

    fn cmd(&self, args: &[&[u8]]) -> Resp {
        let response = {
            let mut slot = self.stream.borrow_mut();
            let r = Self::round_trip(&self.addr, &mut slot, args);
            if r.is_err() {
                *slot = None; // drop desynchronized stream; reconnect on next command
            }
            r
        };
        match response {
            // Server error reply is a complete frame; stream remains aligned.
            Ok(Resp::Error(message)) => panic!("kvspace-redis: {message}"),
            Ok(r) => r,
            Err(e) => panic!("kvspace-redis: {}: {e}", self.addr),
        }
    }

    fn round_trip(addr: &str, slot: &mut Option<Conn>, args: &[&[u8]]) -> io::Result<Resp> {
        if slot.is_none() {
            *slot = Some(Self::open(addr)?);
        }
        let conn = slot.as_mut().expect("connection just opened");
        let mut out = format!("*{}\r\n", args.len()).into_bytes();
        for a in args {
            out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
            out.extend_from_slice(a);
            out.extend_from_slice(b"\r\n");
        }
        conn.get_mut().write_all(&out)?;
        read_resp(conn)
    }
}

fn read_line(conn: &mut Conn) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    if conn.read_until(b'\n', &mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "connection closed",
        ));
    }
    if line.ends_with(b"\n") {
        line.pop();
    }
    if line.ends_with(b"\r") {
        line.pop();
    }
    Ok(line)
}

fn read_resp(conn: &mut Conn) -> io::Result<Resp> {
    let line = read_line(conn)?;
    let Some((&kind, rest)) = line.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty RESP line",
        ));
    };
    let text = || String::from_utf8_lossy(rest).into_owned();
    Ok(match kind {
        b'+' => Resp::Simple(text()),
        b'-' => Resp::Error(text()),
        b':' => Resp::Integer(text().parse().unwrap_or(0)),
        b'$' => {
            let len: i64 = text().parse().unwrap_or(-1);
            if len < 0 {
                return Ok(Resp::Bulk(None));
            }
            let mut data = vec![0u8; len as usize];
            conn.read_exact(&mut data)?;
            let mut crlf = [0u8; 2];
            conn.read_exact(&mut crlf)?;
            Resp::Bulk(Some(data))
        }
        b'*' => {
            let n: i64 = text().parse().unwrap_or(-1);
            if n < 0 {
                return Ok(Resp::Array(Vec::new()));
            }
            let mut arr = Vec::with_capacity(n as usize);
            for _ in 0..n {
                arr.push(read_resp(conn)?);
            }
            Resp::Array(arr)
        }
        // Unrecognized RESP type byte indicates stream desynchronization.
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected RESP type: {}", other as char),
            ))
        }
    })
}

/// Builds a SCAN MATCH glob (`* ? [ ] \` escaped, plus trailing `*`; empty prefix -> `/*`).
fn scan_pattern(prefix: &str) -> Vec<u8> {
    let base = if prefix.is_empty() { "/" } else { prefix };
    let mut pat = Vec::with_capacity(base.len() + 1);
    for &b in base.as_bytes() {
        if matches!(b, b'*' | b'?' | b'[' | b']' | b'\\') {
            pat.push(b'\\');
        }
        pat.push(b);
    }
    pat.push(b'*');
    pat
}

enum Resp {
    // 字段 0 暂未被读取——Redis 错误文本当前被静默丢弃，待接入 Result 错误传播后启用（见 todo）
    #[allow(dead_code)]
    Simple(String),
    // 同上
    #[allow(dead_code)]
    Error(String),
    Integer(i64),
    Bulk(Option<Vec<u8>>),
    Array(Vec<Resp>),
}

impl KVStore for RedisStore {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        match self.cmd(&[b"GET", key.as_bytes()]) {
            Resp::Bulk(b) => b,
            _ => None,
        }
    }

    fn get_many(&self, keys: &[&str]) -> Vec<Option<Vec<u8>>> {
        if keys.is_empty() {
            return Vec::new();
        }
        let mut args: Vec<&[u8]> = vec![b"MGET"];
        for k in keys {
            args.push(k.as_bytes());
        }
        match self.cmd(&args) {
            Resp::Array(items) => items
                .into_iter()
                .map(|it| match it {
                    Resp::Bulk(b) => b,
                    _ => None,
                })
                .collect(),
            _ => vec![None; keys.len()],
        }
    }

    fn set(&self, key: &str, val: &[u8]) {
        let _ = self.cmd(&[b"SET", key.as_bytes(), val]);
    }

    fn exists(&self, key: &str) -> bool {
        matches!(self.cmd(&[b"EXISTS", key.as_bytes()]), Resp::Integer(n) if n > 0)
    }

    fn get_part(&self, key: &str, off: u32, len: u32) -> Option<Vec<u8>> {
        if len == 0 {
            return Some(Vec::new());
        }
        let (o, e) = (off.to_string(), (off as u64 + len as u64 - 1).to_string());
        match self.cmd(&[b"GETRANGE", key.as_bytes(), o.as_bytes(), e.as_bytes()]) {
            Resp::Bulk(b) => b, // 不存在 key 也返回 ""，调用方先 exists 判定
            _ => None,
        }
    }

    fn set_part(&self, key: &str, off: u32, buf: &[u8]) {
        let o = off.to_string();
        let _ = self.cmd(&[b"SETRANGE", key.as_bytes(), o.as_bytes(), buf]);
    }

    fn del(&self, keys: &[&str]) {
        if keys.is_empty() {
            return;
        }
        let mut args: Vec<&[u8]> = vec![b"DEL"];
        for k in keys {
            args.push(k.as_bytes());
        }
        let _ = self.cmd(&args);
    }

    fn scan_keys(&self, prefix: &str) -> Vec<String> {
        // Filter by prefix server-side via MATCH; check subtree boundaries locally via is_subtree_key.
        let pattern = scan_pattern(prefix);
        let mut keys = Vec::new();
        let mut cursor: i64 = 0;
        loop {
            let cur = cursor.to_string();
            match self.cmd(&[
                b"SCAN",
                cur.as_bytes(),
                b"MATCH",
                &pattern,
                b"COUNT",
                b"1000",
            ]) {
                Resp::Array(parts) if parts.len() == 2 => {
                    cursor = match &parts[0] {
                        Resp::Bulk(Some(bytes)) => {
                            String::from_utf8_lossy(bytes).parse().unwrap_or(0)
                        }
                        _ => 0,
                    };
                    if let Resp::Array(items) = &parts[1] {
                        for item in items {
                            if let Resp::Bulk(Some(bytes)) = item {
                                if let Ok(key) = String::from_utf8(bytes.clone()) {
                                    if is_subtree_key(prefix, &key) {
                                        keys.push(key);
                                    }
                                }
                            }
                        }
                    }
                    if cursor == 0 {
                        break;
                    }
                }
                _ => break,
            }
        }
        keys
    }

    fn flush(&self) {
        let _ = self.cmd(&[b"FLUSHDB"]);
    }
}
