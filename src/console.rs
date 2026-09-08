use std::io::{self, IsTerminal};

use crate::types::ColorMode;
use crate::volume::{VolumeInfo, VolumeKind};

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

#[derive(Debug, Clone, Copy)]
pub struct Console {
    color: bool,
    terminal: bool,
}

impl Console {
    pub fn new(mode: ColorMode) -> Self {
        let terminal = io::stdout().is_terminal();
        let color = match mode {
            ColorMode::Always => true,
            ColorMode::Never => false,
            ColorMode::Auto => terminal,
        };
        Self { color, terminal }
    }

    pub fn progress_enabled(&self) -> bool {
        self.terminal
    }

    fn paint(&self, code: &str, message: &str) -> String {
        if self.color {
            format!("{code}{message}{RESET}")
        } else {
            message.to_string()
        }
    }

    pub fn ok(&self, message: &str) {
        println!("{}", self.paint(GREEN, message));
    }

    pub fn fail(&self, message: &str) {
        println!("{}", self.paint(RED, message));
    }

    pub fn warn(&self, message: &str) {
        println!("{}", self.paint(YELLOW, message));
    }

    pub fn info(&self, message: &str) {
        println!("{}", self.paint(CYAN, message));
    }

    pub fn print_volumes(&self, volumes: &[VolumeInfo]) {
        println!(
            "{:<4} {:<28} {:<12} {:<10} {:>12} {:>12}",
            "#", "Mount point", "Kind", "FS", "Total", "Free"
        );
        println!("{}", "-".repeat(84));
        for (index, volume) in volumes.iter().enumerate() {
            println!(
                "{:<4} {:<28} {:<12} {:<10} {:>12} {:>12}",
                index + 1,
                volume.mount_point.display(),
                volume.kind.label(),
                volume.fs,
                format_bytes(volume.total_bytes),
                format_bytes(volume.free_bytes)
            );
        }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

impl VolumeKind {
    pub fn label(self) -> &'static str {
        match self {
            VolumeKind::Removable => "Removable",
            VolumeKind::Fixed => "Fixed",
            VolumeKind::Network => "Network",
            VolumeKind::Other => "Other",
        }
    }
}
