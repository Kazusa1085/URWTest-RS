use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

use crate::console::{Console, format_bytes};
use crate::manifest::{self, StateStatus, TestFile, TestState};
use crate::types::{CleanupMode, VerifyMode};
use crate::volume::{self, VolumeInfo};

/// Chunk size used when the filesystem has a small per-file limit (FAT32).
pub const FAT_CHUNK_SIZE: u64 = 2 * 1024 * 1024 * 1024;

/// Fallback chunk size used if a filesystem rejects a larger file.
pub const FALLBACK_CHUNK_SIZE: u64 = 1024 * 1024 * 1024;

/// I/O buffer size. Files are written and verified in 1 MiB blocks.
const IO_BLOCK_SIZE: usize = 1024 * 1024;

const MIB: f64 = 1024.0 * 1024.0;

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
    pub progress: bool,
}

#[derive(Debug, Clone)]
pub struct VerifyOptions {
    pub target: PathBuf,
    pub volume: VolumeInfo,
    pub stop_on_fail: bool,
    pub cleanup: CleanupMode,
    pub progress: bool,
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
    pub write_seconds: f64,
    pub write_mib_per_sec: f64,
    pub read_seconds: f64,
    pub read_mib_per_sec: f64,
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
    seconds: f64,
}

struct VerifyPassResult {
    all_ok: bool,
    seconds: f64,
    bytes_verified: u64,
}

struct Progress {
    label: &'static str,
    total_bytes: u64,
    processed: u64,
    last_bytes: u64,
    start: Instant,
    last_report: Instant,
    enabled: bool,
}

impl Progress {
    fn new(label: &'static str, total_bytes: u64, enabled: bool) -> Self {
        let now = Instant::now();
        Self {
            label,
            total_bytes,
            processed: 0,
            last_bytes: 0,
            start: now,
            last_report: now,
            enabled,
        }
    }

    fn add(&mut self, bytes: u64) {
        self.processed = self.processed.saturating_add(bytes);
        if self.enabled && self.last_report.elapsed() >= Duration::from_secs(1) {
            self.print_update();
            self.last_bytes = self.processed;
            self.last_report = Instant::now();
        }
    }

    fn print_update(&self) {
        let interval = self.last_report.elapsed().as_secs_f64().max(0.001);
        let current = (self.processed.saturating_sub(self.last_bytes)) as f64 / interval / MIB;
        let average = self.processed as f64 / self.start.elapsed().as_secs_f64().max(0.001) / MIB;
        print!(
            "\r{}: {} / {} | current {:.1} MiB/s | avg {:.1} MiB/s",
            self.label,
            format_bytes(self.processed),
            format_bytes(self.total_bytes),
            current,
            average
        );
        let _ = io::stdout().flush();
    }

    fn finish(self) -> f64 {
        let seconds = self.start.elapsed().as_secs_f64().max(0.001);
        if self.enabled {
            println!();
            println!(
                "{} finished: {} in {:.2}s ({:.1} MiB/s)",
                self.label,
                format_bytes(self.processed),
                seconds,
                mib_per_sec(self.processed, seconds)
            );
        }
        seconds
    }

    fn processed(&self) -> u64 {
        self.processed
    }
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
    let mut state = prepare_state(options)?;
    let mut report = TestReport::new(target);
    report.passes_total = state.total_passes;
    report.passes_completed = state.completed_passes;

    if options.verify == VerifyMode::Later {
        if state.completed_passes >= state.total_passes {
            bail!("all passes on this volume are already complete");
        }

        let pass = state.completed_passes + 1;
        state.current_pass = pass;
        state.status = StateStatus::Writing;
        state.last_error = None;
        manifest::save_state(&options.volume, &mut state)?;

        console.info(&format!(
            "Pass {pass}/{}: writing test data (verification deferred)",
            state.total_passes
        ));
        let result = write_pass(options, &state, pass, console)?;
        apply_write_result(&mut state, &mut report, &result);

        state.status = StateStatus::PendingVerify;
        manifest::save_state(&options.volume, &mut state)?;

        report.pending_verify = true;
        report.success = result.capacity_ok;
        return Ok(report);
    }

