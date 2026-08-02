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
pub fn leafname_to_host(riscos_name: &str) -> String {
    riscos_name
        .chars()
        .map(riscos_to_host_char)
        .map(|c| if HOST_ILLEGAL.contains(&c) { ' ' } else { c })
        .collect()
}

pub fn leafname_to_riscos(host_name: &str) -> String {
    host_name.chars().map(host_to_riscos_char).collect()
}

/// Appends the `,fff` hex filetype suffix (guide §3.3's date-stamp rule)
/// when present, matching DIM's `GetWindowsFilename` convention. Directories
/// and untyped load/exec files get no suffix.
pub fn append_filetype_suffix(host_name: &str, filetype: Option<u16>, is_directory: bool) -> String {
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
        let host = leafname_to_host(riscos);
        assert_eq!(host, "Fred.Bloggs#Was$Here^Ok&Now@Then%Now");
        assert_eq!(leafname_to_riscos(&host), riscos);
    }

    #[test]
    fn filetype_suffix_rules() {
        assert_eq!(append_filetype_suffix("File", Some(0xFEB), false), "File,feb");
        assert_eq!(append_filetype_suffix("Dir", Some(0xFEB), true), "Dir");
        assert_eq!(append_filetype_suffix("Plain", None, false), "Plain");
    }
}
