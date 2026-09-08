use std::path::Path;

use anyhow::{Result, bail};

use super::VolumeInfo;

pub fn list_volumes() -> Vec<VolumeInfo> {
    Vec::new()
}

pub fn available_space(_path: &Path) -> Result<u64> {
    bail!("this platform is not implemented yet; Windows and Linux are supported")
}
