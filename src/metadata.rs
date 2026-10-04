use crate::headlenpow;
use crate::r#const::META_ROOT_NAME;

pub fn key_for(key: &str) -> Option<String> {
    if !key.starts_with('/') || is_reserved(key) {
        return None;
    }
    let mut out = format!("/{META_ROOT_NAME}/");
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for b in key.bytes() {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 15) as usize] as char);
    }
    Some(out)
}

pub fn is_reserved(key: &str) -> bool {
    key == "/.kvspace-meta" || key.starts_with("/.kvspace-meta/")
}

pub fn original_key(hex: &str) -> Option<String> {
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        bytes.push(((hi << 4) | lo) as u8);
    }
    let key = String::from_utf8(bytes).ok()?;
    if key.starts_with('/') && !is_reserved(&key) {
        Some(key)
    } else {
        None
    }
}

pub fn encode(ro: bool, vid: u32) -> Option<Vec<u8>> {
    let mut body = [0u8; 5];
    body[0] = u8::from(ro);
    body[1..].copy_from_slice(&vid.to_le_bytes());
    headlenpow::encode(6, 1, 5, 5, "[5]byte", &body, 5)
}

pub fn decode(data: &[u8]) -> Option<(bool, u32)> {
    let h = headlenpow::decode(data)?;
    if h.total != data.len()
        || h.flags != 1
        || h.a != 5
        || h.b != 5
        || h.langtype != "[5]byte"
        || h.body[0] > 1
    {
        return None;
    }
    Some((
        h.body[0] == 1,
        u32::from_le_bytes(h.body[1..5].try_into().ok()?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_roundtrip() {
        assert_eq!(key_for("/a").as_deref(), Some("/.kvspace-meta/2f61"));
        assert_eq!(key_for("/.kvspace-meta/2f61"), None);
        assert_eq!(original_key("2f61").as_deref(), Some("/a"));
        let mut data = encode(true, 0x12345678).unwrap();
        assert_eq!(decode(&data), Some((true, 0x12345678)));
        data[64] = 2;
        assert_eq!(decode(&data), None);
    }
}
