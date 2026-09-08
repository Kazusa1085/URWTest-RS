use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

use crate::console::{Console, format_bytes};
use crate::manifest::{self, Manifest, ManifestStatus, TestFile};
use crate::types::{CleanupMode, VerifyMode};
use crate::volume::{self, VolumeInfo};

/// Chunk size used when the filesystem has a small per-file limit (FAT32).
pub const FAT_CHUNK_SIZE: u64 = 2 * 1024 * 1024 * 1024;

/// Fallback chunk size used if a filesystem rejects a larger file.
pub const FALLBACK_CHUNK_SIZE: u64 = 1024 * 1024 * 1024;

/// I/O buffer size. Files are written and verified in 1 MiB blocks.
const IO_BLOCK_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub target: PathBuf,
    pub volume: VolumeInfo,
    pub passes: u32,
    pub verify: VerifyMode,
    pub delay: Option<u64>,
    pub stop_on_fail: bool,
    pub cleanup: CleanupMode,
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct VerifyOptions {
    pub target: PathBuf,
    pub stop_on_fail: bool,
    pub cleanup: CleanupMode,
}

#[derive(Debug, Default, Serialize)]
pub struct TestReport {
    pub target: String,
    pub passes_total: u32,
    pub passes_completed: u32,
    pub files_total: u32,
    pub files_passed: u32,
    pub files_failed: u32,
    pub failed_files: Vec<String>,
    pub expected_total: u64,
    pub actual_total: u64,
    pub capacity_ok: bool,
    pub pending_verify: bool,
    pub success: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteStopReason {
    Complete,
    DiskFull,
    FileTooLarge,
}

#[derive(Debug, Clone, Copy)]
struct WriteOutcome {
    actual_size: u64,
    stop_reason: WriteStopReason,
}

struct WritePassResult {
    files: Vec<TestFile>,
    expected_total: u64,
    actual_total: u64,
    capacity_ok: bool,
}

impl TestReport {
    fn new(target: &Path) -> Self {
        Self {
            target: target.display().to_string(),
            ..Self::default()
        }
    }
}

pub fn run_test(options: &RunOptions, console: &Console) -> Result<TestReport> {
    let target = &options.target;
    let mut manifest = prepare_manifest(options)?;
    let mut report = TestReport::new(target);
    report.passes_total = manifest.total_passes;
    report.passes_completed = manifest.completed_passes;

    if options.verify == VerifyMode::Later {
        if manifest.completed_passes >= manifest.total_passes {
            bail!("all passes on this volume are already complete");
        }

        let pass = manifest.completed_passes + 1;
        manifest.current_pass = pass;
        manifest.status = ManifestStatus::Writing;
        manifest.files.clear();
        manifest.last_error = None;
        manifest::save(target, &mut manifest)?;

        console.info(&format!(
            "Pass {pass}/{}: writing test data (verification deferred)",
            manifest.total_passes
        ));
        let WritePassResult {
            files,
            expected_total,
            actual_total,
            capacity_ok,
        } = write_pass(options, pass, console)?;

        report.files_total += files.len() as u32;
        report.expected_total = expected_total;
        report.actual_total = actual_total;
        report.capacity_ok = capacity_ok;

        manifest.files = files;
        manifest.expected_total = expected_total;
        manifest.actual_total = actual_total;
        manifest.capacity_ok = capacity_ok;
        manifest.status = ManifestStatus::PendingVerify;
        manifest::save(target, &mut manifest)?;

        report.pending_verify = true;
        report.success = capacity_ok;
        return Ok(report);
    }

    while manifest.completed_passes < manifest.total_passes {
        let pass = manifest.completed_passes + 1;
        manifest.current_pass = pass;
        manifest.status = ManifestStatus::Writing;
        manifest.files.clear();
        manifest.last_error = None;
        manifest::save(target, &mut manifest)?;

        console.info(&format!(
            "Pass {pass}/{}: writing test data",
            manifest.total_passes
        ));
        let WritePassResult {
            files,
            expected_total,
            actual_total,
            capacity_ok,
        } = write_pass(options, pass, console)?;

        report.files_total += files.len() as u32;
        report.expected_total = expected_total;
        report.actual_total = actual_total;
        report.capacity_ok = capacity_ok;

        manifest.files = files;
        manifest.expected_total = expected_total;
        manifest.actual_total = actual_total;
        manifest.capacity_ok = capacity_ok;
        manifest.status = ManifestStatus::PendingVerify;
        manifest::save(target, &mut manifest)?;

        if options.verify == VerifyMode::Delay {
            let seconds = options.delay.unwrap_or(0);
            if seconds > 0 {
                console.info(&format!(
                    "Waiting {seconds} second(s) before verification..."
                ));
                std::thread::sleep(Duration::from_secs(seconds));
            }
        }

        let data_ok = verify_pass(
            target,
            options.stop_on_fail,
            &mut manifest,
            console,
            &mut report,
        )?;
        let all_ok = data_ok && capacity_ok;

        if !all_ok {
            manifest.status = ManifestStatus::Failed;
            manifest.last_error = Some(if capacity_ok {
                "verification failed".to_string()
            } else {
                "capacity shortfall".to_string()
            });
            manifest::save(target, &mut manifest)?;
            cleanup_after_failure(target, options.cleanup)?;
            report.success = false;
            return Ok(report);
        }

        manifest.completed_passes = pass;
        report.passes_completed = pass;

        if pass >= manifest.total_passes {
            manifest.status = ManifestStatus::Completed;
            manifest::save(target, &mut manifest)?;
            cleanup_after_success(target, options.cleanup)?;
            report.success = true;
            return Ok(report);
        }

        manifest.status = ManifestStatus::PassComplete;
        manifest::save(target, &mut manifest)?;
        manifest::remove_test_files(target)?;
    }

    report.success = true;
    Ok(report)
}

pub fn verify_test(options: &VerifyOptions, console: &Console) -> Result<TestReport> {
    let target = &options.target;
    let mut manifest = manifest::load(target)?
        .with_context(|| format!("no test data found on {}", target.display()))?;

    if manifest.status != ManifestStatus::PendingVerify {
        bail!(
            "no pending verification data on {} (status: {:?})",
            target.display(),
            manifest.status
        );
    }

    manifest.status = ManifestStatus::Verifying;
    manifest.last_error = None;
    manifest::save(target, &mut manifest)?;

    let mut report = TestReport::new(target);
    report.passes_total = manifest.total_passes;
    report.passes_completed = manifest.completed_passes;
    report.files_total = manifest.files.len() as u32;
    report.expected_total = manifest.expected_total;
    report.actual_total = manifest.actual_total;
    report.capacity_ok = manifest.capacity_ok;

    let data_ok = verify_pass(
        target,
        options.stop_on_fail,
        &mut manifest,
        console,
        &mut report,
    )?;
    let all_ok = data_ok && manifest.capacity_ok;

    if !all_ok {
        manifest.status = ManifestStatus::Failed;
        manifest.last_error = Some(if manifest.capacity_ok {
            "verification failed".to_string()
        } else {
            "capacity shortfall".to_string()
        });
        manifest::save(target, &mut manifest)?;
        cleanup_after_failure(target, options.cleanup)?;
        report.success = false;
        return Ok(report);
    }

    manifest.completed_passes = manifest.current_pass;
    report.passes_completed = manifest.completed_passes;

    if manifest.completed_passes >= manifest.total_passes {
        manifest.status = ManifestStatus::Completed;
        manifest::save(target, &mut manifest)?;
        cleanup_after_success(target, options.cleanup)?;
        report.success = true;
    } else {
        manifest.status = ManifestStatus::PassComplete;
        manifest::save(target, &mut manifest)?;
        // Free space for the next pass. The manifest stays in place so a later
        // `run` can continue with the next pass.
        manifest::remove_test_files(target)?;
        report.success = true;
    }

    Ok(report)
}

fn prepare_manifest(options: &RunOptions) -> Result<Manifest> {
    let target = &options.target;
    let total_passes = options.passes.max(1);

    match manifest::load(target)? {
        None => {
            let test_dir = manifest::test_dir(target);
            if test_dir.exists() {
                if options.force {
                    manifest::remove_test_dir(target)?;
                } else {
                    bail!(
                        "test directory already exists at {}; use --force to discard it",
                        test_dir.display()
                    );
                }
            }
            Ok(Manifest::new(
                target,
                total_passes,
                options.stop_on_fail,
                options.cleanup,
            ))
        }
        Some(mut manifest) => match manifest.status {
            ManifestStatus::PassComplete => {
                if manifest.completed_passes >= manifest.total_passes {
                    bail!("all passes on this volume are already complete");
                }
                manifest::remove_test_files(target)?;
                manifest.stop_on_fail = options.stop_on_fail;
                manifest.cleanup = options.cleanup;
                Ok(manifest)
            }
            ManifestStatus::PendingVerify => {
                bail!(
                    "pending verification data exists on {}; run `urwtest-rs verify --target {}` first, or use --force",
                    target.display(),
                    target.display()
                );
            }
            ManifestStatus::Completed => {
                if !options.force {
                    bail!(
                        "a completed test already exists on {}; use --force to start over",
                        target.display()
                    );
                }
                manifest::remove_test_dir(target)?;
                Ok(Manifest::new(
                    target,
                    total_passes,
                    options.stop_on_fail,
                    options.cleanup,
                ))
            }
            ManifestStatus::Failed => {
                if !options.force {
                    bail!(
                        "a failed test state exists on {}; use --force to start over",
                        target.display()
                    );
                }
                manifest::remove_test_dir(target)?;
                Ok(Manifest::new(
                    target,
                    total_passes,
                    options.stop_on_fail,
                    options.cleanup,
                ))
            }
            ManifestStatus::Writing | ManifestStatus::Verifying => {
                if !options.force {
                    bail!(
                        "an interrupted test state exists on {}; use --force to start over",
                        target.display()
                    );
                }
                manifest::remove_test_dir(target)?;
                Ok(Manifest::new(
                    target,
                    total_passes,
                    options.stop_on_fail,
                    options.cleanup,
                ))
            }
        },
    }
}

fn write_pass(options: &RunOptions, pass: u32, console: &Console) -> Result<WritePassResult> {
    let target = &options.target;
    let test_dir = manifest::test_dir(target);
    fs::create_dir_all(&test_dir)
        .with_context(|| format!("failed to create test directory {}", test_dir.display()))?;

    let expected_total = volume::free_space(target)?;
    if expected_total < 1024 * 1024 {
        bail!(
            "not enough free space on {} to start a test ({} available)",
            target.display(),
            format_bytes(expected_total)
        );
    }

    let mut chunk_size = choose_chunk_size(&options.volume.fs, expected_total);
    let mut files = Vec::new();
    let mut actual_total = 0u64;
    let mut file_index = 0u32;

    while actual_total < expected_total {
        let remaining = expected_total - actual_total;
        let planned_size = remaining.min(chunk_size).max(1);
        let name = format!("urwtest_p{pass:03}_{file_index:04}.bin");
        let path = test_dir.join(&name);
        let seed = seed_for(pass, file_index);

        console.info(&format!(
            "  writing {name} (up to {})",
            format_bytes(planned_size)
        ));
        let outcome = write_file(&path, planned_size, seed)?;
        actual_total = actual_total.saturating_add(outcome.actual_size);

        files.push(TestFile {
            name,
            pass,
            expected_size: planned_size,
            actual_size: outcome.actual_size,
            seed,
            verified: false,
            passed: None,
        });

        match outcome.stop_reason {
            WriteStopReason::Complete => {
                file_index = file_index.saturating_add(1);
            }
            WriteStopReason::DiskFull => break,
            WriteStopReason::FileTooLarge => {
                if chunk_size > FALLBACK_CHUNK_SIZE {
                    console.warn(&format!(
                        "  filesystem rejected a {} file; falling back to {} chunks",
                        format_bytes(chunk_size),
                        format_bytes(FALLBACK_CHUNK_SIZE)
                    ));
                    chunk_size = FALLBACK_CHUNK_SIZE;
                    file_index = file_index.saturating_add(1);
                } else {
                    bail!(
                        "filesystem rejected a {} file as too large",
                        format_bytes(FALLBACK_CHUNK_SIZE)
                    );
                }
            }
        }
    }

    let shortfall = expected_total.saturating_sub(actual_total);
    let tolerance = std::cmp::max(64 * 1024 * 1024, expected_total / 100);
    let capacity_ok = shortfall <= tolerance;
    if !capacity_ok {
        console.fail(&format!(
            "  capacity shortfall: wrote {} of {}",
            format_bytes(actual_total),
            format_bytes(expected_total)
        ));
    }

    Ok(WritePassResult {
        files,
        expected_total,
        actual_total,
        capacity_ok,
    })
}

fn write_file(path: &Path, max_size: u64, seed: u64) -> Result<WriteOutcome> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if is_disk_full(&error) => {
            return Ok(WriteOutcome {
                actual_size: 0,
                stop_reason: WriteStopReason::DiskFull,
            });
        }
        Err(error) if is_file_too_large(&error) => {
            return Ok(WriteOutcome {
                actual_size: 0,
                stop_reason: WriteStopReason::FileTooLarge,
            });
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to create {}", path.display()));
        }
    };

    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut buffer = vec![0u8; IO_BLOCK_SIZE];
    let mut written = 0u64;

    while written < max_size {
        let remaining = max_size - written;
        let block_size = remaining.min(IO_BLOCK_SIZE as u64) as usize;
        rng.fill_bytes(&mut buffer[..block_size]);

        let mut offset = 0usize;
        while offset < block_size {
            match file.write(&buffer[offset..block_size]) {
                Ok(0) => {
                    return Err(
                        io::Error::new(io::ErrorKind::WriteZero, "write returned zero").into(),
                    );
                }
                Ok(count) => offset += count,
                Err(error) if is_disk_full(&error) => {
                    let actual_size = written + offset as u64;
                    let _ = file.flush();
                    return Ok(WriteOutcome {
                        actual_size,
                        stop_reason: WriteStopReason::DiskFull,
                    });
                }
                Err(error) if is_file_too_large(&error) => {
                    let actual_size = written + offset as u64;
                    let _ = file.flush();
                    return Ok(WriteOutcome {
                        actual_size,
                        stop_reason: WriteStopReason::FileTooLarge,
                    });
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to write {}", path.display()));
                }
            }
        }

        written += block_size as u64;
    }

    if let Err(error) = file.flush() {
        if is_disk_full(&error) {
            return Ok(WriteOutcome {
                actual_size: written,
                stop_reason: WriteStopReason::DiskFull,
            });
        }
        if is_file_too_large(&error) {
            return Ok(WriteOutcome {
                actual_size: written,
                stop_reason: WriteStopReason::FileTooLarge,
            });
        }
        return Err(error).with_context(|| format!("failed to flush {}", path.display()));
    }

    if let Err(error) = file.sync_all() {
        if is_disk_full(&error) {
            return Ok(WriteOutcome {
                actual_size: written,
                stop_reason: WriteStopReason::DiskFull,
            });
        }
        if is_file_too_large(&error) {
            return Ok(WriteOutcome {
                actual_size: written,
                stop_reason: WriteStopReason::FileTooLarge,
            });
        }
        return Err(error).with_context(|| format!("failed to sync {}", path.display()));
    }

    Ok(WriteOutcome {
        actual_size: written,
        stop_reason: WriteStopReason::Complete,
    })
}

