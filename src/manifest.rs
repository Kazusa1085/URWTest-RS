use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::types::CleanupMode;
use crate::volume::VolumeInfo;

pub const TEST_FILE_PREFIX: &str = "urwtest_rs_";
pub const TEST_FILE_SUFFIX: &str = ".bin";
pub const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateStatus {
    Writing,
    PendingVerify,
    Verifying,
    PassComplete,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestState {
    pub version: u32,
    pub target: String,
    pub volume_key: String,
    pub volume_fs: String,
    pub volume_label: Option<String>,
    pub volume_device: Option<String>,
    pub volume_total_bytes: u64,
    pub created_unix: u64,
    pub updated_unix: u64,
    pub total_passes: u32,
    pub completed_passes: u32,
    pub current_pass: u32,
    pub status: StateStatus,
    pub stop_on_fail: bool,
    pub cleanup: CleanupMode,
    pub expected_total: u64,
    pub actual_total: u64,
    pub capacity_ok: bool,
    pub last_error: Option<String>,
}

impl TestState {
    pub fn new(
        volume: &VolumeInfo,
        total_passes: u32,
        stop_on_fail: bool,
        cleanup: CleanupMode,
    ) -> Self {
        let now = unix_now();
        Self {
            version: STATE_VERSION,
            target: volume.mount_point.display().to_string(),
            volume_key: volume_key(volume),
            volume_fs: volume.fs.clone(),
            volume_label: volume.label.clone(),
            volume_device: volume.device.clone(),
            volume_total_bytes: volume.total_bytes,
            created_unix: now,
            updated_unix: now,
            total_passes: total_passes.max(1),
            completed_passes: 0,
            current_pass: 1,
            status: StateStatus::Writing,
            stop_on_fail,
            cleanup,
            expected_total: 0,
            actual_total: 0,
            capacity_ok: true,
            last_error: None,
        }
    }

    pub fn touch(&mut self) {
        self.updated_unix = unix_now();
    }
}

#[derive(Debug, Clone)]
pub struct TestFile {
    pub name: String,
    pub pass: u32,
    pub total_passes: u32,
    pub index: u32,
    pub seed: u64,
    pub actual_size: u64,
}

pub fn make_test_file_name(pass: u32, total_passes: u32, index: u32, seed: u64) -> String {
    format!("{TEST_FILE_PREFIX}p{pass:03}of{total_passes:03}_f{index:05}_s{seed}{TEST_FILE_SUFFIX}")
}

pub fn parse_test_file_name(name: &str) -> Option<TestFile> {
    let stem = name
        .strip_prefix(TEST_FILE_PREFIX)?
        .strip_suffix(TEST_FILE_SUFFIX)?;
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.len() != 3 {
        return None;
    }

    let (pass, total_passes) = parts[0].strip_prefix('p')?.split_once("of")?;
    let index = parts[1].strip_prefix('f')?;
    let seed = parts[2].strip_prefix('s')?;

    Some(TestFile {
        name: name.to_string(),
        pass: pass.parse().ok()?,
        total_passes: total_passes.parse().ok()?,
        index: index.parse().ok()?,
        seed: seed.parse().ok()?,
        actual_size: 0,
    })
}

pub fn scan_test_files(target: &Path) -> Result<Vec<TestFile>> {
    let mut files = Vec::new();
    if !target.exists() {
        return Ok(files);
    }

    for entry in fs::read_dir(target)
        .with_context(|| format!("failed to list target directory {}", target.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(mut file) = parse_test_file_name(name) {
            file.actual_size = entry.metadata()?.len();
            files.push(file);
        }
    }

    files.sort_by_key(|file| (file.pass, file.index));
    Ok(files)
}

pub fn remove_test_files(target: &Path) -> Result<()> {
    if !target.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(target)
        .with_context(|| format!("failed to list target directory {}", target.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if parse_test_file_name(name).is_some() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove test file {}", path.display()))?;
        }
    }

    Ok(())
}

pub fn load_state(volume: &VolumeInfo) -> Result<Option<TestState>> {
    let path = state_path(volume);
    if path.exists() {
        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read state file {}", path.display()))?;
        let state = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse state file {}", path.display()))?;
        return Ok(Some(state));
    }

    find_matching_state(volume)
}

pub fn save_state(volume: &VolumeInfo, state: &mut TestState) -> Result<()> {
    state.touch();
    let directory = state_dir();
    fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create state directory {}", directory.display()))?;

    let path = state_path(volume);
    let temporary = directory.join("state.json.tmp");
    let data = serde_json::to_string_pretty(state).context("failed to serialize test state")?;
    fs::write(&temporary, data)
        .with_context(|| format!("failed to write state file {}", temporary.display()))?;

    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("failed to replace state file {}", path.display()))?;
    }
    fs::rename(&temporary, &path)
        .with_context(|| format!("failed to install state file {}", path.display()))?;
    Ok(())
}

