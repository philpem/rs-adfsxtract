use std::fs::File;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use serde::Serialize;

use acornfsextract::cli::{Cli, Command, OutputFormat};
use acornfsextract::diagnostics::Diagnostics;
use acornfsextract::error::FcError;
use acornfsextract::extract::log::ExtractionLog;
use acornfsextract::extract::report::{
    DiscReport, build_afs_report, build_dfs_report, build_report,
};
use acornfsextract::extract::walker::{ExtractOptions, ExtractSummary, walk_and_extract};
use acornfsextract::format::afs::AfsFs;
use acornfsextract::format::dfs::DfsFs;
use acornfsextract::format::filecore::FileCoreFs;
use acornfsextract::io::rescue::RescueMap;
use acornfsextract::verify::{VerifyReport, verify, verify_filecore_volume};

const EXIT_OK: u8 = 0;
const EXIT_ERROR: u8 = 1;
const EXIT_NOT_RECOGNISED: u8 = 2;

#[derive(Serialize)]
struct NotRecognised {
    recognized: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Info { image, format } => run_info(&image, format),
        Command::Extract {
            image,
            output,
            rescue_map,
            bad_sectors,
            on_broken_directory,
            inf,
            log,
            dry_run,
            partition,
            format,
        } => run_extract(
            &image,
            &output,
            rescue_map.as_deref(),
            bad_sectors.into(),
            on_broken_directory.into(),
            inf,
            log.as_deref(),
            dry_run,
            partition.as_deref(),
            format,
        ),
        Command::Verify {
            image,
            format,
            diagnostics,
        } => run_verify(&image, format, diagnostics.as_deref()),
    }
}

/// One filesystem volume found on an image. Most discs contain a single
/// volume; an ADFS/AFS hybrid contains two (an ADFS partition and a File
/// Server partition), presented separately so they can be handled
/// independently or extracted together into subdirectories.
struct Volume {
    label: String,
    fs: AnyFs,
}

/// A single recognised filesystem backend.
enum AnyFs {
    FileCore(Box<FileCoreFs<File>>),
    Afs(AfsFs<File>),
    Dfs(DfsFs<File>),
}

/// Opens every recognisable volume on an image. FileCore and AFS are both
/// signature/structurally-based and are always attempted (an ADFS/AFS hybrid
/// yields two volumes); DFS - which has no magic number, only structural
/// plausibility - is only tried as a final fallback if neither of the others
/// matched. Labels are made unique (a repeated filesystem type is suffixed).
fn open_volumes(path: &Path) -> Result<Vec<Volume>, FcError> {
    let mut volumes = Vec::new();
    let mut hard_err: Option<FcError> = None;

    if let Ok(file) = File::open(path) {
        match FileCoreFs::open(file) {
            Ok(fs) => push_volume(&mut volumes, "ADFS", AnyFs::FileCore(Box::new(fs))),
            Err(FcError::NotRecognised) => {}
            Err(e) => hard_err = Some(e),
        }
    }

    if let Ok(file) = File::open(path) {
        match AfsFs::open(file) {
            Ok(fs) => push_volume(&mut volumes, "AFS", AnyFs::Afs(fs)),
            Err(FcError::NotRecognised) => {}
            Err(e) => hard_err = Some(e),
        }
    }

    if volumes.is_empty()
        && let Ok(file) = File::open(path)
    {
        match DfsFs::open(file) {
            Ok(fs) => push_volume(&mut volumes, "DFS", AnyFs::Dfs(fs)),
            Err(FcError::NotRecognised) => {}
            Err(e) => hard_err = Some(e),
        }
    }

    if volumes.is_empty() {
        Err(hard_err.unwrap_or(FcError::NotRecognised))
    } else {
        Ok(volumes)
    }
}

fn push_volume(volumes: &mut Vec<Volume>, label: &str, fs: AnyFs) {
    let mut name = label.to_string();
    let mut n = 0;
    while volumes.iter().any(|v| v.label == name) {
        n += 1;
        name = format!("{label}-{n}");
    }
    volumes.push(Volume { label: name, fs });
}

fn volume_report(v: &mut Volume) -> Result<DiscReport, FcError> {
    match &mut v.fs {
        AnyFs::FileCore(fc) => build_report(fc.as_mut()),
        AnyFs::Afs(afs) => build_afs_report(afs),
        AnyFs::Dfs(dfs) => build_dfs_report(dfs),
    }
}

