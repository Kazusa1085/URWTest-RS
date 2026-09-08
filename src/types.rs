use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VerifyMode {
    /// Write the test data, then immediately read it back and verify it.
    #[default]
    Immediate,
    /// Write the test data and exit. The data is verified on the next run.
    Later,
    /// Write the test data, wait for a configurable delay, then verify it.
    Delay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CleanupMode {
    /// Remove test files after the operation, even if verification failed.
    Always,
    /// Remove test files only after successful verification.
    #[default]
    OnSuccess,
    /// Keep test files for manual inspection.
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}
