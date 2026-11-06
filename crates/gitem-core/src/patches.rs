//! Local patch library: gitem's equivalent of cloud patches. Patches are plain
//! files in the config dir and can be shared via a secret gist (see `forge`).
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::settings::config_dir;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchMeta {
    pub id: String,
    pub title: String,
    pub description: String,
    pub repo: String,
    pub created: i64,
    pub base: String,
    #[serde(default)]
    pub gist_url: String,
}

pub fn dir() -> PathBuf {
    config_dir().join("patches")
}

/// Bytes, because a patch of files in other encodings must keep theirs.
pub fn save(meta: &PatchMeta, content: &[u8]) -> std::io::Result<()> {
    let d = dir();
    std::fs::create_dir_all(&d)?;
    crate::util::write_atomic(&d.join(format!("{}.patch", meta.id)), content)?;
    save_meta(meta)
}

/// Updates a patch's details and leaves the patch itself alone.
pub fn save_meta(meta: &PatchMeta) -> std::io::Result<()> {
    crate::util::write_atomic(&dir().join(format!("{}.json", meta.id)), &serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?)
}

pub fn list() -> Vec<PatchMeta> {
    let mut v: Vec<PatchMeta> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    v.sort_by_key(|m: &PatchMeta| std::cmp::Reverse(m.created));
    v
}

pub fn patch_path(id: &str) -> PathBuf {
    dir().join(format!("{id}.patch"))
}

pub fn content(id: &str) -> std::io::Result<Vec<u8>> {
    std::fs::read(patch_path(id))
}

pub fn delete(id: &str) -> std::io::Result<()> {
    let d = dir();
    let _ = std::fs::remove_file(d.join(format!("{id}.json")));
    std::fs::remove_file(d.join(format!("{id}.patch")))
}
