use std::io::{self, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::console::{format_bytes, Console};
use crate::engine::{self, RunOptions, TestReport, VerifyOptions};
use crate::manifest::{self, ManifestStatus};
use crate::types::{CleanupMode, VerifyMode};
use crate::volume::{self, VolumeInfo};

pub fn run(console: &Console) -> Result<()> {
    let volumes = volume::list_volumes();
    if volumes.is_empty() {
        bail!("no testable mounted volumes found");
    }

    println!("Available volumes:");
    console.print_volumes(&volumes);
    println!();

    let target = prompt_target(&volumes)?;
    let volume = volume::find_volume(&target)
        .with_context(|| format!("{} is not a mounted volume root", target.display()))?;

    if let Some(manifest) = manifest::load(&target)? {
        match manifest.status {
            ManifestStatus::PendingVerify => {
                let verify_now =
                    prompt_yes_no("检测到存在已经写入数据，是否立刻校验？", true)?;
                if verify_now {
                    let options = VerifyOptions {
                        target: target.clone(),
                        stop_on_fail: false,
                        cleanup: CleanupMode::OnSuccess,
                    };
                    let report = engine::verify_test(&options, console)?;
                    print_report(console, &report);

                    if report.success && report.passes_completed < report.passes_total {
                        let continue_next =
                            prompt_yes_no("还有未完成的跑圈，是否继续下一圈？", true)?;
                        if continue_next {
                            let options = RunOptions {
                                target,
                                volume,
                                passes: report.passes_total,
                                verify: VerifyMode::Immediate,
                                delay: None,
                                stop_on_fail: false,
                                cleanup: CleanupMode::OnSuccess,
                                force: false,
                            };
                            let report = engine::run_test(&options, console)?;
                            print_report(console, &report);
                        }
                    }
                }
                return Ok(());
            }
            ManifestStatus::PassComplete => {
                let continue_next =
                    prompt_yes_no("检测到上一圈已完成，是否继续下一圈？", true)?;
                if continue_next {
                    let options = RunOptions {
                        target,
                        volume,
                        passes: manifest.total_passes,
                        verify: VerifyMode::Immediate,
                        delay: None,
                        stop_on_fail: false,
                        cleanup: CleanupMode::OnSuccess,
                        force: false,
                    };
                    let report = engine::run_test(&options, console)?;
                    print_report(console, &report);
                }
                return Ok(());
            }
            ManifestStatus::Completed => {
                console.info("该卷上的测试已经完成。");
                return Ok(());
            }
            ManifestStatus::Failed => {
                console.fail("该卷上的测试之前失败了。请使用 --force 重新开始。");
                return Ok(());
            }
            ManifestStatus::Writing | ManifestStatus::Verifying => {
                console.fail("检测到中断的测试状态。请使用 --force 重新开始。");
                return Ok(());
            }
        }
    }

    let passes = prompt_u32("Enter pass count", 1)?;
    let immediate = prompt_yes_no("Write then verify immediately?", true)?;
    let (verify_mode, delay) = if immediate {
        (VerifyMode::Immediate, None)
    } else {
        let seconds = prompt_u64("Enter delay seconds (0 = exit and verify later)", 0)?;
        if seconds == 0 {
            (VerifyMode::Later, None)
        } else {
            (VerifyMode::Delay, Some(seconds))
        }
    };
    let stop_on_fail = prompt_yes_no("Stop on first failure?", true)?;
    let keep_files = prompt_yes_no("Keep test files after verification?", false)?;
    let cleanup = if keep_files {
        CleanupMode::Never
    } else {
        CleanupMode::OnSuccess
    };

    println!();
    println!("Target: {}", volume.display_name());
    println!("Passes: {passes}");
    println!(
        "Verify: {}",
        match verify_mode {
            VerifyMode::Immediate => "immediate",
            VerifyMode::Later => "later",
            VerifyMode::Delay => "after delay",
        }
    );
    if let Some(seconds) = delay {
        println!("Delay: {seconds} second(s)");
    }
    println!(
        "On failure: {}",
        if stop_on_fail {
            "stop immediately"
        } else {
            "continue"
        }
    );
    println!(
        "Test files: {}",
        if keep_files { "keep" } else { "remove on success" }
    );

    if !prompt_yes_no("Start test?", true)? {
        return Ok(());
    }

    let options = RunOptions {
        target,
        volume,
        passes,
        verify: verify_mode,
        delay,
        stop_on_fail,
        cleanup,
        force: false,
    };
    let report = engine::run_test(&options, console)?;
    print_report(console, &report);

    if report.pending_verify {
        console.info("写入完成。请下次插入该盘并重新运行本程序进行校验。");
    }

    Ok(())
}

fn print_report(console: &Console, report: &TestReport) {
    println!();
    println!("Target: {}", report.target);
    println!(
        "Passes: {}/{}",
        report.passes_completed, report.passes_total
    );
    println!(
        "Files:  {} passed, {} failed, {} total",
        report.files_passed, report.files_failed, report.files_total
    );

    if !report.capacity_ok {
        console.fail(&format!(
            "Capacity shortfall: wrote {} of {}",
            format_bytes(report.actual_total),
            format_bytes(report.expected_total)
        ));
    }

    if report.files_failed > 0 {
        console.fail("Test failed.");
        for file in &report.failed_files {
            console.fail(&format!("  failed: {file}"));
        }
    } else if report.pending_verify {
        console.warn("Test data written. Verification is still pending.");
    } else if report.success {
        console.ok("All checks passed.");
    } else {
        console.fail("Test did not complete successfully.");
    }
}

fn prompt_target(volumes: &[VolumeInfo]) -> Result<PathBuf> {
    loop {
        print!("Enter volume number or path: ");
        io::stdout().flush()?;

        let input = read_input()?;
        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        if let Ok(index) = input.parse::<usize>() {
            if index >= 1 && index <= volumes.len() {
                return Ok(volumes[index - 1].mount_point.clone());
            }
            eprintln!("Invalid volume number.");
            continue;
        }

        let path = PathBuf::from(input);
        if volume::find_volume(&path).is_some() {
            return Ok(path);
        }
        eprintln!("{} is not a mounted volume root.", path.display());
    }
}

fn prompt_yes_no(prompt: &str, default: bool) -> Result<bool> {
    loop {
        let suffix = if default { "[Y/n]" } else { "[y/N]" };
        print!("{prompt} {suffix} ");
        io::stdout().flush()?;

        let input = read_input()?;
        let input = input.trim().to_ascii_lowercase();

        match input.as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => eprintln!("Please answer y or n."),
        }
    }
}

fn prompt_u32(prompt: &str, default: u32) -> Result<u32> {
    loop {
        print!("{prompt} [{default}] ");
        io::stdout().flush()?;

        let input = read_input()?;
        let input = input.trim();

        if input.is_empty() {
            return Ok(default);
        }
        match input.parse::<u32>() {
            Ok(value) if value >= 1 => return Ok(value),
            _ => eprintln!("Please enter a positive integer."),
        }
    }
}

fn prompt_u64(prompt: &str, default: u64) -> Result<u64> {
    loop {
        print!("{prompt} [{default}] ");
        io::stdout().flush()?;

        let input = read_input()?;
        let input = input.trim();

        if input.is_empty() {
            return Ok(default);
        }
        match input.parse::<u64>() {
            Ok(value) => return Ok(value),
            Err(_) => eprintln!("Please enter a non-negative integer."),
        }
    }
}

fn read_input() -> Result<String> {
    let mut input = String::new();
    if io::stdin().read_line(&mut input)? == 0 {
        bail!("standard input closed");
    }
    Ok(input)
}