fn choose_chunk_size(filesystem: &str, expected_total: u64) -> u64 {
    let filesystem = filesystem.to_ascii_lowercase();
    let is_fat = filesystem.contains("fat32")
        || filesystem.contains("fat16")
        || filesystem == "vfat"
        || filesystem == "fat"
        || filesystem == "msdos";
    if is_fat {
        FAT_CHUNK_SIZE
    } else {
        expected_total.max(1)
    }
}

fn verify_pass(
    target: &Path,
    stop_on_fail: bool,
    manifest: &mut Manifest,
    console: &Console,
    report: &mut TestReport,
) -> Result<bool> {
    let test_dir = manifest::test_dir(target);
    let mut all_ok = true;

    for file in &mut manifest.files {
        let path = test_dir.join(&file.name);
        let (ok, reason) = match verify_file(&path, file) {
            Ok(()) => (true, None),
            Err(reason) => (false, Some(reason)),
        };

        file.verified = true;
        file.passed = Some(ok);

        if ok {
            report.files_passed += 1;
            console.ok(&format!("OK   {}", file.name));
        } else {
            report.files_failed += 1;
            report.failed_files.push(file.name.clone());
            all_ok = false;
            console.fail(&format!("FAIL {}", file.name));
            if let Some(reason) = reason {
                console.fail(&format!("     {reason}"));
            }
            if stop_on_fail {
                break;
            }
        }
    }

    Ok(all_ok)
}

