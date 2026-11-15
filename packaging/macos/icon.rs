//! Writes the logo as a PNG for the macOS app icon: `icon OUT.png SIZE`. Built by build-dmg.sh with
//! plain `rustc`, from the same geometry the window and the Windows and Linux icons are drawn from.
//! The gem fills Apple's icon grid (824 of 1024) with clear space around it, as the Dock expects.
//! No dependencies: the PNG is written with stored (uncompressed) deflate blocks.

#[path = "../../crates/gitem/src/logo.rs"]
#[allow(dead_code)]
mod logo;

fn crc32(bytes: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for b in bytes {
        c ^= *b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

fn png(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(rgba.len() + size as usize);
    for row in rgba.chunks((size * 4) as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for x in &raw {
        a = (a + *x as u32) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push((i + 1 == blocks.len()) as u8);
        z.extend_from_slice(&(block.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&size.to_be_bytes());
    header.extend_from_slice(&size.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(out), Some(size)) = (args.get(1), args.get(2).and_then(|s| s.parse::<u32>().ok())) else {
        eprintln!("usage: icon OUT.png SIZE");
        std::process::exit(2);
    };
    let inner = size * 824 / 1024;
    let gem = logo::rgba(inner);
    let offset = (size - inner) / 2;
    let mut canvas = vec![0u8; (size * size * 4) as usize];
    for y in 0..inner {
        let from = (y * inner * 4) as usize;
        let to = (((y + offset) * size + offset) * 4) as usize;
        canvas[to..to + (inner * 4) as usize].copy_from_slice(&gem[from..from + (inner * 4) as usize]);
    }
    std::fs::write(out, png(size, &canvas)).unwrap_or_else(|e| {
        eprintln!("{out}: {e}");
        std::process::exit(1);
    });
}
