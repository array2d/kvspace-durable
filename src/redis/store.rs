// redis/store.rs — Redis 存储原语（最小 RESP 客户端，仅 GET/SET/DEL/SCAN/FLUSHDB）。

use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::TcpStream;

use crate::store::KVStore;

pub struct RedisStore {
    stream: RefCell<TcpStream>,
}

pub fn connect(addr: &str) -> RedisStore {
    let addr = if addr.is_empty() { "127.0.0.1:6379" } else { addr };
    RedisStore::new(addr)
}

impl RedisStore {
    pub fn new(addr: &str) -> Self {
        let stream = TcpStream::connect(addr).unwrap_or_else(|e| panic!("kvspace-redis: connect {}: {}", addr, e));
        stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).ok();
        stream.set_write_timeout(Some(std::time::Duration::from_secs(3))).ok();
        RedisStore { stream: RefCell::new(stream) }
    }

    fn cmd(&self, args: &[&[u8]]) -> Resp {
        {
            let mut s = self.stream.borrow_mut();
            let mut out = format!("*{}\r\n", args.len()).into_bytes();
            for a in args {
                out.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
                out.extend_from_slice(a);
                out.extend_from_slice(b"\r\n");
            }
            s.write_all(&out).unwrap_or_else(|e| panic!("kvspace-redis: write: {}", e));
        }
        self.read_resp()
    }

    fn read_line(&self) -> Vec<u8> {
        let mut s = self.stream.borrow_mut();
        let mut line = Vec::new();
        let mut prev = 0u8;
        loop {
            let mut b = [0u8; 1];
            s.read_exact(&mut b).unwrap_or_else(|e| panic!("kvspace-redis: read: {}", e));
            line.push(b[0]);
            if prev == b'\r' && b[0] == b'\n' {
                line.truncate(line.len() - 2);
                return line;
            }
            prev = b[0];
        }
    }

    fn read_resp(&self) -> Resp {
        let line = self.read_line();
        if line.is_empty() {
            return Resp::Error("empty response".to_string());
        }
        match line[0] {
            b'+' => Resp::Simple(String::from_utf8_lossy(&line[1..]).into_owned()),
            b'-' => Resp::Error(String::from_utf8_lossy(&line[1..]).into_owned()),
            b':' => Resp::Integer(String::from_utf8_lossy(&line[1..]).parse().unwrap_or(0)),
            b'$' => {
                let len: i64 = String::from_utf8_lossy(&line[1..]).parse().unwrap_or(-1);
                if len < 0 {
                    return Resp::Bulk(None);
                }
                let mut s = self.stream.borrow_mut();
                let mut data = vec![0u8; len as usize];
                s.read_exact(&mut data).unwrap_or_else(|e| panic!("kvspace-redis: read bulk: {}", e));
                let mut crlf = [0u8; 2];
                s.read_exact(&mut crlf).unwrap_or_else(|e| panic!("kvspace-redis: read crlf: {}", e));
                Resp::Bulk(Some(data))
            }
            b'*' => {
                let n: i64 = String::from_utf8_lossy(&line[1..]).parse().unwrap_or(-1);
                if n < 0 {
                    return Resp::Array(Vec::new());
                }
                let mut arr = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    arr.push(self.read_resp());
                }
                Resp::Array(arr)
            }
            _ => Resp::Error(format!("unexpected RESP type: {}", line[0] as char)),
        }
    }
}

enum Resp {
    Simple(String),
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
        // SCAN 全库后客户端按 prefix 过滤，避免 Redis glob 对 [ ] 等元字符的误匹配。
        let mut keys = Vec::new();
        let mut cursor: i64 = 0;
        loop {
            let cur_str = cursor.to_string();
            match self.cmd(&[b"SCAN", cur_str.as_bytes(), b"COUNT", b"1000"]) {
                Resp::Array(arr) if arr.len() == 2 => {
                    cursor = match &arr[0] {
                        Resp::Bulk(Some(b)) => String::from_utf8_lossy(b).parse().unwrap_or(0),
                        _ => 0,
                    };
                    if let Resp::Array(items) = &arr[1] {
                        for it in items {
                            if let Resp::Bulk(Some(b)) = it {
                                let k = String::from_utf8_lossy(b).into_owned();
                                if k == prefix || (k.len() > prefix.len() && k.starts_with(prefix) && {
                                    let c = k.as_bytes()[prefix.len()];
                                    c == b'/' || c == b'.'
                                }) {
                                    keys.push(k);
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

    fn pexpire(&self, key: &str, ttl: std::time::Duration) -> bool {
        let ms = ttl.as_millis().max(1).to_string();
        match self.cmd(&[b"PEXPIRE", key.as_bytes(), ms.as_bytes()]) {
            Resp::Integer(n) => n == 1,
            _ => false,
        }
    }

    fn flush(&self) {
        let _ = self.cmd(&[b"FLUSHDB"]);
    }
}
