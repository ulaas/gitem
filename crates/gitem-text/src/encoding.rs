//! Reading bytes as text and writing them back the same way.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// Text that isn't UTF-8 is read as Windows-1252, the usual one for such files. Every
    /// byte means a character in it, so the file comes back as it was.
    Windows1252,
}

/// Why a file can't be edited as text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// It has NUL bytes and no UTF-16 mark: a picture, an archive, a compiled file.
    Binary,
}

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];

/// The text in `bytes`, how it was encoded, and whether it started with a byte order mark.
pub fn decode(bytes: &[u8]) -> Result<(String, Encoding, bool), Unsupported> {
    if let Some(rest) = bytes.strip_prefix(UTF8_BOM) {
        return Ok(match std::str::from_utf8(rest) {
            Ok(s) => (s.to_string(), Encoding::Utf8, true),
            Err(_) => (windows_1252(bytes), Encoding::Windows1252, false),
        });
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
    Ok(match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), Encoding::Utf8, false),
        Err(_) => (windows_1252(bytes), Encoding::Windows1252, false),
    })
}

fn windows_1252(bytes: &[u8]) -> String {
    encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes).0.into_owned()
}

fn utf16(bytes: &[u8], read: fn([u8; 2]) -> u16) -> Result<String, Unsupported> {
    if !bytes.len().is_multiple_of(2) {
        return Err(Unsupported::Binary);
    }
    let units = bytes.as_chunks::<2>().0.iter().map(|c| read(*c));
    char::decode_utf16(units).collect::<Result<String, _>>().map_err(|_| Unsupported::Binary)
}

/// The bytes for `text`, or which characters the encoding has no byte for.
pub fn encode(text: &str, encoding: Encoding, bom: bool) -> Result<Vec<u8>, String> {
    Ok(match encoding {
        Encoding::Windows1252 => {
            let missing: String = text.chars().filter(|c| encoding_rs::WINDOWS_1252.encode(&c.to_string()).2).take(5).collect();
            if !missing.is_empty() {
                return Err(format!("Windows-1252 has no byte for {missing:?}. Save as UTF-8 instead, from the encoding menu."));
            }
            encoding_rs::WINDOWS_1252.encode(text).0.into_owned()
        }
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
    })
}

impl Encoding {
    pub fn name(self, bom: bool) -> &'static str {
        match (self, bom) {
            (Encoding::Utf8, false) => "UTF-8",
            (Encoding::Utf8, true) => "UTF-8 with BOM",
            (Encoding::Utf16Le, _) => "UTF-16 LE",
            (Encoding::Utf16Be, _) => "UTF-16 BE",
            (Encoding::Windows1252, _) => "Windows-1252",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_encoding_comes_back_byte_for_byte() {
        let samples: [&[u8]; 7] = [b"plain\r\nascii\n", b"\xEF\xBB\xBFwith bom", b"\xFF\xFEh\0i\0", b"\xFE\xFF\0h\0i", "çğ 中文 🦀".as_bytes(), b"caf\xe9 \x80 \x81\x8d\x9d", b"\xEF\xBB\xBFbad \xff"];
        for bytes in samples {
            let (text, enc, bom) = decode(bytes).unwrap();
            assert_eq!(encode(&text, enc, bom).unwrap(), bytes, "{}", enc.name(bom));
        }
    }

    #[test]
    fn what_cannot_be_edited_says_why() {
        assert_eq!(decode(b"PNG\0\0\x01"), Err(Unsupported::Binary));
        assert_eq!(decode(b"caf\xe9").unwrap(), ("café".to_string(), Encoding::Windows1252, false));
        assert!(encode("café 中", Encoding::Windows1252, false).unwrap_err().contains("中"));
        assert_eq!(decode(b"\xFF\xFEodd"), Err(Unsupported::Binary));
    }
}
