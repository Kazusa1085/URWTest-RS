use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::types::CleanupMode;

pub const TEST_DIR: &str = ".urwtest";
pub const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestStatus {
    Writing,
    PendingVerify,
    Verifying,
    PassComplete,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFile {
    pub name: String,
    pub pass: u32,
    pub expected_size: u64,
    pub actual_size: u64,
    pub seed: u64,
    pub verified: bool,
    pub passed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub target: String,
    pub created_unix: u64,
    pub updated_unix: u64,
    pub total_passes: u32,
    pub completed_passes: u32,
    pub current_pass: u32,
    pub status: ManifestStatus,
    pub stop_on_fail: bool,
    pub cleanup: CleanupMode,
    pub expected_total: u64,
    pub actual_total: u64,
    pub capacity_ok: bool,
    pub files: Vec<TestFile>,
    pub last_error: Option<String>,
}

impl Manifest {
    pub fn new(target: &Path, total_passes: u32, stop_on_fail: bool, cleanup: CleanupMode) -> Self {
        let now = unix_now();
        Self {
            version: 1,
            target: target.display().to_string(),
            created_unix: now,
            updated_unix: now,
            total_passes,
            completed_passes: 0,
            current_pass: 1,
            status: ManifestStatus::Writing,
            stop_on_fail,
            cleanup,
            expected_total: 0,
            actual_total: 0,
            capacity_ok: true,
            files: Vec::new(),
            last_error: None,
        }
    }

    pub fn touch(&mut self) {
        self.updated_unix = unix_now();
    }
}

pub fn test_dir(target: &Path) -> PathBuf {
    target.join(TEST_DIR)
}

pub fn manifest_path(target: &Path) -> PathBuf {
    test_dir(target).join(MANIFEST_FILE)
}

pub fn load(target: &Path) -> Result<Option<Manifest>> {
    let path = manifest_path(target);
    if !path.exists() {
        return Ok(None);
    }

    let data = fs::read_to_string(&path)
        .with_context(|| format!("failed to read manifest {}", path.display()))?;
    let manifest = serde_json::from_str(&data)
        .with_context(|| format!("failed to parse manifest {}", path.display()))?;
    Ok(Some(manifest))
}

pub fn save(target: &Path, manifest: &mut Manifest) -> Result<()> {
    manifest.touch();
    let dir = test_dir(target);
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create test directory {}", dir.display()))?;

    let path = manifest_path(target);
    let temporary = dir.join("manifest.json.tmp");
    let data = serde_json::to_string_pretty(manifest).context("failed to serialize manifest")?;
    fs::write(&temporary, data)
        .with_context(|| format!("failed to write manifest {}", temporary.display()))?;

    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("failed to replace manifest {}", path.display()))?;
    }
    fs::rename(&temporary, &path)
        .with_context(|| format!("failed to install manifest {}", path.display()))?;
    Ok(())
}

pub fn remove_test_files(target: &Path) -> Result<()> {
    let dir = test_dir(target);
    if !dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(&dir)
        .with_context(|| format!("failed to list test directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.file_name().and_then(|name| name.to_str()) != Some(MANIFEST_FILE)
        {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove test file {}", path.display()))?;
        }
    }
    Ok(())
}

pub fn remove_test_dir(target: &Path) -> Result<()> {
    let dir = test_dir(target);
    if dir.exists() {
        fs::remove_dir_all(&dir)
            .with_context(|| format!("failed to remove test directory {}", dir.display()))?;
    }
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