    while state.completed_passes < state.total_passes {
        let pass = state.completed_passes + 1;
        state.current_pass = pass;
        state.status = StateStatus::Writing;
        state.last_error = None;
        manifest::save_state(&options.volume, &mut state)?;

        console.info(&format!(
            "Pass {pass}/{}: writing test data",
            state.total_passes
        ));
        let result = write_pass(options, &state, pass, console)?;
        apply_write_result(&mut state, &mut report, &result);
        state.status = StateStatus::PendingVerify;
        manifest::save_state(&options.volume, &mut state)?;

        if options.verify == VerifyMode::Delay {
            let seconds = options.delay.unwrap_or(0);
            if seconds > 0 {
                console.info(&format!(
                    "Waiting {seconds} second(s) before verification..."
                ));
                std::thread::sleep(Duration::from_secs(seconds));
            }
        }

        let verify_result = verify_pass(options, &state, pass, console, &mut report)?;
        report.read_seconds += verify_result.seconds;
        report.read_mib_per_sec =
            mib_per_sec(verify_result.bytes_verified, report.read_seconds.max(0.001));

        let all_ok = verify_result.all_ok && result.capacity_ok;
        if !all_ok {
            state.status = StateStatus::Failed;
            state.last_error = Some(if result.capacity_ok {
                "verification failed".to_string()
            } else {
                "capacity shortfall".to_string()
            });
            manifest::save_state(&options.volume, &mut state)?;
            cleanup_after_failure(target, options.cleanup)?;
            report.success = false;
            return Ok(report);
        }

        state.completed_passes = pass;
        report.passes_completed = pass;

        if pass >= state.total_passes {
            state.status = StateStatus::Completed;
            manifest::save_state(&options.volume, &mut state)?;
            cleanup_after_success(target, options.cleanup)?;
            report.success = true;
            return Ok(report);
        }

        state.status = StateStatus::PassComplete;
        manifest::save_state(&options.volume, &mut state)?;
        manifest::remove_test_files(target)?;
    }

    report.success = true;
    Ok(report)
}

pub fn verify_test(options: &VerifyOptions, console: &Console) -> Result<TestReport> {
    let target = &options.target;
    let mut state = load_or_infer_state(options)?;

    if state.status != StateStatus::PendingVerify {
        bail!(
            "no pending verification data on {} (status: {:?})",
            target.display(),
            state.status
        );
    }

    state.status = StateStatus::Verifying;
    state.last_error = None;
    manifest::save_state(&options.volume, &mut state)?;

    let pass = state.current_pass;
    let files: Vec<TestFile> = manifest::scan_test_files(target)?
        .into_iter()
        .filter(|file| file.pass == pass)
        .collect();
    if files.is_empty() {
        bail!(
            "no test files found for pass {pass} on {}",
            target.display()
        );
    }

    let mut report = TestReport::new(target);
    report.passes_total = state.total_passes;
    report.passes_completed = state.completed_passes;
    report.files_total = files.len() as u32;
    report.expected_total = state.expected_total;
    report.actual_total = state.actual_total;
    report.capacity_ok = state.capacity_ok;

    let verify_result = verify_files(
        target,
        &files,
        options.stop_on_fail,
        options.progress,
        console,
        &mut report,
    )?;
    report.read_seconds = verify_result.seconds;
    report.read_mib_per_sec =
        mib_per_sec(verify_result.bytes_verified, report.read_seconds.max(0.001));

    let all_ok = verify_result.all_ok && state.capacity_ok;
    if !all_ok {
        state.status = StateStatus::Failed;
        state.last_error = Some(if state.capacity_ok {
            "verification failed".to_string()
        } else {
            "capacity shortfall".to_string()
        });
        manifest::save_state(&options.volume, &mut state)?;
        cleanup_after_failure(target, options.cleanup)?;
        report.success = false;
        return Ok(report);
    }

    state.completed_passes = pass;
    report.passes_completed = pass;

    if state.completed_passes >= state.total_passes {
        state.status = StateStatus::Completed;
        manifest::save_state(&options.volume, &mut state)?;
        cleanup_after_success(target, options.cleanup)?;
        report.success = true;
    } else {
        state.status = StateStatus::PassComplete;
        manifest::save_state(&options.volume, &mut state)?;
        manifest::remove_test_files(target)?;
        report.success = true;
    }

    Ok(report)
}

fn apply_write_result(state: &mut TestState, report: &mut TestReport, result: &WritePassResult) {
    report.files_total += result.files.len() as u32;
    report.expected_total = result.expected_total;
    report.actual_total = result.actual_total;
    report.capacity_ok = result.capacity_ok;
    report.write_seconds += result.seconds;
    report.write_mib_per_sec = mib_per_sec(report.actual_total, report.write_seconds.max(0.001));

    state.expected_total = result.expected_total;
    state.actual_total = result.actual_total;
    state.capacity_ok = result.capacity_ok;
}

