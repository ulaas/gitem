//! Records which commit a binary was built from, for the About box and `--version`.
use std::path::Path;
use std::process::Command;

#[path = "src/logo.rs"]
mod logo;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// One entry of a compiled resource (.res) file.
fn resource(out: &mut Vec<u8>, kind: u16, name: u16, flags: u16, data: &[u8]) {
    let u16s = |o: &mut Vec<u8>, v: &[u16]| v.iter().for_each(|x| o.extend_from_slice(&x.to_le_bytes()));
    let u32s = |o: &mut Vec<u8>, v: &[u32]| v.iter().for_each(|x| o.extend_from_slice(&x.to_le_bytes()));
    u32s(out, &[data.len() as u32, 32]);
    u16s(out, &[0xffff, kind, 0xffff, name]);
    u32s(out, &[0]);
    let language = if data.is_empty() { 0 } else { 0x0409 };
    u16s(out, &[flags, language]);
    u32s(out, &[0, 0]);
    out.extend_from_slice(data);
    out.resize(out.len().next_multiple_of(4), 0);
}

/// The logo as an uncompressed 32-bit icon image: a bitmap header, the pixels
/// bottom-up in BGRA, and an empty transparency mask, since alpha does that job.
fn icon_image(size: u32) -> Vec<u8> {
    let rgba = logo::rgba(size);
    let mask_row = size.div_ceil(32) * 4;
    let pixels = size * size * 4;
    let mut out = Vec::new();
    for v in [40, size, size * 2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    for v in [0, pixels + mask_row * size, 0, 0, 0, 0] {
        out.extend_from_slice(&(v as u32).to_le_bytes());
    }
    for row in rgba.chunks((size * 4) as usize).rev() {
        for px in row.chunks(4) {
            out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
    }
    out.resize(out.len() + (mask_row * size) as usize, 0);
    out
}

/// Windows shows this icon for the file itself. Written directly, since the linker
/// takes a .res file as is and the Windows SDK's resource compiler may not be installed.
fn embed_icon() {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    const MOVEABLE_PURE_DISCARDABLE: u16 = 0x1030;
    const MOVEABLE_DISCARDABLE: u16 = 0x1010;
    let sizes = [16u32, 20, 24, 32, 40, 48, 64, 128, 256];
    let mut res = Vec::new();
    resource(&mut res, 0, 0, 0, &[]);
    let mut group: Vec<u8> = Vec::new();
    for v in [0u16, 1, sizes.len() as u16] {
        group.extend_from_slice(&v.to_le_bytes());
    }
    for (i, size) in sizes.iter().enumerate() {
        let image = icon_image(*size);
        let id = i as u16 + 1;
        resource(&mut res, RT_ICON, id, MOVEABLE_DISCARDABLE, &image);
        // In the directory, 256 is written as 0.
        let side = if *size >= 256 { 0 } else { *size as u8 };
        group.extend_from_slice(&[side, side, 0, 0]);
        group.extend_from_slice(&1u16.to_le_bytes());
        group.extend_from_slice(&32u16.to_le_bytes());
        group.extend_from_slice(&(image.len() as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());
    }
    resource(&mut res, RT_GROUP_ICON, 1, MOVEABLE_PURE_DISCARDABLE, &group);
    let path = Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("gitem-icon.res");
    std::fs::write(&path, res).expect("write the icon resource");
    println!("cargo:rustc-link-arg-bins={}", path.display());
}

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        embed_icon();
    }
    let sha = git(&["rev-parse", "--short=9", "HEAD"]).unwrap_or_default();
    let date = git(&["log", "-1", "--format=%cs"]).unwrap_or_default();
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    println!("cargo:rustc-env=GITEM_BUILD_SHA={sha}");
    println!("cargo:rustc-env=GITEM_BUILD_DATE={date}");
    println!("cargo:rustc-env=GITEM_BUILD_DIRTY={}", if dirty { "1" } else { "" });

    // Rerun on a new commit, on staging, and on any source edit.
    for name in ["HEAD", "index", "packed-refs"] {
        if let Some(p) = git(&["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    if let Some(p) = git(&["symbolic-ref", "-q", "HEAD"]).and_then(|r| git(&["rev-parse", "--git-path", &r])) {
        println!("cargo:rerun-if-changed={p}");
    }
    for p in ["../../crates", "../../Cargo.toml", "../../Cargo.lock"] {
        if Path::new(p).exists() {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
