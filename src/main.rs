use std::fs::File;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use serde::Serialize;

use acornfsextract::cli::{Cli, Command, OutputFormat};
use acornfsextract::error::FcError;
use acornfsextract::extract::log::ExtractionLog;
use acornfsextract::extract::report::{DiscReport, build_report};
use acornfsextract::extract::walker::{ExtractOptions, ExtractSummary, walk_and_extract};
use acornfsextract::format::filecore::FileCoreFs;
use acornfsextract::io::rescue::RescueMap;

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
            format,
        ),
    }
}

fn open_image(path: &Path) -> Result<FileCoreFs<File>, FcError> {
    let file = File::open(path).map_err(FcError::from)?;
    FileCoreFs::open(file)
}

fn run_info(image: &Path, format: OutputFormat) -> ExitCode {
    let mut fs = match open_image(image) {
        Ok(fs) => fs,
        Err(FcError::NotFileCore) => {
            print_not_recognised(format);
            return ExitCode::from(EXIT_NOT_RECOGNISED);
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_ERROR);
        }
    };

    match build_report(&mut fs) {
        Ok(report) => {
            print_report(&report, format);
            ExitCode::from(EXIT_OK)
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(EXIT_ERROR)
        }
    }
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
    format: OutputFormat,
) -> ExitCode {
    let mut fs = match open_image(image) {
        Ok(fs) => fs,
        Err(FcError::NotFileCore) => {
            print_not_recognised(format);
            return ExitCode::from(EXIT_NOT_RECOGNISED);
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_ERROR);
        }
    };

    let report = match build_report(&mut fs) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(EXIT_ERROR);
        }
    };

    let rescue_map = match rescue_map_path {
        Some(p) => match std::fs::read_to_string(p).map_err(FcError::from).and_then(|text| {
            RescueMap::parse(&text).map_err(|e| FcError::Parse(e.to_string()))
        }) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(EXIT_ERROR);
            }
        },
        None => None,
    };

    let opts = ExtractOptions {
        output_dir: output.to_path_buf(),
        write_inf,
        dry_run,
        broken_dir_policy,
        bad_sector_policy,
        rescue_map,
    };

    let mut log = ExtractionLog::default();
    let result = walk_and_extract(&mut fs, &opts, &mut log);

    if let Some(p) = log_path
        && let Err(e) = log.write_to(p) {
            eprintln!("warning: failed to write log to {}: {e}", p.display());
        }

    match result {
        Ok(summary) => {
            print_extract_result(&report, &summary, format);
            ExitCode::from(EXIT_OK)
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

fn print_not_recognised(format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&NotRecognised { recognized: false }).unwrap());
        }
        OutputFormat::Text => println!("not a recognised FileCore image"),
    }
}

fn print_report(report: &DiscReport, format: OutputFormat) {
    match format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(report).unwrap()),
        OutputFormat::Text => println!("{}", report.to_text()),
    }
}

#[derive(Serialize)]
struct ExtractResult<'a> {
    disc: &'a DiscReport,
    files_extracted: u64,
    files_skipped: u64,
    dirs_created: u64,
    total_bytes: u64,
}

fn print_extract_result(report: &DiscReport, summary: &ExtractSummary, format: OutputFormat) {
    match format {
        OutputFormat::Json => {
            let result = ExtractResult {
                disc: report,
                files_extracted: summary.files_extracted,
                files_skipped: summary.files_skipped,
                dirs_created: summary.dirs_created,
                total_bytes: summary.total_bytes,
            };
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
        OutputFormat::Text => {
            println!("{}", report.to_text());
            println!();
            println!("Files extracted: {}", summary.files_extracted);
            println!("Files skipped:   {}", summary.files_skipped);
            println!("Dirs created:    {}", summary.dirs_created);
            println!("Total bytes:     {}", summary.total_bytes);
        }
    }
}
