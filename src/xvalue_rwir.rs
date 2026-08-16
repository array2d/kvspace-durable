// xvalue_rwir.rs — 对齐 xvalue_rwir.go（Rwir、Rwfunc）

use crate::r#const::*;
use crate::xvalue::{tlv_encode, XValue};

// ── Rwir ───────────────────────────────────────────────────────────────────
// body 格式：[2B numReads LE][2B numWrites LE][sig]

#[derive(Clone, Debug, PartialEq)]
pub struct Rwir {
    pub body: Vec<u8>,
}

impl Rwir {
    pub fn num_reads(&self) -> i32 {
        u16::from_le_bytes([self.body[0], self.body[1]]) as i32
    }
    pub fn num_writes(&self) -> i32 {
        u16::from_le_bytes([self.body[2], self.body[3]]) as i32
    }
    pub fn sig(&self) -> String {
        String::from_utf8_lossy(&self.body[4..]).into_owned()
    }
    pub fn encode(&self) -> Vec<u8> {
        tlv_encode(KIND_RWIR, &self.body, 1)
    }
}

pub fn new_rwir(num_reads: i32, num_writes: i32, sig: &str) -> XValue {
    let mut raw = Vec::with_capacity(4 + sig.len());
    raw.extend_from_slice(&(num_reads as u16).to_le_bytes());
    raw.extend_from_slice(&(num_writes as u16).to_le_bytes());
    raw.extend_from_slice(sig.as_bytes());
    XValue::Rwir(Rwir { body: raw })
}

pub fn decode_rwir(body: &[u8]) -> Rwir {
    Rwir { body: body.to_vec() }
}

// ── Rwfunc ─────────────────────────────────────────────────────────────────
// body 格式：[2B numReads LE][2B numWrites LE][paramTypes 由 \n 连接]
// al = 指令数（ArrayLen），不含函数定义所在 slot 0。

#[derive(Clone, Debug, PartialEq)]
pub struct Rwfunc {
    pub body: Vec<u8>,
    pub al: i32,
}

impl Rwfunc {
    pub fn num_reads(&self) -> i32 {
        u16::from_le_bytes([self.body[0], self.body[1]]) as i32
    }
    pub fn num_writes(&self) -> i32 {
        u16::from_le_bytes([self.body[2], self.body[3]]) as i32
    }
    pub fn num_insts(&self) -> i32 {
        self.al
    }
    /// ParamTypes 返回参数类型标注（kindexp）列表；无标注则空。
    pub fn param_types(&self) -> Vec<String> {
        if self.body.len() <= 4 {
            return Vec::new();
        }
        String::from_utf8_lossy(&self.body[4..]).split('\n').map(|x| x.to_string()).collect()
    }
    pub fn encode(&self) -> Vec<u8> {
        tlv_encode(KIND_RWFUNC, &self.body, self.al)
    }
}

pub fn new_rwfunc(num_insts: i32, num_reads: i32, num_writes: i32) -> XValue {
    let mut raw = Vec::with_capacity(4);
    raw.extend_from_slice(&(num_reads as u16).to_le_bytes());
    raw.extend_from_slice(&(num_writes as u16).to_le_bytes());
    XValue::Rwfunc(Rwfunc { body: raw, al: num_insts })
}

/// NewRwfuncWithTypes 创建带参数类型标注（kindexp）的 rwfunc。body 追加 \n 分隔的参数类型串。
pub fn new_rwfunc_with_types(num_insts: i32, num_reads: i32, num_writes: i32, param_types: &[String]) -> XValue {
    let joined = param_types.join("\n");
    let mut raw = Vec::with_capacity(4 + joined.len());
    raw.extend_from_slice(&(num_reads as u16).to_le_bytes());
    raw.extend_from_slice(&(num_writes as u16).to_le_bytes());
    raw.extend_from_slice(joined.as_bytes());
    XValue::Rwfunc(Rwfunc { body: raw, al: num_insts })
}

pub fn decode_rwfunc(body: &[u8], al: i32) -> Rwfunc {
    Rwfunc { body: body.to_vec(), al }
}