fn prepare_state(options: &RunOptions) -> Result<TestState> {
    let target = &options.target;
    let existing_state = manifest::load_state(&options.volume)?;
    let existing_files = manifest::scan_test_files(target)?;

    match existing_state {
        None => {
            if !existing_files.is_empty() {
                if options.force {
                    manifest::remove_test_files(target)?;
                } else {
                    bail!(
                        "test files already exist in {}; run `urwtest-rs verify --target {}` first, or use --force",
                        target.display(),
                        target.display()
                    );
                }
            }
            Ok(TestState::new(
                &options.volume,
                options.passes.max(1),
                options.stop_on_fail,
                options.cleanup,
            ))
        }
        Some(mut state) => match state.status {
            StateStatus::PassComplete => {
                manifest::remove_test_files(target)?;
                state.stop_on_fail = options.stop_on_fail;
                state.cleanup = options.cleanup;
                Ok(state)
            }
            StateStatus::PendingVerify => {
                bail!(
                    "pending verification data exists on {}; run `urwtest-rs verify --target {}` first, or use --force",
                    target.display(),
                    target.display()
                );
            }
            StateStatus::Completed
            | StateStatus::Failed
            | StateStatus::Writing
            | StateStatus::Verifying => {
                if !options.force {
                    bail!(
                        "existing test state on {} (status {:?}); use --force to start over",
                        target.display(),
                        state.status
                    );
                }
                manifest::remove_test_files(target)?;
                manifest::remove_state(&options.volume)?;
                Ok(TestState::new(
                    &options.volume,
                    options.passes.max(1),
                    options.stop_on_fail,
                    options.cleanup,
                ))
            }
        },
    }
}

fn load_or_infer_state(options: &VerifyOptions) -> Result<TestState> {
    if let Some(state) = manifest::load_state(&options.volume)? {
        return Ok(state);
    }

    let files = manifest::scan_test_files(&options.target)?;
    if files.is_empty() {
        bail!(
            "no test data found on {} (no state file and no matching test files)",
            options.target.display()
        );
    }

    let first = &files[0];
    let pass = first.pass;
    let total_passes = first.total_passes;
    let actual_total: u64 = files
        .iter()
        .filter(|file| file.pass == pass)
        .map(|file| file.actual_size)
        .sum();

    let mut state = TestState::new(
        &options.volume,
        total_passes,
        options.stop_on_fail,
        options.cleanup,
    );
    state.current_pass = pass;
    state.completed_passes = pass.saturating_sub(1);
    state.status = StateStatus::PendingVerify;
    state.expected_total = actual_total;
    state.actual_total = actual_total;
    state.capacity_ok = true;
    manifest::save_state(&options.volume, &mut state)?;
    Ok(state)
}

