use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::extract::walker::BrokenDirPolicy;
use crate::io::rescue::BadSectorPolicy;

#[derive(Parser)]
#[command(name = "acornfsextract", about = "Extracts files from Acorn FileCore (ADFS) and DFS disc images", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Reports disc metadata (format, disc name, size, structural health)
    /// without extracting.
    Info {
        image: PathBuf,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
    /// Extracts every file from the image into a directory tree.
    Extract {
        image: PathBuf,
        #[arg(long)]
        output: PathBuf,
        /// ddrescue/gddrescue mapfile marking known bad sectors.
        #[arg(long)]
        rescue_map: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = BadSectorArg::Null)]
        bad_sectors: BadSectorArg,
        #[arg(long, value_enum, default_value_t = BrokenDirArg::Recover)]
        on_broken_directory: BrokenDirArg,
        /// Write a .inf sidecar file next to each extracted file.
        #[arg(long)]
        inf: bool,
        /// Write the extraction log (bad sectors, skips, anomalies) here.
        #[arg(long)]
        log: Option<PathBuf>,
        /// List what would be extracted without writing anything.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum BadSectorArg {
    Skip,
    Null,
    Marker,
}

impl From<BadSectorArg> for BadSectorPolicy {
    fn from(v: BadSectorArg) -> Self {
        match v {
            BadSectorArg::Skip => BadSectorPolicy::Skip,
            BadSectorArg::Null => BadSectorPolicy::NullFill,
            BadSectorArg::Marker => BadSectorPolicy::MarkerFill,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
pub enum BrokenDirArg {
    Fail,
    Skip,
    Recover,
}

impl From<BrokenDirArg> for BrokenDirPolicy {
    fn from(v: BrokenDirArg) -> Self {
        match v {
            BrokenDirArg::Fail => BrokenDirPolicy::Fail,
            BrokenDirArg::Skip => BrokenDirPolicy::Skip,
            BrokenDirArg::Recover => BrokenDirPolicy::Recover,
        }
    }
}
