mod cli;
mod console;
mod engine;
mod interactive;
mod manifest;
mod types;
mod volume;

use std::path::Path;
use std::process;

use anyhow::{Result, anyhow, bail};
use clap::Parser;

use crate::cli::{Cli, Commands};
use crate::console::{Console, format_bytes};
use crate::engine::{RunOptions, TestReport, VerifyOptions};
use crate::types::VerifyMode;

fn main() {
    if let Err(error) = real_main() {
        eprintln!("Error: {error:#}");
        process::exit(2);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let console = Console::new(cli.color);
    let progress = console.progress_enabled() && !cli.no_progress;

    match &cli.command {
        None => interactive::run(&console, progress)?,
        Some(Commands::List) => {
            let volumes = volume::list_volumes();
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&volumes)?);
            } else if volumes.is_empty() {
                console.warn("No testable mounted volumes found.");
            } else {
                console.print_volumes(&volumes);
            }
        }
        Some(Commands::Run(args)) => {
            if args.verify == VerifyMode::Delay && args.delay.is_none() {
                bail!("--delay is required when --verify delay is used");
            }

            let target = volume::normalize_target_path(&args.target);
            let volume = resolve_target(&target)?;
            let options = RunOptions {
                target,
                volume,
                passes: args.passes.max(1),
                verify: args.verify,
                delay: args.delay,
                stop_on_fail: args.stop_on_fail,
                cleanup: args.cleanup,
                force: args.force,
                progress,
            };

            let report = engine::run_test(&options, &console)?;
            emit_report(&cli, &console, &report)?;
            if !report.success {
                process::exit(1);
            }
        }
        Some(Commands::Verify(args)) => {
            let target = volume::normalize_target_path(&args.target);
            let volume = resolve_target(&target)?;
            let options = VerifyOptions {
                target,
                volume,
                stop_on_fail: args.stop_on_fail,
                cleanup: args.cleanup,
                progress,
            };

            let report = engine::verify_test(&options, &console)?;
            emit_report(&cli, &console, &report)?;
            if !report.success {
                process::exit(1);
            }
        }
        Some(Commands::Status(args)) => {
            let target = volume::normalize_target_path(&args.target);
            let volume = resolve_target(&target)?;
            let state = manifest::load_state(&volume)?;
            let files = manifest::scan_test_files(&target)?;

            if cli.json {
                match state {
                    Some(state) => {
                        println!("{}", serde_json::to_string_pretty(&state)?);
                    }
                    None if !files.is_empty() => {
                        let value = serde_json::json!({
                            "state": null,
                            "pending_files": files.len(),
                            "pass": files[0].pass,
                            "total_passes": files[0].total_passes,
                        });
                        println!("{}", serde_json::to_string_pretty(&value)?);
                    }
                    None => {
                        println!("null");
                    }
                }
            } else {
                match state {
                    Some(state) => {
                        println!("Target: {}", state.target);
                        println!("Status: {:?}", state.status);
                        println!("Passes: {}/{}", state.completed_passes, state.total_passes);
                        println!("Current pass: {}", state.current_pass);
                        println!(
                            "Capacity: {} / {}",
                            format_bytes(state.actual_total),
                            format_bytes(state.expected_total)
                        );
                        if let Some(error) = &state.last_error {
                            println!("Last error: {error}");
                        }
                    }
                    None if !files.is_empty() => {
                        console.warn(&format!(
                            "No state file, but {} pending test file(s) found.",
                            files.len()
                        ));
                        println!(
                            "Detected pass: {} / {}",
                            files[0].pass, files[0].total_passes
                        );
                    }
                    None => {
                        console.warn("No test state found for this volume.");
                    }
                }
            }
        }
    }

    Ok(())
}

fn resolve_target(path: &Path) -> Result<volume::VolumeInfo> {
    volume::find_volume(path)
        .ok_or_else(|| anyhow!("target is not a mounted volume root: {}", path.display()))
}

fn emit_report(cli: &Cli, console: &Console, report: &TestReport) -> Result<()> {
    if cli.json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    println!("Target: {}", report.target);
    println!(
        "Passes: {}/{}",
        report.passes_completed, report.passes_total
    );
    println!(
        "Files:  {} passed, {} failed, {} total",
        report.files_passed, report.files_failed, report.files_total
    );
    if report.write_seconds > 0.0 {
        println!(
            "Write:  {} in {:.2}s ({:.1} MiB/s)",
            format_bytes(report.actual_total),
            report.write_seconds,
            report.write_mib_per_sec
        );
    }
    if report.read_seconds > 0.0 {
        println!(
            "Read:   {} in {:.2}s ({:.1} MiB/s)",
            format_bytes(report.actual_total),
            report.read_seconds,
            report.read_mib_per_sec
        );
    }

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

    Ok(())
}
