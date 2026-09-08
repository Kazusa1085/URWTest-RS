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

    match &cli.command {
        None => interactive::run(&console)?,
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
            };

            let report = engine::run_test(&options, &console)?;
            emit_report(&cli, &console, &report)?;
            if !report.success {
                process::exit(1);
            }
        }
        Some(Commands::Verify(args)) => {
            let target = volume::normalize_target_path(&args.target);
            let _volume = resolve_target(&target)?;
            let options = VerifyOptions {
                target,
                stop_on_fail: args.stop_on_fail,
                cleanup: args.cleanup,
            };

            let report = engine::verify_test(&options, &console)?;
            emit_report(&cli, &console, &report)?;
            if !report.success {
                process::exit(1);
            }
        }
        Some(Commands::Status(args)) => {
            let target = volume::normalize_target_path(&args.target);
            let _volume = resolve_target(&target)?;
            let manifest = manifest::load(&target)?;

            if cli.json {
                match manifest {
                    Some(manifest) => {
                        println!("{}", serde_json::to_string_pretty(&manifest)?);
                    }
                    None => {
                        println!("null");
                    }
                }
            } else {
                match manifest {
                    Some(manifest) => {
                        println!("Target: {}", manifest.target);
                        println!("Status: {:?}", manifest.status);
                        println!(
                            "Passes: {}/{}",
                            manifest.completed_passes, manifest.total_passes
                        );
                        println!("Current pass: {}", manifest.current_pass);
                        println!("Files: {}", manifest.files.len());
                        if let Some(error) = &manifest.last_error {
                            println!("Last error: {error}");
                        }
                    }
                    None => {
                        console.warn("No test state found on this volume.");
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
