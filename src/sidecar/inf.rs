//! `.inf` sidecar writer, stardot forum format (per the user's decisions):
//! bare (unprefixed) hex fields so Arcology's existing `_parse_inf_line`
//! (`int(x, 16)`) can parse them without a worker-side patch, and the
//! filename field carries the *original* RISC OS name (matching DIM's
//! `CreateINFFile` and what Arcology's `process_inf_sidecars` expects),
//! not the DOSFS-translated host name.
//!
//! Percent-encoding operates on the UTF-8 bytes of the (already
//! charset-decoded) name, since by this point the original RISC OS byte
//! sequence is no longer available - DIM's own encoding operates on raw
//! 8-bit RISC OS bytes instead, so this is not byte-for-byte identical to
//! DIM's output, just format-compatible.

fn needs_quoting(name: &str) -> bool {
    name.is_empty()
        || name.starts_with('"')
        || name
            .bytes()
            .any(|b| !(0x21..=0x7E).contains(&b) || b == b'%' || b == b'"')
}

pub fn encode_filename(name: &str) -> String {
    if !needs_quoting(name) {
        return name.to_string();
    }
    let mut out = String::from("\"");
    for b in name.bytes() {
        match b {
            b'%' => out.push_str("%25"),
            b'"' => out.push_str("%22"),
            0x21..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out.push('"');
    out
}

pub struct InfFields<'a> {
    pub riscos_name: &'a str,
    pub load: u32,
    pub exec: u32,
    pub length: u64,
    pub attrs: u32,
    pub crc32: Option<u32>,
    pub datetime_unix_secs: Option<i64>,
}

pub fn build_inf_line(fields: &InfFields) -> String {
    let mut s = format!(
        "{} {:08x} {:08x} {:08x} {:02x}",
        encode_filename(fields.riscos_name),
        fields.load,
        fields.exec,
        fields.length,
        fields.attrs & 0xFF,
    );
    if let Some(crc) = fields.crc32 {
        s.push_str(&format!(" CRC32={crc:08X}"));
    }
    if let Some(secs) = fields.datetime_unix_secs {
        s.push_str(&format!(
            " DATETIME={}",
            crate::util::format_inf_datetime(secs)
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_name_unquoted() {
        assert_eq!(encode_filename("MyFile"), "MyFile");
    }

    #[test]
    fn name_with_space_quoted_and_encoded() {
        assert_eq!(encode_filename("My File"), "\"My%20File\"");
    }

    #[test]
    fn build_line_bare_hex() {
        let fields = InfFields {
            riscos_name: "File",
            load: 0xFFF0_0FEB,
            exec: 0x1234_5678,
            length: 0x100,
            attrs: 0x03,
            crc32: Some(0xDEADBEEF),
            datetime_unix_secs: Some(0),
        };
        let line = build_inf_line(&fields);
        assert_eq!(
            line,
            "File fff00feb 12345678 00000100 03 CRC32=DEADBEEF DATETIME=19700101000000"
        );
        assert!(!line.contains('&'));
    }
}
