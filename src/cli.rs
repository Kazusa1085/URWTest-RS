use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::types::{CleanupMode, ColorMode, VerifyMode};

#[derive(Debug, Parser)]
#[command(
    name = "urwtest-rs",
    version,
    about = "Cross-platform mounted-volume read/write test"
)]
pub struct Cli {
    /// Emit machine-readable JSON where supported.
    #[arg(long, global = true)]
    pub json: bool,

    /// Control ANSI colors.
    #[arg(long, global = true, value_enum, default_value_t = ColorMode::Auto)]
    pub color: ColorMode,

    /// Disable live speed/progress output.
    #[arg(long, global = true)]
    pub no_progress: bool,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// List mounted volumes that can be tested.
    List,

    /// Write test data to a mounted volume and optionally verify it.
    Run(RunArgs),

    /// Verify test data previously written to a mounted volume.
    Verify(VerifyArgs),

    /// Show the persisted test state for a mounted volume.
    Status(StatusArgs),
}

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Mount point or drive letter to test, for example /mnt/usb or E:\.
    #[arg(short, long)]
    pub target: PathBuf,

    /// Number of write/verify passes.
    #[arg(short, long, default_value_t = 1)]
    pub passes: u32,

    /// When to verify the written data.
    #[arg(long, value_enum, default_value_t = VerifyMode::Immediate)]
    pub verify: VerifyMode,

    /// Seconds to wait before verification when --verify delay is used.
    #[arg(long)]
    pub delay: Option<u64>,

    /// Stop the current pass after the first failed file.
    #[arg(long)]
    pub stop_on_fail: bool,

    /// What to do with test files after verification.
    #[arg(long, value_enum, default_value_t = CleanupMode::OnSuccess)]
    pub cleanup: CleanupMode,

    /// Discard existing test state on the target volume and start over.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Mount point or drive letter containing pending test data.
    #[arg(short, long)]
    pub target: PathBuf,

    /// Stop the pass after the first failed file.
    #[arg(long)]
    pub stop_on_fail: bool,

    /// What to do with test files after verification.
    #[arg(long, value_enum, default_value_t = CleanupMode::OnSuccess)]
    pub cleanup: CleanupMode,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Mount point or drive letter to inspect.
    #[arg(short, long)]
    pub target: PathBuf,
}