fn volume_verify(v: &mut Volume, diag: &mut Diagnostics) -> Result<VerifyReport, FcError> {
    match &mut v.fs {
        AnyFs::FileCore(fc) => {
            verify_filecore_volume(&**fc, diag)?;
            verify(&mut **fc, diag)
        }
        AnyFs::Afs(afs) => verify(afs, diag),
        AnyFs::Dfs(dfs) => verify(&mut *dfs, diag),
    }
}

fn volume_extract(
    v: &mut Volume,
    opts: &ExtractOptions,
    log: &mut ExtractionLog,
) -> Result<ExtractSummary, FcError> {
    match &mut v.fs {
        AnyFs::FileCore(fc) => walk_and_extract(fc.as_mut(), opts, log),
        AnyFs::Afs(afs) => walk_and_extract(afs, opts, log),
        AnyFs::Dfs(dfs) => walk_and_extract(dfs, opts, log),
    }
}

fn open_or_exit(image: &Path, format: OutputFormat) -> Result<Vec<Volume>, ExitCode> {
    match open_volumes(image) {
        Ok(v) => Ok(v),
        Err(FcError::NotRecognised) => {
            print_not_recognised(format);
            Err(ExitCode::from(EXIT_NOT_RECOGNISED))
        }
        Err(e) => {
            eprintln!("error: {e}");
            Err(ExitCode::from(EXIT_ERROR))
        }
    }
}

fn run_info(image: &Path, format: OutputFormat) -> ExitCode {
    let mut volumes = match open_or_exit(image, format) {
        Ok(v) => v,
        Err(code) => return code,
    };

    let mut reports = Vec::new();
    for v in &mut volumes {
        match volume_report(v) {
            Ok(r) => reports.push((v.label.clone(), r)),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(EXIT_ERROR);
            }
        }
    }
    print_partition_reports(&reports, format);
    ExitCode::from(EXIT_OK)
}

#[allow(clippy::too_many_arguments)]
fn run_extract(
    image: &Path,
    output: &Path,
    rescue_map_path: Option<&Path>,
    bad_sector_policy: acornfsextract::io::rescue::BadSectorPolicy,
    broken_dir_policy: acornfsextract::extract::walker::BrokenDirPolicy,
    write_inf: bool,
    log_path: Option<&Path>,
    dry_run: bool,
    partition: Option<&str>,
    format: OutputFormat,
) -> ExitCode {
    let mut volumes = match open_or_exit(image, format) {
        Ok(v) => v,
        Err(code) => return code,
    };

    if let Some(p) = partition {
        volumes.retain(|v| v.label == p);
        if volumes.is_empty() {
            eprintln!("partition '{p}' not found on this image");
            return ExitCode::from(EXIT_ERROR);
        }
    }

    let rescue_map = match rescue_map_path {
        Some(p) => match std::fs::read_to_string(p)
            .map_err(FcError::from)
            .and_then(|text| RescueMap::parse(&text).map_err(|e| FcError::Parse(e.to_string())))
        {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(EXIT_ERROR);
            }
        },
        None => None,
    };

    let multi = volumes.len() > 1;
    let mut results = Vec::new();
    let mut aggregate = ExtractSummary::default();
    let mut log = ExtractionLog::default();

    for v in &mut volumes {
        let dir = if multi {
            output.join(&v.label)
        } else {
            output.to_path_buf()
        };
        let opts = ExtractOptions {
            output_dir: dir,
            write_inf,
            dry_run,
            broken_dir_policy,
            bad_sector_policy,
            rescue_map: rescue_map.clone(),
        };
        let report = match volume_report(v) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(EXIT_ERROR);
            }
        };
        let summary = match volume_extract(v, &opts, &mut log) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(EXIT_ERROR);
            }
        };
        aggregate.files_extracted += summary.files_extracted;
        aggregate.files_skipped += summary.files_skipped;
        aggregate.dirs_created += summary.dirs_created;
        aggregate.total_bytes += summary.total_bytes;
        results.push((v.label.clone(), report, summary));
    }

    if let Some(p) = log_path
        && let Err(e) = log.write_to(p)
    {
        eprintln!("warning: failed to write log to {}: {e}", p.display());
    }

    print_extract_results(&results, &aggregate, format);
    ExitCode::from(EXIT_OK)
}

