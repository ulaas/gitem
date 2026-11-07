//! Reading bytes as text and writing them back the same way.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Why a file can't be edited as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// It has NUL bytes and no UTF-16 mark: a picture, an archive, a compiled file.
    Binary,
    /// Text in some other encoding, such as Latin-1. Editing it as UTF-8 would change bytes
    /// nobody touched, so it's shown read-only.
    NotUtf8,
}

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// The text in `bytes`, how it was encoded, and whether it started with a byte order mark.
pub fn decode(bytes: &[u8]) -> Result<(String, Encoding, bool), Unsupported> {
    if let Some(rest) = bytes.strip_prefix(UTF8_BOM) {
        return std::str::from_utf8(rest).map(|s| (s.to_string(), Encoding::Utf8, true)).map_err(|_| Unsupported::NotUtf8);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, u16::from_le_bytes).map(|s| (s, Encoding::Utf16Le, true));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, u16::from_be_bytes).map(|s| (s, Encoding::Utf16Be, true));
    }
    // Where git looks too, so the editor and the diff agree on what's binary.
    if bytes[..bytes.len().min(8000)].contains(&0) {
        return Err(Unsupported::Binary);
    }
    std::str::from_utf8(bytes).map(|s| (s.to_string(), Encoding::Utf8, false)).map_err(|_| Unsupported::NotUtf8)
}

fn utf16(bytes: &[u8], read: fn([u8; 2]) -> u16) -> Result<String, Unsupported> {
    if !bytes.len().is_multiple_of(2) {
        return Err(Unsupported::Binary);
    }
    let units = bytes.as_chunks::<2>().0.iter().map(|c| read(*c));
    char::decode_utf16(units).collect::<Result<String, _>>().map_err(|_| Unsupported::Binary)
}

pub fn encode(text: &str, encoding: Encoding, bom: bool) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 => {
            let mut out = Vec::with_capacity(text.len() + 3);
            if bom {
                out.extend_from_slice(UTF8_BOM);
            }
            out.extend_from_slice(text.as_bytes());
            out
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let le = encoding == Encoding::Utf16Le;
            let mut out = Vec::with_capacity(text.len() * 2 + 2);
            if bom {
                out.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
            }
            for u in text.encode_utf16() {
                out.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
            }
            out
        }
    }
}

impl Encoding {
    pub fn name(self, bom: bool) -> &'static str {
        match (self, bom) {
            (Encoding::Utf8, false) => "UTF-8",
            (Encoding::Utf8, true) => "UTF-8 with BOM",
            (Encoding::Utf16Le, _) => "UTF-16 LE",
            (Encoding::Utf16Be, _) => "UTF-16 BE",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_encoding_comes_back_byte_for_byte() {
        let samples: [&[u8]; 5] = [b"plain\r\nascii\n", b"\xEF\xBB\xBFwith bom", b"\xFF\xFEh\0i\0", b"\xFE\xFF\0h\0i", "çğ 中文 🦀".as_bytes()];
        for bytes in samples {
            let (text, enc, bom) = decode(bytes).unwrap();
            assert_eq!(encode(&text, enc, bom), bytes, "{}", enc.name(bom));
        }
    }

    #[test]
    fn what_cannot_be_edited_says_why() {
        assert_eq!(decode(b"PNG\0\0\x01"), Err(Unsupported::Binary));
        assert_eq!(decode(b"caf\xe9"), Err(Unsupported::NotUtf8));
        assert_eq!(decode(b"\xFF\xFEodd"), Err(Unsupported::Binary));
    }
}
