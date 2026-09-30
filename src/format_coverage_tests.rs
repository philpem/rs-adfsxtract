//! Report-only format-coverage test.
//!
//! This does **not** assert a coverage threshold (CI should just report). It
//! encodes the disk-format matrix from `docs/FORMAT_COVERAGE.md` as structured
//! data, prints the current state, and enforces only internal invariants so the
//! matrix can't silently drift:
//!
//! - every `real` cell must name a fixture that actually exists in `data/`;
//! - cell identifiers are unique (a dimensional collision is a bug);
//! - a floor on real-media cells so we notice breadth regressions without
//!   failing the build on an uncovered edge.
//!
//! Ground truth is official-tool media; see `docs/FORMAT_COVERAGE.md` and
//! `tools/GOLDEN_AUTHORING.md`.

use std::path::Path;

const DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/data/");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Real,
    Synthetic,
    SyntheticOnly,
    Uncovered,
    Refused,
    /// Exercised against real media at runtime only (key-gated, never
    /// committed); no pinned hash/name is relied upon.
    RuntimeReal,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Real => "real",
            Status::Synthetic => "synthetic",
            Status::SyntheticOnly => "synthetic-only",
            Status::Uncovered => "uncovered",
            Status::Refused => "refused",
            Status::RuntimeReal => "real (runtime)",
        }
    }
}

struct Cell {
    id: &'static str,
    status: Status,
    fixture: Option<&'static str>,
    test: &'static str,
}

fn filecore_matrix() -> Vec<Cell> {
    vec![
        Cell {
            id: "adfs/s",
            status: Status::SyntheticOnly,
            fixture: None,
            test: "sml_geometry::tests::from_total_sectors_s* + S/M reference (golden pending)",
        },
        Cell {
            id: "adfs/m",
            status: Status::SyntheticOnly,
            fixture: None,
            test: "sml_geometry::tests::from_total_sectors_m* + S/M reference (golden pending)",
        },
        Cell {
            id: "adfs/l",
            status: Status::Real,
            fixture: Some("adfs640L.adl.gz"),
            test: "reference_media_tests",
        },
        Cell {
            id: "adfs/d",
            status: Status::Real,
            fixture: Some("adfs800D.adf.gz"),
            test: "reference_media_tests",
        },
        Cell {
            id: "adfs/e",
            status: Status::Real,
            fixture: Some("adfs800E.adf.gz"),
            test: "reference_media_tests",
        },
        Cell {
            id: "adfs/f",
            status: Status::Real,
            fixture: Some("adfs1600F.adf.gz"),
            test: "reference_media_tests",
        },
        Cell {
            id: "adfs/eplus",
            status: Status::SyntheticOnly,
            fixture: None,
            test: "testutil::big_dir_round_trip",
        },
        Cell {
            id: "adfs/fplus",
            status: Status::SyntheticOnly,
            fixture: None,
            test: "testutil::big_dir_round_trip",
        },
        Cell {
            id: "adfs/g-hd",
            status: Status::SyntheticOnly,
            fixture: None,
            test: "testutil::big_dir_round_trip",
        },
        Cell {
            id: "adfs/oldmap-hd",
            status: Status::Uncovered,
            fixture: None,
            test: "detect/mod old-map hard-disc branch (golden TBD)",
        },
        Cell {
            id: "adfs/newmap-seq",
            status: Status::Refused,
            fixture: None,
            test: "scenario_tests::newmap_sequential_track_order_refused",
        },
        Cell {
            id: "frag/2-frag",
            status: Status::Synthetic,
            fixture: None,
            test: "testutil::fragmented_file_reassembles_in_order",
        },
        Cell {
            id: "frag/gt2",
            status: Status::Synthetic,
            fixture: None,
            test: "testutil::random_fragmentation_reassembles_correctly_unseeded (1..=120 seeds)",
        },
        Cell {
            id: "frag/cross-zone",
            status: Status::Synthetic,
            fixture: None,
            test: "map_new::tests::cross_zone_span_is_continued_and_joined",
        },
        Cell {
            id: "frag/shared-offset",
            status: Status::SyntheticOnly,
            fixture: None,
            test: "build_new_map_disc / big-dir paths",
        },
        Cell {
            id: "frag/big-dir",
            status: Status::Synthetic,
            fixture: None,
            test: "testutil::random_fragmentation_in_big_directory",
        },
        Cell {
            id: "frag/free-chain",
            status: Status::Synthetic,
            fixture: None,
            test: "map_new::tests / random_fragmentation fuzz",
        },
        Cell {
            id: "frag/real",
            status: Status::RuntimeReal,
            fixture: None,
            test: "arcology_corpus_tests::hard_disc_fragmentation_against_arcology_corpus (key-gated; many real .dd discs contain fragmented files)",
        },
        Cell {
            id: "feat/boot-block-f",
            status: Status::Real,
            fixture: Some("adfs1600F.adf.gz"),
            test: "reference_media_tests::metadata_matches_known_values",
        },
        Cell {
            id: "feat/boot-block-absent",
            status: Status::Real,
            fixture: Some("adfs800E.adf.gz"),
            test: "reference_media_tests::metadata_matches_known_values",
        },
        Cell {
            id: "feat/zone-check",
            status: Status::Real,
            fixture: Some("adfs1600F.adf.gz"),
            test: "reference_media_tests",
        },
        Cell {
            id: "feat/dir-check-byte",
            status: Status::Real,
            fixture: Some("adfs640L.adl.gz"),
            test: "reference_media_tests",
        },
        Cell {
            id: "feat/sml-attr-in-name",
            status: Status::Synthetic,
            fixture: None,
            test: "testutil::build_old_map_disc / dir_old",
        },
        Cell {
            id: "feat/dosfs-charset",
            status: Status::Synthetic,
            fixture: None,
            test: "scenario_tests::dosfs_characters_translated_end_to_end",
        },
        Cell {
            id: "feat/inf-sidecar",
            status: Status::Synthetic,
            fixture: None,
            test: "scenario_tests::inf_sidecar_written_when_requested",
        },
        Cell {
            id: "feat/bad-sector",
            status: Status::Synthetic,
            fixture: None,
            test: "scenario_tests::bad_sector_*",
        },
        Cell {
            id: "feat/broken-dir",
            status: Status::Synthetic,
            fixture: None,
            test: "scenario_tests::broken_directory_*",
        },
    ]
}