fn run_verify(image: &Path, format: OutputFormat, diagnostics_path: Option<&Path>) -> ExitCode {
    let mut volumes = match open_or_exit(image, format) {
        Ok(v) => v,
        Err(code) => return code,
    };

    let mut diag = Diagnostics::default();
    let mut aggregate = VerifyReport::default();
    let mut per_volume = Vec::new();
    for v in &mut volumes {
        match volume_verify(v, &mut diag) {
            Ok(r) => {
                aggregate.directories += r.directories;
                aggregate.files += r.files;
                aggregate.unreadable += r.unreadable;
                per_volume.push((v.label.clone(), r));
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(EXIT_ERROR);
            }
        }
    }

    print_verify_report(&aggregate, &per_volume, format);

    match diagnostics_path {
        Some(p) => {
            if let Err(e) = diag.write_jsonl_file(p) {
                eprintln!(
                    "warning: failed to write diagnostics to {}: {e}",
                    p.display()
                );
                return ExitCode::from(EXIT_ERROR);
            }
        }
        None => {
            if let Err(e) = diag.print_stderr() {
                eprintln!("warning: failed to write diagnostics: {e}");
            }
        }
    }
    ExitCode::from(EXIT_OK)
}

#[derive(Serialize)]
struct PartitionReport<'a> {
    partition: &'a str,
    #[serde(flatten)]
    disc: &'a DiscReport,
}

fn print_partition_reports(reports: &[(String, DiscReport)], format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let v: Vec<PartitionReport> = reports
                .iter()
                .map(|(label, r)| PartitionReport {
                    partition: label,
                    disc: r,
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
        }
        OutputFormat::Text => {
            for (i, (label, r)) in reports.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                println!("Partition {label}:");
                println!("{}", r.to_text());
            }
        }
    }
}

#[derive(Serialize)]
struct PartitionVerifyReport<'a> {
    partition: &'a str,
    directories: usize,
    files: usize,
    unreadable: usize,
}

fn print_verify_report(
    aggregate: &VerifyReport,
    per_volume: &[(String, VerifyReport)],
    format: OutputFormat,
) {
    match format {
        OutputFormat::Json => {
            let v: Vec<PartitionVerifyReport> = per_volume
                .iter()
                .map(|(label, r)| PartitionVerifyReport {
                    partition: label,
                    directories: r.directories,
                    files: r.files,
                    unreadable: r.unreadable,
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
        }
        OutputFormat::Text => {
            for (label, r) in per_volume {
                println!(
                    "partition {label}: directories={} files={} unreadable={}",
                    r.directories, r.files, r.unreadable
                );
            }
            println!(
                "total: directories={} files={} unreadable={}",
                aggregate.directories, aggregate.files, aggregate.unreadable
            );
        }
    }
}

fn print_not_recognised(format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&NotRecognised { recognized: false }).unwrap()
            );
        }
        OutputFormat::Text => {
            println!("not a recognised disc image (FileCore, AFS or DFS)");
        }
    }
}

#[derive(Serialize)]
struct PartitionExtractResult<'a> {
    partition: &'a str,
    #[serde(flatten)]
    disc: &'a DiscReport,
    files_extracted: u64,
    files_skipped: u64,
    dirs_created: u64,
    total_bytes: u64,
}

fn print_extract_results(
    results: &[(String, DiscReport, ExtractSummary)],
    aggregate: &ExtractSummary,
    format: OutputFormat,
) {
    match format {
        OutputFormat::Json => {
            let v: Vec<PartitionExtractResult> = results
                .iter()
                .map(|(label, report, s)| PartitionExtractResult {
                    partition: label,
                    disc: report,
                    files_extracted: s.files_extracted,
                    files_skipped: s.files_skipped,
                    dirs_created: s.dirs_created,
                    total_bytes: s.total_bytes,
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
        }
        OutputFormat::Text => {
            for (i, (label, report, s)) in results.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                println!("Partition {label}:");
                println!("{}", report.to_text());
                println!();
                println!("Files extracted: {}", s.files_extracted);
                println!("Files skipped:   {}", s.files_skipped);
                println!("Dirs created:    {}", s.dirs_created);
                println!("Total bytes:     {}", s.total_bytes);
            }
            if results.len() > 1 {
                println!();
                println!(
                    "Total: {} files extracted, {} files skipped, {} dirs, {} bytes",
                    aggregate.files_extracted,
                    aggregate.files_skipped,
                    aggregate.dirs_created,
                    aggregate.total_bytes
                );
            }
        }
    }
}
