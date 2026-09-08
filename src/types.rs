use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyMode {
    /// Write the test data, then immediately read it back and verify it.
    Immediate,
    /// Write the test data and exit. The data is verified on the next run.
    Later,
    /// Write the test data, wait for a configurable delay, then verify it.
    Delay,
}

impl Default for VerifyMode {
    fn default() -> Self {
        Self::Immediate
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupMode {
    /// Remove test files after the operation, even if verification failed.
    Always,
    /// Remove test files only after successful verification.
    OnSuccess,
    /// Keep test files for manual inspection.
    Never,
}

impl Default for CleanupMode {
    fn default() -> Self {
        Self::OnSuccess
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

impl Default for ColorMode {
    fn default() -> Self {
        Self::Auto
    }
}
