use super::riscos_time::{RiscOsTimestamp, encode_load_exec_bits};

/// Decoded interpretation of a directory entry's load/exec address pair
/// (guide §3.3): if the top 12 bits of `load` are all set, the entry is
/// date-stamped and carries a 12-bit filetype; otherwise load/exec is a
/// literal (legacy) memory load/execution address pair with neither.
#[derive(Debug, Clone, Copy)]
pub struct LoadExec {
    pub filetype: Option<u16>,
    pub timestamp: Option<RiscOsTimestamp>,
    pub raw_load: u32,
    pub raw_exec: u32,
}

pub fn decode(load: u32, exec: u32) -> LoadExec {
    if (load >> 20) == 0xFFF {
        let filetype = ((load >> 8) & 0xFFF) as u16;
        LoadExec {
            filetype: Some(filetype),
            timestamp: Some(super::riscos_time::decode(load, exec)),
            raw_load: load,
            raw_exec: exec,
        }
    } else {
        LoadExec {
            filetype: None,
            timestamp: None,
            raw_load: load,
            raw_exec: exec,
        }
    }
}

/// Builds a date-stamped load/exec pair for a given filetype and timestamp
/// (used by the synthetic test-image builder).
pub fn encode(filetype: u16, ts: RiscOsTimestamp) -> (u32, u32) {
    let (load_low, exec) = encode_load_exec_bits(ts);
    let load = 0xFFF0_0000u32 | (((filetype as u32) & 0xFFF) << 8) | load_low as u32;
    (load, exec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_typed_dated_file() {
        let (load, exec) = encode(
            0xFEB,
            RiscOsTimestamp {
                unix_secs: 1_600_000_000,
                nanos: 0,
            },
        );
        let d = decode(load, exec);
        assert_eq!(d.filetype, Some(0xFEB));
        assert_eq!(d.timestamp.unwrap().unix_secs, 1_600_000_000);
    }

    #[test]
    fn plain_load_exec_has_no_filetype() {
        let d = decode(0x8000, 0x8020);
        assert!(d.filetype.is_none());
        assert!(d.timestamp.is_none());
    }
}
