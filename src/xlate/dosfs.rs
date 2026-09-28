//! DOSFS-style filename translation, mirroring DIM's `BBCtoWin`/`WintoBBC`
//! behaviour (character table only - this is an independent implementation).

/// RISC OS character -> host character. `.` is RISC OS's path separator,
/// so this is applied per path component, not to a whole path.
const RISCOS_TO_HOST: [(char, char); 7] = [
    ('/', '.'),
    ('?', '#'),
    ('<', '$'),
    ('>', '^'),
    ('+', '&'),
    ('=', '@'),
    (';', '%'),
];

pub fn riscos_to_host_char(c: char) -> char {
    for &(from, to) in &RISCOS_TO_HOST {
        if c == from {
            return to;
        }
    }
    c
}

pub fn host_to_riscos_char(c: char) -> char {
    for &(from, to) in &RISCOS_TO_HOST {
        if c == to {
            return from;
        }
    }
    c
}

/// Characters still illegal on common host filesystems after the DOSFS
/// swap above; replaced with a space (defensive backstop, matches DIM's
/// `ValidateWinFilename`).
const HOST_ILLEGAL: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|', '\0'];

/// Translates one RISC OS leafname (not a whole path) to a host-safe name.
/// Returns `(name, was_substituted)` - see [`leafname_to_host`]'s doc
/// comment on why a substitution can happen and why callers must not treat
/// it silently.
pub fn leafname_to_host(riscos_name: &str) -> (String, bool) {
    let host: String = riscos_name
        .chars()
        .map(riscos_to_host_char)
        .map(|c| if HOST_ILLEGAL.contains(&c) { ' ' } else { c })
        .collect();

    // `Path::join` treats `.`/`..` as "this directory"/"parent directory",
    // not a literal name, and an empty name collides with the parent
    // directory itself when joined. Nothing in the on-disk format forbids
    // a name field from containing arbitrary bytes, so a corrupted or
    // deliberately crafted disc image can produce one of these: a RISC OS
    // name of literally "." or "/" (a single byte) survives translation
    // unchanged or maps straight to ".", and "//" - two bytes, trivially
    // fits even DFS's 7-byte name field - becomes ".." via the swap above.
    // Left unhandled, extracting such an entry as a *directory* would let
    // every file inside it escape `--output` via `host_dir.join("..")`,
    // compounding with nesting depth. Substituted with an explicit,
    // obviously-synthetic name; the caller is expected to log this, since
    // it means the disc's structure is no longer being represented
    // faithfully on the host filesystem.
    match host.as_str() {
        "" => ("_empty_name_".to_string(), true),
        "." => ("_dot_".to_string(), true),
        ".." => ("_dotdot_".to_string(), true),
        _ => (host, false),
    }
}

pub fn leafname_to_riscos(host_name: &str) -> String {
    host_name.chars().map(host_to_riscos_char).collect()
}

/// Appends the `,fff` hex filetype suffix (guide §3.3's date-stamp rule)
/// when present, matching DIM's `GetWindowsFilename` convention. Directories
/// and untyped load/exec files get no suffix.
pub fn append_filetype_suffix(
    host_name: &str,
    filetype: Option<u16>,
    is_directory: bool,
) -> String {
    match (filetype, is_directory) {
        (Some(ft), false) => format!("{host_name},{ft:03x}"),
        _ => host_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_is_involutive() {
        for &(from, to) in &RISCOS_TO_HOST {
            assert_eq!(host_to_riscos_char(to), from);
            assert_eq!(riscos_to_host_char(from), to);
        }
    }

    #[test]
    fn leafname_round_trip() {
        let riscos = "Fred/Bloggs?Was<Here>Ok+Now=Then;Now";
        let (host, substituted) = leafname_to_host(riscos);
        assert_eq!(host, "Fred.Bloggs#Was$Here^Ok&Now@Then%Now");
        assert!(!substituted);
        assert_eq!(leafname_to_riscos(&host), riscos);
    }

    #[test]
    fn dangerous_names_are_substituted_not_passed_through() {
        // "//" -> ".." via the '/'->'.' swap - would otherwise let
        // `Path::join` escape the output directory.
        let (host, substituted) = leafname_to_host("//");
        assert_eq!(host, "_dotdot_");
        assert!(substituted);
        assert_ne!(host, "..");

        let (host, substituted) = leafname_to_host("/");
        assert_eq!(host, "_dot_");
        assert!(substituted);

        let (host, substituted) = leafname_to_host("");
        assert_eq!(host, "_empty_name_");
        assert!(substituted);
    }

    #[test]
    fn filetype_suffix_rules() {
        assert_eq!(
            append_filetype_suffix("File", Some(0xFEB), false),
            "File,feb"
        );
        assert_eq!(append_filetype_suffix("Dir", Some(0xFEB), true), "Dir");
        assert_eq!(append_filetype_suffix("Plain", None, false), "Plain");
    }
}