fn verify_file(path: &Path, file: &TestFile) -> std::result::Result<(), String> {
    let metadata = fs::metadata(path).map_err(|error| format!("cannot stat file: {error}"))?;
    if metadata.len() != file.actual_size {
        return Err(format!(
            "file size changed: expected {}, found {}",
            format_bytes(file.actual_size),
            format_bytes(metadata.len())
        ));
    }

    let mut reader = File::open(path).map_err(|error| format!("cannot open file: {error}"))?;
    let mut rng = ChaCha8Rng::seed_from_u64(file.seed);
    let mut expected = vec![0u8; IO_BLOCK_SIZE];
    let mut actual = vec![0u8; IO_BLOCK_SIZE];
    let mut offset = 0u64;

    while offset < file.actual_size {
        let remaining = file.actual_size - offset;
        let block_size = remaining.min(IO_BLOCK_SIZE as u64) as usize;
        rng.fill_bytes(&mut expected[..block_size]);

        reader
            .read_exact(&mut actual[..block_size])
            .map_err(|error| format!("read error at {}: {error}", format_bytes(offset)))?;

        if actual[..block_size] != expected[..block_size] {
            return Err(format!("data mismatch at {}", format_bytes(offset)));
        }

        offset += block_size as u64;
    }

    Ok(())
}

