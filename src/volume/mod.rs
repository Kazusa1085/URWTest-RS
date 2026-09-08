use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeKind {
    Removable,
    Fixed,
    Network,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeInfo {
    pub mount_point: PathBuf,
    pub device: Option<String>,
    pub fs: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub kind: VolumeKind,
    pub label: Option<String>,
}

impl VolumeInfo {
    pub fn display_name(&self) -> String {
        match &self.label {
            Some(label) if !label.is_empty() => {
                format!("{} ({label})", self.mount_point.display())
            }
            _ => self.mount_point.display().to_string(),
        }
    }
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{available_space, list_volumes};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{available_space, list_volumes};

#[cfg(not(any(windows, target_os = "linux")))]
mod unsupported;
#[cfg(not(any(windows, target_os = "linux")))]
pub use unsupported::{available_space, list_volumes};

/// Normalize a user-supplied volume path.
///
/// Windows accepts `E:` and turns it into `E:\`; Unix paths are left mostly
/// untouched and canonicalized by `find_volume`.
pub fn normalize_target_path(target: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let text = target.to_string_lossy();
        if text.len() == 2 && text.ends_with(':') {
            return PathBuf::from(format!("{text}\\"));
        }
    }
    target.to_path_buf()
}

/// Find the mounted volume whose root is exactly `target`.
///
/// This deliberately does not walk up from subdirectories: the engine only
/// writes into a mounted volume root, never into arbitrary directories or raw
/// devices.
pub fn find_volume(target: &Path) -> Option<VolumeInfo> {
    let target = normalize_target_path(target);
    let target = normalize_existing_path(&target);
    list_volumes()
        .into_iter()
        .find(|volume| normalize_existing_path(&volume.mount_point) == target)
}

fn normalize_existing_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Return free space for the mounted volume containing `target`.
pub fn free_space(target: &Path) -> Result<u64> {
    let target = normalize_target_path(target);
    if !target.exists() {
        bail!("target does not exist: {}", target.display());
    }
    available_space(&target)
}
