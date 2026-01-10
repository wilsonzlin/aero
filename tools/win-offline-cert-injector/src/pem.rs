pub fn decode_cert_file(contents: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    if looks_like_pem(contents) {
        parse_pem_certificates(contents)
    } else {
        Ok(vec![contents.to_vec()])
    }
}

fn looks_like_pem(contents: &[u8]) -> bool {
    let mut i = 0usize;
    while i < contents.len() && contents[i].is_ascii_whitespace() {
        i += 1;
    }
    contents[i..].starts_with(b"-----BEGIN")
}

fn parse_pem_certificates(contents: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    const BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";
    const END: &[u8] = b"-----END CERTIFICATE-----";

    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(begin_at) = find_bytes(&contents[cursor..], BEGIN) {
        let begin_at = cursor + begin_at + BEGIN.len();
        let end_at_rel = find_bytes(&contents[begin_at..], END).ok_or_else(|| {
            "PEM has BEGIN CERTIFICATE without matching END CERTIFICATE".to_string()
        })?;
        let end_at = begin_at + end_at_rel;
        let der = base64_decode(trim_ascii_whitespace(&contents[begin_at..end_at]))?;
        out.push(der);
        cursor = end_at + END.len();
    }

    if out.is_empty() {
        return Err("PEM did not contain any CERTIFICATE blocks".to_string());
    }
    Ok(out)
}

fn trim_ascii_whitespace(mut bytes: &[u8]) -> &[u8] {
    while let Some((&b, rest)) = bytes.split_first() {
        if !b.is_ascii_whitespace() {
            break;
        }
        bytes = rest;
    }
    while let Some((&b, rest)) = bytes.split_last() {
        if !b.is_ascii_whitespace() {
            break;
        }
        bytes = rest;
    }
    bytes
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn base64_decode(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut cleaned = Vec::with_capacity(data.len());
    for &b in data {
        if b.is_ascii_whitespace() {
            continue;
        }
        cleaned.push(b);
    }

    if cleaned.len() % 4 != 0 {
        return Err("invalid base64 length".to_string());
    }

    let mut out = Vec::with_capacity((cleaned.len() / 4) * 3);
    for chunk in cleaned.chunks_exact(4) {
        let mut vals = [0u8; 4];
        let mut padding = 0usize;
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                padding += 1;
                vals[i] = 0;
                continue;
            }
            vals[i] =
                decode_base64_value(b).ok_or_else(|| format!("invalid base64 character: {b:?}"))?;
        }

        let triple = ((vals[0] as u32) << 18)
            | ((vals[1] as u32) << 12)
            | ((vals[2] as u32) << 6)
            | (vals[3] as u32);

        out.push(((triple >> 16) & 0xFF) as u8);
        if padding < 2 {
            out.push(((triple >> 8) & 0xFF) as u8);
        }
        if padding < 1 {
            out.push((triple & 0xFF) as u8);
        }
    }

    Ok(out)
}

fn decode_base64_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decode_known() {
        assert_eq!(base64_decode(b"TQ==").unwrap(), b"M");
        assert_eq!(base64_decode(b"TWE=").unwrap(), b"Ma");
        assert_eq!(base64_decode(b"TWFu").unwrap(), b"Man");
    }

    #[test]
    fn parse_multiple_pem_certs() {
        let pem = b"-----BEGIN CERTIFICATE-----\nTQ==\n-----END CERTIFICATE-----\n\
                    -----BEGIN CERTIFICATE-----\nTWE=\n-----END CERTIFICATE-----\n";
        let certs = decode_cert_file(pem).unwrap();
        assert_eq!(certs.len(), 2);
        assert_eq!(certs[0], b"M");
        assert_eq!(certs[1], b"Ma");
    }
}
