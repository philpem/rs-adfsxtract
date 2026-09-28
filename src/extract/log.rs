use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum LogEntry {
    Extracted {
        path: String,
        bytes: u64,
    },
    BadSectors {
        path: String,
        ranges: Vec<(u64, u64)>,
        policy: String,
    },
    SkippedWhollyBad {
        path: String,
    },
    BrokenDirectory {
        path: String,
        anomalies: Vec<String>,
        action: String,
    },
    SkippedEntry {
        path: String,
        reason: String,
    },
    Warning {
        message: String,
    },
}

impl LogEntry {
    fn to_text(&self) -> String {
        match self {
            LogEntry::Extracted { path, bytes } => format!("extracted  {path} ({bytes} bytes)"),
            LogEntry::BadSectors {
                path,
                ranges,
                policy,
            } => {
                let ranges_str: Vec<String> = ranges
                    .iter()
                    .map(|(s, l)| format!("{s:#x}+{l:#x}"))
                    .collect();
                format!(
                    "bad-sectors {path} [{}] policy={policy}",
                    ranges_str.join(", ")
                )
            }
            LogEntry::SkippedWhollyBad { path } => {
                format!("skipped    {path} (wholly bad sectors)")
            }
            LogEntry::BrokenDirectory {
                path,
                anomalies,
                action,
            } => {
                format!(
                    "broken-dir {path} action={action}: {}",
                    anomalies.join("; ")
                )
            }
            LogEntry::SkippedEntry { path, reason } => format!("skipped    {path}: {reason}"),
            LogEntry::Warning { message } => format!("warning    {message}"),
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct ExtractionLog {
    pub entries: Vec<LogEntry>,
}

impl ExtractionLog {
    pub fn push(&mut self, entry: LogEntry) {
        self.entries.push(entry);
    }

    pub fn to_text(&self) -> String {
        self.entries
            .iter()
            .map(LogEntry::to_text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn write_to(&self, path: &Path) -> io::Result<()> {
        let mut f = File::create(path)?;
        writeln!(f, "{}", self.to_text())
    }
}
