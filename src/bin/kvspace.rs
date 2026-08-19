// kvspace CLI — 顶替 Go 版，命令与输出格式对齐 kvspace-go/cmd/kvspace。

use std::env;
use std::process::exit;

use kvspace_durable::*;

fn default_dsn() -> String {
    env::var("KVSPACE").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

/// ParseValue 解析 CLI value 字符串为 XValue。
fn parse_value(raw: &str) -> Result<XValue, String> {
    if let Some(rest) = raw.strip_prefix('*') {
        if let Some(colon) = rest.find(':') {
            return Ok(new_ptr(&rest[..colon], &rest[colon + 1..], 1));
        }
        return Ok(new_ptr("", rest, 1));
    }
    match raw.find(':') {
        None => Ok(new_uint8(raw.as_bytes())),
        Some(idx) => {
            let kind = &raw[..idx];
            let repr = &raw[idx + 1..];
            match kind {
                "int" => repr
                    .parse::<i64>()
                    .map(|i| new_int64(&[i]))
                    .map_err(|_| format!("invalid int: {:?}", repr)),
                "float" => repr
                    .parse::<f64>()
                    .map(|f| new_float64(&[f]))
                    .map_err(|_| format!("invalid float: {:?}", repr)),
                "bool" => match repr {
                    "true" => Ok(new_bool(&[true])),
                    "false" => Ok(new_bool(&[false])),
                    _ => Err(format!("invalid bool: {:?}", repr)),
                },
                "string" => Ok(new_char_byte(repr.as_bytes())),
                "nil" => Ok(XValue::None),
                KIND_INDEX => Ok(new_index(&[])),
                KIND_DICT => Ok(new_dict_index(&[])),
                _ => Err(format!("unknown kind: {:?}", kind)),
            }
        }
    }
}

fn fatalf(msg: &str) -> ! {
    eprintln!("{}", msg);
    exit(1)
}

fn parse_bool_flag(args: &[String], name: &str, default: bool) -> (bool, Vec<String>) {
    let mut val = default;
    let mut rest = Vec::new();
    for a in args {
        if let Some(v) = a.strip_prefix(&format!("--{}=", name)) {
            val = v == "true";
        } else if a.as_str() == format!("--{}", name) {
            val = true;
        } else {
            rest.push(a.clone());
        }
    }
    (val, rest)
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut dsn = default_dsn();
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--kvspace" && i + 1 < args.len() {
            dsn = args[i + 1].clone();
            i += 2;
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }
    if rest.is_empty() {
        eprintln!("usage: kvspace [--kvspace dsn] <subcommand> [args]");
        exit(1);
    }

    let mut kv = conn(&dsn);
    let sub = rest[0].clone();
    let tail = &rest[1..];

    let run = |kv: &mut dyn KVSpace| -> () {
        match sub.as_str() {
            "get" => {
                for k in tail {
                    let v = get_one(kv, k);
                    if is_none(&v) {
                        println!("{}\t(nil)", k);
                    } else {
                        println!("{}\t{}", k, format(&v));
                    }
                }
            }
            "set" => {
                if tail.len() < 2 {
                    fatalf("usage: kvspace set <key> <value>");
                }
                match parse_value(&tail[1]) {
                    Ok(v) => {
                        if let Err(e) = kv.set(&[KVPair { key: tail[0].clone(), val: v }]) {
                            fatalf(&e);
                        }
                    }
                    Err(e) => fatalf(&e),
                }
            }
            "del" => {
                if let Err(e) = kv.del(tail) {
                    fatalf(&e);
                }
            }
            "deltree" => {
                if let Some(p) = tail.first() {
                    if let Err(e) = kv.del_tree(p) {
                        fatalf(&e);
                    }
                }
            }
            "mkindex" => {
                if let Some(p) = tail.first() {
                    if let Err(e) = kv.mkindex(p) {
                        fatalf(&e);
                    }
                }
            }
            "delextindex" => {
                if let Some(p) = tail.first() {
                    if let Err(e) = kv.del_ext_index(p) {
                        fatalf(&e);
                    }
                }
            }
            "extindex" => {
                if tail.len() >= 2 {
                    if let Err(e) = kv.ext_index(&tail[0], &tail[1]) {
                        fatalf(&e);
                    }
                }
            }
            "list" | "ls" => {
                let (show_ext, r1) = parse_bool_flag(tail, "showext", true);
                let (show_kind, r2) = parse_bool_flag(&r1, "kind", true);
                if let Some(p) = r2.first() {
                    fprint_list(kv, p, show_ext, show_kind);
                }
            }
            "tree" => {
                let (show_ext, r1) = parse_bool_flag(tail, "showext", true);
                let (show_kind, r2) = parse_bool_flag(&r1, "kind", false);
                if let Some(p) = r2.first() {
                    println!("{}", p);
                    fprint_tree(kv, p, "", show_ext, show_kind);
                }
            }
            "clear" => {
                let _ = kv.clear();
            }
            other => {
                eprintln!("unknown subcommand: {}", other);
                exit(1);
            }
        }
    };
    run(kv.as_mut());
}