fn dfs_matrix() -> Vec<Cell> {
    vec![
        Cell {
            id: "dfs/acorn-ssd",
            status: Status::Real,
            fixture: Some("apd01_ssd.ssd.gz"),
            test: "dfs_reference_media_tests::acorn_dfs_ssd_real_media",
        },
        Cell {
            id: "dfs/acorn-dsd",
            status: Status::Real,
            fixture: Some("8bs0_dsd.dsd.gz"),
            test: "dfs_reference_media_tests::acorn_dfs_dsd_real_media_interleave",
        },
        Cell {
            id: "dfs/watford-62",
            status: Status::RuntimeReal,
            fixture: None,
            test: "external target (BGAME1_A.BBC, genuine Watford 62-file disc) + testutil::watford_extension_beyond_declared_total_is_extracted (regression for the total_sectors/image-size divergence bug)",
        },
        Cell {
            id: "dfs/solidisk",
            status: Status::RuntimeReal,
            fixture: None,
            test: "external target (Eagle_Empire.ssd, a single-density disc recovered from a Solidisk DDFS original); true double-density Solidisk DDFS geometry is an unsupported variant",
        },
        Cell {
            id: "dfs/hadfs",
            status: Status::RuntimeReal,
            fixture: None,
            test: "mdfs.net Software/HADFS/System.ssd (HADFS, unsupported variant; must not crash on DFS detection)",
        },
        Cell {
            id: "dfs/dirchar-locked",
            status: Status::Real,
            fixture: Some("apd01_ssd.ssd.gz"),
            test: "dfs_reference_media_tests::acorn_dfs_ssd_real_media (G/U dir chars)",
        },
        Cell {
            id: "dfs/bounds",
            status: Status::Synthetic,
            fixture: None,
            test: "scenario_tests::dfs_broken_entry_*",
        },
    ]
}

fn print_major(name: &str, cells: &[Cell]) -> (usize, usize, usize) {
    let mut real = 0;
    let mut runtime = 0;
    let mut uncovered = 0;
    println!("## {name}");
    for c in cells {
        match c.status {
            Status::Real => real += 1,
            Status::RuntimeReal => runtime += 1,
            Status::Uncovered => uncovered += 1,
            _ => {}
        }
        let f = c.fixture.map(|f| format!(" [{f}]")).unwrap_or_default();
        println!("  {:<28} {:<14}{}", c.id, c.status.label(), f);
        println!("        -> {}", c.test);
    }
    println!();
    (real, runtime, uncovered)
}

#[test]
fn format_coverage_matrix_reports_and_is_internally_consistent() {
    // Assemble and print the full matrix (report-only).
    let filecore = filecore_matrix();
    let dfs = dfs_matrix();
    let (fc_real, fc_runtime, fc_uncovered) = print_major("FileCore (ADFS)", &filecore);
    let (dfs_real, dfs_runtime, dfs_uncovered) = print_major("DFS", &dfs);

    // Invariant 1: cell identifiers are unique across both matrices.
    let mut seen = std::collections::HashSet::new();
    for c in filecore.iter().chain(dfs.iter()) {
        assert!(seen.insert(c.id), "duplicate cell id: {}", c.id);
    }

    // Invariant 2: every `real` cell must reference a fixture present on disk.
    for c in filecore.iter().chain(dfs.iter()) {
        if c.status == Status::Real {
            let fixture = c.fixture.expect("real cell must name a fixture");
            let path = Path::new(DATA_DIR).join(fixture);
            assert!(
                path.exists(),
                "real cell {} references missing fixture {}",
                c.id,
                path.display()
            );
        }
    }

    // Invariant 3: a floor on real-media breadth. Low enough to pass today but
    // high enough that a format-breadth regression is conspicuous in CI.
    let real_total = fc_real + dfs_real;
    assert!(
        real_total >= 10,
        "real-media format coverage dropped to {real_total} (floor is 10)"
    );

    // Summary line so CI visibly surfaces uncovered cells.
    println!(
        "COVERAGE SUMMARY: real={real_total} runtime_real={} uncovered_filecore={fc_uncovered} uncovered_dfs={dfs_uncovered}",
        fc_runtime + dfs_runtime
    );
}
