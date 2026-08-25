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
    // map[dims]: 与 map:dims 皆可；kind 恒为 "map"。
    if raw.starts_with("map") {
        let dims: Vec<i32> = raw
            .trim_start_matches("map")
            .trim_matches([':', '[', ']'])
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().unwrap_or(0))
            .collect();
        if dims.is_empty() {
            return Err("map 需要 dims，如 map[2,3]:".to_string());
        }
        return Ok(new_map_index(&[], &dims));
    }
    let split = raw.find(':').unwrap_or(raw.len());
    let kind = &raw[..split];
    let repr = &raw[split..].trim_start_matches(':');
    match kind {
        "int" => repr
            .parse::<i64>()
            .map(|i| new_int64(&[i]))
            .map_err(|_| format!("invalid int: {:?}", repr)),
        "float" => repr
            .parse::<f64>()
            .map(|f| new_float64(&[f]))
            .map_err(|_| format!("invalid float: {:?}", repr)),
        "bool" => match *repr {
            "true" => Ok(new_bool(&[true])),
            "false" => Ok(new_bool(&[false])),
            _ => Err(format!("invalid bool: {:?}", repr)),
        },
        "string" => Ok(new_char_byte(repr.as_bytes())),
        "float32" => repr
            .parse::<f32>()
            .map(|f| new_float32(&[f]))
            .map_err(|_| format!("invalid float32: {:?}", repr)),
        "nil" => Ok(XValue::None),
        KIND_INDEX => Ok(new_index(&[])),
        KIND_OBJ => Ok(new_obj_index(&[])),
        _ => Err(format!("unknown kind: {:?}", kind)),
    }
}

fn fatalf(msg: &str) -> ! {
    eprintln!("{}", msg);
    exit(1)
}

/// 用权限位（ro/vid）重编码 XValue 的 head，保留 kind/ref/dims/body。
fn encode_with_perm(v: &XValue, ro: bool, vid: u32) -> Vec<u8> {
    if is_none(v) {
        return Vec::new();
    }
    let data = v.encode();
    let h = decode_xvalue_head(&data);
    encode_head_perm(&h.kind(), h.r#ref(), &h.dims(), h.body(&data), ro, vid)
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
                    fatalf("usage: kvspace set <key> <value> [ro|rw] [vid]");
                }
                let ro = tail.len() > 2 && tail[2] == "ro";
                let vid = if tail.len() > 3 {
                    tail[3].parse::<u32>().unwrap_or(0)
                } else {
                    0
                };
                match parse_value(&tail[1]) {
                    Ok(v) => {
                        let raw = encode_with_perm(&v, ro, vid);
                        if let Err(e) = kv.set(&[KVPair {
                            key: tail[0].clone(),
                            val: v,
                            raw: Some(raw),
                        }]) {
                            fatalf(&e);
                        }
                    }
                    Err(e) => fatalf(&e),
                }
            }
            "head" => {
                for k in tail {
                    let raw = kv.get_raw(k);
                    if raw.is_empty() {
                        println!("{}\t(nil)", k);
                    } else {
                        let h = decode_xvalue_head(&raw);
                        let dims = h
                            .dims()
                            .iter()
                            .map(|d| d.to_string())
                            .collect::<Vec<_>>()
                            .join(",");
                        println!(
                            "{}\t{}\tref={}\tro={}\tvid={}\tndim={}\tdims=[{}]",
                            k,
                            h.kind(),
                            h.r#ref(),
                            h.ro as u8,
                            h.vid,
                            h.ndim(),
                            dims
                        );
                    }
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
