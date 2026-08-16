// fs/store.rs — 文件系统存储原语。
// 每个 KV key 映射为 root 下一个文件，文件名 = key 字节的 hex（扁平、无层级，
// 规避 KV 树「同一路径既是叶又是目录」与文件系统层级冲突的问题）。

use std::path::{Path, PathBuf};

use crate::store::KVStore;

pub struct FsStore {
    root: PathBuf,
}

pub fn connect(root: &str) -> FsStore {
    let root = if root.is_empty() { "/tmp/kvspace-fs" } else { root };
    FsStore::new(root)
}

impl FsStore {
    pub fn new(root: &str) -> Self {
        std::fs::create_dir_all(root).unwrap_or_else(|e| panic!("kvspace-fs: create root {}: {}", root, e));
        FsStore { root: PathBuf::from(root) }
    }

    fn key_file(&self, key: &str) -> PathBuf {
        self.root.join(hex_encode(key.as_bytes()))
    }
}

impl KVStore for FsStore {
    fn get(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.key_file(key)).ok()
    }

    fn set(&self, key: &str, val: &[u8]) {
        std::fs::write(self.key_file(key), val).unwrap_or_else(|e| panic!("kvspace-fs: set {}: {}", key, e));
    }

    fn del(&self, keys: &[&str]) {
        for k in keys {
            let _ = std::fs::remove_file(self.key_file(k));
        }
    }

    fn scan_keys(&self, prefix: &str) -> Vec<String> {
        let mut keys = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if let Ok(key) = String::from_utf8(hex_decode(&name)) {
                    if key.starts_with(prefix) {
                        keys.push(key);
                    }
                }
            }
        }
        keys
    }

    fn flush(&self) {
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for e in entries.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// 供调试/测试：返回 root 下的文件数。
pub fn file_count(store: &FsStore) -> usize {
    std::fs::read_dir(&store.root).map(|d| d.count()).unwrap_or(0)
}

#[allow(dead_code)]
fn _assert_path(_p: &Path) {}