fn seed_for(pass: u32, file_index: u32) -> u64 {
    // Small deterministic FNV-1a variant over pass and file index. The exact
    // seed values are not externally meaningful; they only need to be stable
    // across write and verify runs.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in pass
        .to_le_bytes()
        .iter()
        .chain(file_index.to_le_bytes().iter())
    {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn is_disk_full(error: &io::Error) -> bool {
    // ENOSPC on Unix; ERROR_HANDLE_DISK_FULL / ERROR_DISK_FULL on Windows.
    matches!(error.raw_os_error(), Some(28) | Some(39) | Some(112))
}

fn is_file_too_large(error: &io::Error) -> bool {
    // EFBIG on Unix; ERROR_FILE_TOO_LARGE / ERROR_FILE_SYSTEM_LIMITATION on
    // Windows.
    matches!(error.raw_os_error(), Some(27) | Some(223) | Some(665))
}

fn cleanup_after_success(target: &Path, cleanup: CleanupMode) -> Result<()> {
    match cleanup {
        CleanupMode::Always | CleanupMode::OnSuccess => manifest::remove_test_dir(target),
        CleanupMode::Never => Ok(()),
    }
}

fn cleanup_after_failure(target: &Path, cleanup: CleanupMode) -> Result<()> {
    match cleanup {
        CleanupMode::Always => manifest::remove_test_dir(target),
        CleanupMode::OnSuccess | CleanupMode::Never => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, SeekFrom};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "urwtest-rs-{label}-{}-{nanos}.bin",
            std::process::id()
        ))
    }

    fn record_for(name: &str, expected_size: u64, actual_size: u64, seed: u64) -> TestFile {
        TestFile {
            name: name.to_string(),
            pass: 1,
            expected_size,
            actual_size,
            seed,
            verified: false,
            passed: None,
        }
    }

    #[test]
    fn write_and_verify_round_trip() {
        let path = temporary_path("round-trip");
        let expected_size = IO_BLOCK_SIZE as u64 + 12_345;
        let seed = 0x1234_5678_9abc_def0;

        let outcome = write_file(&path, expected_size, seed).unwrap();
        assert_eq!(outcome.actual_size, expected_size);
        assert_eq!(outcome.stop_reason, WriteStopReason::Complete);

        let record = record_for("round-trip.bin", expected_size, outcome.actual_size, seed);
        verify_file(&path, &record).unwrap();

        let _ = fs::remove_file(path);
    }

    #[test]
    fn corruption_is_detected() {
        let path = temporary_path("corruption");
        let expected_size = IO_BLOCK_SIZE as u64 + 12_345;
        let seed = 0x0fed_cba9_8765_4321;

        let outcome = write_file(&path, expected_size, seed).unwrap();
        assert_eq!(outcome.actual_size, expected_size);

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.seek(SeekFrom::Start(10)).unwrap();
        let mut original = [0u8; 1];
        file.read_exact(&mut original).unwrap();
        let replacement = if original[0] == 0xff { 0x00 } else { 0xff };
        file.seek(SeekFrom::Start(10)).unwrap();
        file.write_all(&[replacement]).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let record = record_for("corruption.bin", expected_size, outcome.actual_size, seed);
        assert!(verify_file(&path, &record).is_err());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn partial_file_data_can_be_verified() {
        let path = temporary_path("partial");
        let planned_size = 1024 * 1024;
        let actual_size = planned_size / 2;
        let seed = 42;

        let outcome = write_file(&path, actual_size, seed).unwrap();
        assert_eq!(outcome.actual_size, actual_size);

        // Capacity shortfall is tracked globally by write_pass, so a partial
        // final file should still verify its actual contents successfully.
        let record = record_for("partial.bin", planned_size, actual_size, seed);
        verify_file(&path, &record).unwrap();

        let _ = fs::remove_file(path);
    }

    #[test]
    fn fat_filesystems_use_2gib_chunks() {
        let total = 100 * 1024 * 1024 * 1024;
        assert_eq!(choose_chunk_size("FAT32", total), FAT_CHUNK_SIZE);
        assert_eq!(choose_chunk_size("vfat", total), FAT_CHUNK_SIZE);
        assert_eq!(choose_chunk_size("NTFS", total), total);
        assert_eq!(choose_chunk_size("exFAT", total), total);
    }
}