fn write_pass(
    options: &RunOptions,
    state: &TestState,
    pass: u32,
    console: &Console,
) -> Result<WritePassResult> {
    let target = &options.target;
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
    let mut progress = Progress::new("write", expected_total, options.progress);

    while actual_total < expected_total {
        let remaining = expected_total - actual_total;
        let planned_size = remaining.min(chunk_size).max(1);
        let seed = seed_for(pass, file_index);
        let name = manifest::make_test_file_name(pass, state.total_passes, file_index, seed);
        let path = target.join(&name);

        console.info(&format!(
            "  writing {name} (up to {})",
            format_bytes(planned_size)
        ));
        let file_start = Instant::now();
        let outcome = write_file(&path, planned_size, seed, &mut progress)?;
        let file_seconds = file_start.elapsed().as_secs_f64().max(0.001);
        actual_total = actual_total.saturating_add(outcome.actual_size);

        console.info(&format!(
            "  wrote {} in {:.2}s ({:.1} MiB/s)",
            format_bytes(outcome.actual_size),
            file_seconds,
            mib_per_sec(outcome.actual_size, file_seconds)
        ));

        files.push(TestFile {
            name,
            pass,
            total_passes: state.total_passes,
            index: file_index,
            seed,
            actual_size: outcome.actual_size,
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

    let seconds = progress.finish();
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
        seconds,
    })
}

fn write_file(
    path: &Path,
    max_size: u64,
    seed: u64,
    progress: &mut Progress,
) -> Result<WriteOutcome> {
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
                Ok(count) => {
                    offset += count;
                    progress.add(count as u64);
                }
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

fn verify_pass(
    options: &RunOptions,
    state: &TestState,
    pass: u32,
    console: &Console,
    report: &mut TestReport,
) -> Result<VerifyPassResult> {
    let files: Vec<TestFile> = manifest::scan_test_files(&options.target)?
        .into_iter()
        .filter(|file| file.pass == pass && file.total_passes == state.total_passes)
        .collect();
    if files.is_empty() {
        bail!(
            "no test files found for pass {pass} on {}",
            options.target.display()
        );
    }
    verify_files(
        &options.target,
        &files,
        options.stop_on_fail,
        options.progress,
        console,
        report,
    )
}

fn verify_files(
    target: &Path,
    files: &[TestFile],
    stop_on_fail: bool,
    progress_enabled: bool,
    console: &Console,
    report: &mut TestReport,
) -> Result<VerifyPassResult> {
    let total_bytes: u64 = files.iter().map(|file| file.actual_size).sum();
    let mut progress = Progress::new("verify", total_bytes, progress_enabled);
    let mut all_ok = true;

    for file in files {
        let path = target.join(&file.name);
        let file_start = Instant::now();
        let (ok, reason) = match verify_file(&path, file, &mut progress) {
            Ok(()) => (true, None),
            Err(reason) => (false, Some(reason)),
        };
        let file_seconds = file_start.elapsed().as_secs_f64().max(0.001);

        if ok {
            report.files_passed += 1;
            console.ok(&format!(
                "OK   {} ({}, {:.1} MiB/s)",
                file.name,
                format_bytes(file.actual_size),
                mib_per_sec(file.actual_size, file_seconds)
            ));
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

    let bytes_verified = progress.processed();
    let seconds = progress.finish();
    Ok(VerifyPassResult {
        all_ok,
        seconds,
        bytes_verified,
    })
}

fn verify_file(
    path: &Path,
    file: &TestFile,
    progress: &mut Progress,
) -> std::result::Result<(), String> {
    let metadata = std::fs::metadata(path).map_err(|error| format!("cannot stat file: {error}"))?;
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
        progress.add(block_size as u64);

        if actual[..block_size] != expected[..block_size] {
            return Err(format!("data mismatch at {}", format_bytes(offset)));
        }

        offset += block_size as u64;
    }

    Ok(())
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

fn mib_per_sec(bytes: u64, seconds: f64) -> f64 {
    bytes as f64 / seconds.max(0.001) / MIB
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
        CleanupMode::Always | CleanupMode::OnSuccess => manifest::remove_test_files(target),
        CleanupMode::Never => Ok(()),
    }
}

fn cleanup_after_failure(target: &Path, cleanup: CleanupMode) -> Result<()> {
    match cleanup {
        CleanupMode::Always => manifest::remove_test_files(target),
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

    fn record_for(
        name: &str,
        pass: u32,
        total_passes: u32,
        index: u32,
        seed: u64,
        actual_size: u64,
    ) -> TestFile {
        TestFile {
            name: name.to_string(),
            pass,
            total_passes,
            index,
            seed,
            actual_size,
        }
    }

    #[test]
    fn write_and_verify_round_trip() {
        let path = temporary_path("round-trip");
        let expected_size = IO_BLOCK_SIZE as u64 + 12_345;
        let seed = 0x1234_5678_9abc_def0;
        let mut progress = Progress::new("test", expected_size, false);

        let outcome = write_file(&path, expected_size, seed, &mut progress).unwrap();
        assert_eq!(outcome.actual_size, expected_size);
        assert_eq!(outcome.stop_reason, WriteStopReason::Complete);

        let record = record_for("round-trip.bin", 1, 1, 0, seed, outcome.actual_size);
        let mut progress = Progress::new("test", expected_size, false);
        verify_file(&path, &record, &mut progress).unwrap();

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn corruption_is_detected() {
        let path = temporary_path("corruption");
        let expected_size = IO_BLOCK_SIZE as u64 + 12_345;
        let seed = 0x0fed_cba9_8765_4321;
        let mut progress = Progress::new("test", expected_size, false);

        let outcome = write_file(&path, expected_size, seed, &mut progress).unwrap();
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

        let record = record_for("corruption.bin", 1, 1, 0, seed, outcome.actual_size);
        let mut progress = Progress::new("test", expected_size, false);
        assert!(verify_file(&path, &record, &mut progress).is_err());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn partial_file_data_can_be_verified() {
        let path = temporary_path("partial");
        let planned_size = 1024 * 1024;
        let actual_size = planned_size / 2;
        let seed = 42;
        let mut progress = Progress::new("test", actual_size, false);

        let outcome = write_file(&path, actual_size, seed, &mut progress).unwrap();
        assert_eq!(outcome.actual_size, actual_size);

        // Capacity shortfall is tracked globally by write_pass, so a partial
        // final file should still verify its actual contents successfully.
        let record = record_for("partial.bin", 1, 1, 0, seed, actual_size);
        let mut progress = Progress::new("test", actual_size, false);
        verify_file(&path, &record, &mut progress).unwrap();

        let _ = std::fs::remove_file(path);
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
