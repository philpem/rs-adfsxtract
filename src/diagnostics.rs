//! Structured diagnostics for the `verify` command. A typed `Fault` gives
//! callers and scripts a stable machine-readable category while `Display`
//! keeps the human-facing message. This is deliberately a small, honest set:
//! every variant must be producible by the verification path.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

/// Severity is kept small and stable because diagnostics are a command-line
/// contract as much as a recovery log.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    Warning,
    Damage,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Damage => "damage",
        }
    }
}

/// A typed problem found while verifying an image.
#[derive(Clone, Debug)]
pub enum Fault {
    BootBlockChecksumMismatch {
        stored: u8,
        computed: u8,
    },
    ZoneChecksumMismatch {
        zone: usize,
    },
    ZoneCrossCheckMismatch {
        actual: u8,
        expected: u8,
    },
    DiscRecordGeometryMismatch {
        field: String,
        boot: String,
        zone0: String,
    },
    BrokenDirectory {
        details: String,
    },
    UnreadableDirectory {
        details: String,
    },
    UnreadableFile {
        details: String,
    },
    DirectoryCycle,
}

impl Fault {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BootBlockChecksumMismatch { .. } => "boot_block_checksum_mismatch",
            Self::ZoneChecksumMismatch { .. } => "zone_checksum_mismatch",
            Self::ZoneCrossCheckMismatch { .. } => "zone_cross_check_mismatch",
            Self::DiscRecordGeometryMismatch { .. } => "disc_record_geometry_mismatch",
            Self::BrokenDirectory { .. } => "broken_directory",
            Self::UnreadableDirectory { .. } => "unreadable_directory",
            Self::UnreadableFile { .. } => "unreadable_file",
            Self::DirectoryCycle => "directory_cycle",
        }
    }

    pub fn severity(&self) -> Severity {
        match self {
            Self::BootBlockChecksumMismatch { .. }
            | Self::ZoneChecksumMismatch { .. }
            | Self::ZoneCrossCheckMismatch { .. }
            | Self::DiscRecordGeometryMismatch { .. } => Severity::Warning,
            Self::BrokenDirectory { .. }
            | Self::UnreadableDirectory { .. }
            | Self::UnreadableFile { .. }
            | Self::DirectoryCycle => Severity::Damage,
        }
    }
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BootBlockChecksumMismatch { stored, computed } => write!(
                f,
                "boot block checksum mismatch: stored 0x{stored:02x}, computed 0x{computed:02x}"
            ),
            Self::ZoneChecksumMismatch { zone } => write!(f, "zone {zone} checksum mismatch"),
            Self::ZoneCrossCheckMismatch { actual, expected } => write!(
                f,
                "zone CrossCheck XOR is 0x{actual:02x}, expected 0x{expected:02x}"
            ),
            Self::DiscRecordGeometryMismatch { field, boot, zone0 } => write!(
                f,
                "disc record {field} differs between boot block ({boot}) and zone 0 ({zone0}); keeping boot-block geometry"
            ),
            Self::BrokenDirectory { details } => write!(f, "broken directory: {details}"),
            Self::UnreadableDirectory { details } => write!(f, "unreadable directory: {details}"),
            Self::UnreadableFile { details } => write!(f, "unreadable file: {details}"),
            Self::DirectoryCycle => write!(f, "directory cycle detected"),
        }
    }
}

/// One structured problem report. Offsets are in image byte space; a path is
/// optional because some faults are found before any directory entry is known.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub fault: Fault,
    pub path: Option<String>,
    pub offset: Option<u64>,
    pub len: Option<u64>,
}

impl Diagnostic {
    pub fn from_fault(fault: Fault) -> Self {
        Self {
            severity: fault.severity(),
            fault,
            path: None,
            offset: None,
            len: None,
        }
    }

    pub fn at_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn at_range(mut self, offset: u64, len: u64) -> Self {
        self.offset = Some(offset);
        self.len = Some(len);
        self
    }

    fn write_human(&self, mut out: impl Write) -> io::Result<()> {
        write!(out, "{}:", self.severity.as_str())?;
        if let Some(path) = &self.path {
            write!(out, " {path}:")?;
        }
        if let Some(offset) = self.offset {
            write!(out, " @0x{offset:x}")?;
            if let Some(len) = self.len {
                write!(out, "+0x{len:x}")?;
            }
            write!(out, ":")?;
        }
        writeln!(out, " {}", self.fault)
    }

    fn write_jsonl(&self, mut out: impl Write) -> io::Result<()> {
        write!(
            out,
            "{{\"severity\":\"{}\",\"code\":\"{}\",\"message\":\"{}\"",
            self.severity.as_str(),
            self.fault.code(),
            json_escape(&self.fault.to_string())
        )?;
        if let Some(path) = &self.path {
            write!(out, ",\"path\":\"{}\"", json_escape(path))?;
        }
        if let Some(offset) = self.offset {
            write!(out, ",\"offset\":{offset}")?;
        }
        if let Some(len) = self.len {
            write!(out, ",\"len\":{len}")?;
        }
        writeln!(out, "}}")
    }
}

#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    pub fn extend(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        self.items.extend(diagnostics);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn print_stderr(&self) -> io::Result<()> {
        let stderr = io::stderr();
        let mut locked = stderr.lock();
        for item in &self.items {
            item.write_human(&mut locked)?;
        }
        Ok(())
    }

    pub fn write_jsonl_file(&self, path: &Path) -> io::Result<()> {
        let mut file = File::create(path)?;
        for item in &self.items {
            item.write_jsonl(&mut file)?;
        }
        Ok(())
    }
}

fn json_escape(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out
}