pub fn remove_state(volume: &VolumeInfo) -> Result<()> {
    let path = state_path(volume);
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("failed to remove state file {}", path.display()))?;
    }
    Ok(())
}

pub fn state_path(volume: &VolumeInfo) -> PathBuf {
    state_dir().join(format!("{}.json", volume_key(volume)))
}

fn find_matching_state(volume: &VolumeInfo) -> Result<Option<TestState>> {
    let directory = state_dir();
    if !directory.exists() {
        return Ok(None);
    }

    for entry in fs::read_dir(&directory)
        .with_context(|| format!("failed to list state directory {}", directory.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Ok(data) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(state) = serde_json::from_str::<TestState>(&data) else {
            continue;
        };
        if same_volume(&state, volume) {
            return Ok(Some(state));
        }
    }

    Ok(None)
}

fn same_volume(state: &TestState, volume: &VolumeInfo) -> bool {
    if state.volume_fs != volume.fs || state.volume_total_bytes != volume.total_bytes {
        return false;
    }

    let label_matches = match (&state.volume_label, &volume.label) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    };
    let device_matches = match (state.volume_device.as_deref(), volume.device.as_deref()) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    };

    label_matches && device_matches
}

fn volume_key(volume: &VolumeInfo) -> String {
    let mut material = String::new();
    material.push_str(&volume.fs);
    material.push('|');
    material.push_str(&volume.total_bytes.to_string());
    material.push('|');

    if let Some(label) = volume.label.as_deref().filter(|label| !label.is_empty()) {
        material.push_str("label:");
        material.push_str(label);
    } else if let Some(device) = volume.device.as_deref() {
        material.push_str("device:");
        material.push_str(device);
    } else {
        material.push_str("mount:");
        material.push_str(&volume.mount_point.display().to_string());
    }

    fnv1a_hex(material.as_bytes())
}

fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(windows)]
fn state_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("APPDATA") {
        return PathBuf::from(path).join("urwtest-rs");
    }
    if let Some(path) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(path).join("urwtest-rs");
    }
    std::env::temp_dir().join("urwtest-rs-state")
}

#[cfg(not(windows))]
fn state_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(path).join("urwtest-rs");
    }
    if let Some(path) = std::env::var_os("HOME") {
        return PathBuf::from(path).join(".local/state/urwtest-rs");
    }
    std::env::temp_dir().join("urwtest-rs-state")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_name_round_trip() {
        let name = make_test_file_name(2, 5, 17, 0x1234_5678_9abc_def0);
        let parsed = parse_test_file_name(&name).expect("valid test file name");
        assert_eq!(parsed.pass, 2);
        assert_eq!(parsed.total_passes, 5);
        assert_eq!(parsed.index, 17);
        assert_eq!(parsed.seed, 0x1234_5678_9abc_def0);
        assert_eq!(parsed.name, name);
    }

    #[test]
    fn unrelated_file_name_is_ignored() {
        assert!(parse_test_file_name("photo.jpg").is_none());
        assert!(parse_test_file_name("urwtest_rs_p1of1_f00000_s1.txt").is_none());
    }

    #[test]
    fn scan_and_remove_only_matching_test_files() {
        let directory =
            std::env::temp_dir().join(format!("urwtest-rs-manifest-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();

        let test_name = make_test_file_name(1, 1, 0, 123);
        let keep_name = "keep-me.txt";
        fs::write(directory.join(&test_name), b"data").unwrap();
        fs::write(directory.join(keep_name), b"keep").unwrap();

        let files = scan_test_files(&directory).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, test_name);
        assert_eq!(files[0].actual_size, 4);

        remove_test_files(&directory).unwrap();
        assert!(!directory.join(&test_name).exists());
        assert!(directory.join(keep_name).exists());

        let _ = fs::remove_dir_all(&directory);
    }
}
