// const.rs — 对齐 const.go
// 路径与成员分隔符统一管理。所有构造 KV 路径、解析限定名、成员访问的地方
// 均须使用这些常量，禁止硬编码 "." 等裸字符串。

// ── 路径结构 ──────────────────────────────────────────────────────────────

pub const PATH_SEP: &str = "/"; // 路径分隔符
pub const DIR_INDEX_SUF: &str = "/"; // 目录索引键后缀（尾斜杠 = 目录，必须以 / 开头的 key 保证不冲突）
pub const RUNTIME_MEMBER_SEP: &str = "‥"; // 运行时保留字段前缀（U+2025）——‥ 的唯一定义处，List 时隐藏
pub const INDEX_VALUE_SEP: &str = "\n"; // index XValueHead 中的路径分隔符
pub const EXT_INDEX_HEAD: &str = "…"; // extindex XValueHead bytes 首元素前缀，如 …/lib/init/

// ── 错误（对齐 const.go 的 error 变量） ──────────────────────────────────

pub const ERR_DIR_MUST_END_WITH_SLASH: &str = "kvspace: index must end with /";
pub const ERR_INVALID_PATH: &str = "kvspace: path must be absolute and canonical";
pub const ERR_INVALID_DIR_VALUE: &str = "kvspace: directory value must be kind=index";
pub const ERR_INVALID_VALUE: &str = "kvspace: value cannot be encoded and decoded losslessly";
pub const ERR_DISCONNECTED: &str = "kvspace: connection is disconnected";
pub const ERR_GET: &str = "kvspace: GET";
pub const ERR_PIPE_EXEC: &str = "kvspace: pipeline exec";
pub const ERR_RESOLVE: &str = "kvspace: 路径解析 GET";
pub const ERR_SCAN: &str = "kvspace: SCAN";
pub const ERR_EXT_WRITE: &str = "kvspace: 禁止对 extindex 只读路径执行写操作";
pub const ERR_EXT_DEL: &str = "kvspace: 禁止删除 extindex 只读路径";
pub const ERR_NOT_DIR: &str = "kvspace: 父路径不是目录";
pub const ERR_PARENT_NOT_FOUND: &str = "kvspace: 父目录不存在";
pub const ERR_EXT_CASCADE: &str = "kvspace: ExtIndex 不容许级联";
pub const ERR_EXT_TARGET: &str = "kvspace: ExtIndex target must be an existing ordinary index";
pub const ERR_EXT_COLLISION: &str = "kvspace: ExtIndex local and extension children overlap";
pub const ERR_LINK_TYPE_MISMATCH: &str = "kvspace: Link target 和 linkpath 类型不一致";
pub const ERR_LINK_PATH_EXISTS: &str = "kvspace: Link path already contains a non-link value";

// ── XValueHead kind ─────────────────────────────────────────────────────────

pub const KIND_NONE: &str = "None";
pub const DICT_SEP: &str = ".";

// kind 继承树：uint8 是所有定长数值类型的祖先。
// 字节宽度由 ElemSize(kind) 定义——任何 elemSize>0 的 kind 继承 uint8。
pub const KIND_BOOL: &str = "bool"; // → uint8, 1B
pub const KIND_INT8: &str = "int8"; // → uint8, 1B
pub const KIND_UINT8: &str = "uint8"; // 基础字节，1B
pub const KIND_CHAR: &str = "char/utf32"; // 码点，4B×N（默认字符串，定宽）
pub const KIND_CHAR_UTF8: &str = "char/utf8"; // UTF-8 字节串，1B×N（变宽，存储/交换）
pub const KIND_CHAR_ASCII: &str = "char/ascii"; // ASCII 字节串，1B×N（定宽）
pub const KIND_INT16: &str = "int16"; // → uint8, 2B
pub const KIND_UINT16: &str = "uint16"; // → uint8, 2B
pub const KIND_INT32: &str = "int32"; // → uint8, 4B
pub const KIND_UINT32: &str = "uint32"; // → uint8, 4B
pub const KIND_FLOAT32: &str = "float32"; // → byte, 4B
pub const KIND_INT64: &str = "int64"; // → byte, 8B
pub const KIND_UINT64: &str = "uint64"; // → byte, 8B
pub const KIND_FLOAT64: &str = "float64"; // → byte, 8B

pub const KIND_DICT: &str = "dict";
pub const KIND_INDEX: &str = "index";
pub const KIND_EXT_INDEX: &str = "extindex"; // 扩展索引，写留在上层
