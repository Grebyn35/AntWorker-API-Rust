//! Base64, in which the setup sends each value to the server, and the server sends the settings
//! file it holds, as `base64` on the server reads and writes it.

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `bytes` in base64, as `base64 -d` on the server reads it.
pub(super) fn encode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |group, (index, &byte)| {
                group | u32::from(byte) << (16 - 8 * index)
            });
        for index in 0..4 {
            if index <= chunk.len() {
                let sextet = (group >> (18 - 6 * index)) & 0x3f;
                text.push(char::from(BASE64[sextet as usize]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// The bytes that the base64 `text` holds, as `base64` on the server writes it.
pub(super) fn decode(text: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    let mut group = 0_u32;
    let mut count = 0;
    for c in text.trim_end_matches('=').bytes() {
        let sextet = BASE64
            .iter()
            .position(|&symbol| symbol == c)
            .ok_or_else(|| format!("{text:?} isn't base64"))?;
        group = group << 6 | u32::try_from(sextet).map_err(|error| error.to_string())?;
        count += 1;
        if count == 4 {
            bytes.extend_from_slice(&group.to_be_bytes()[1..]);
            group = 0;
            count = 0;
        }
    }
    // The bits of a last group of two or three symbols that come after its last whole byte are
    // zero.
    match count {
        0 => {}
        2 => bytes.push((group >> 4).to_be_bytes()[3]),
        3 => bytes.extend_from_slice(&(group >> 2).to_be_bytes()[2..]),
        _ => return Err(format!("{text:?} isn't base64")),
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_22_base64_matches_the_standard_s_examples() {
        for (bytes, text) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(bytes), text);
            assert_eq!(decode(text).unwrap(), bytes);
        }
        assert!(decode("Zm9v!").is_err());
    }
}
