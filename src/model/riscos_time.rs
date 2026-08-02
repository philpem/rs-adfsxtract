//! 40-bit centisecond timestamp (guide §3.3 + Glossary, reconciled): epoch
//! 1900-01-01 00:00:00. Load's low byte is the *high* byte of the count
//! (bits 39-32); exec holds the low 32 bits (bits 31-0) - not "the low 8
//! bits of exec", as the guide's Glossary entry for "Exec address"
//! ambiguously implies (see SPEC-ERRATA.md).

/// Seconds between 1900-01-01 00:00:00 and the Unix epoch (1970-01-01).
pub const EPOCH_DIFF_SECONDS: i64 = 2_208_988_800;
const CENTIS_PER_SECOND: u64 = 100;
const NANOS_PER_CENTI: u64 = 10_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiscOsTimestamp {
    pub unix_secs: i64,
    pub nanos: u32,
}

pub fn decode(load: u32, exec: u32) -> RiscOsTimestamp {
    let centis: u64 = ((load & 0xFF) as u64) << 32 | exec as u64;
    let secs_since_1900 = (centis / CENTIS_PER_SECOND) as i64;
    let nanos = ((centis % CENTIS_PER_SECOND) * NANOS_PER_CENTI) as u32;
    RiscOsTimestamp {
        unix_secs: secs_since_1900 - EPOCH_DIFF_SECONDS,
        nanos,
    }
}

fn centiseconds_from_unix(unix_secs: i64, nanos: u32) -> u64 {
    let secs_since_1900 = unix_secs + EPOCH_DIFF_SECONDS;
    (secs_since_1900 as u64) * CENTIS_PER_SECOND + (nanos as u64) / NANOS_PER_CENTI
}

/// Returns (load's low byte, exec) encoding `ts` as a 40-bit centisecond
/// count. The caller combines the load byte with a filetype into a full
/// load-address word (see `model::filetype::encode`).
pub fn encode_load_exec_bits(ts: RiscOsTimestamp) -> (u8, u32) {
    let centis = centiseconds_from_unix(ts.unix_secs, ts.nanos);
    (((centis >> 32) & 0xFF) as u8, (centis & 0xFFFF_FFFF) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let ts = RiscOsTimestamp {
            unix_secs: 1_700_000_000,
            nanos: 0,
        };
        let (load_low, exec) = encode_load_exec_bits(ts);
        let load = 0xFFF0_0000u32 | (load_low as u32);
        let decoded = decode(load, exec);
        assert_eq!(decoded.unix_secs, ts.unix_secs);
    }

    #[test]
    fn epoch_1900_decodes_to_negative_unix_time() {
        let decoded = decode(0xFFF0_0000, 0);
        assert_eq!(decoded.unix_secs, -EPOCH_DIFF_SECONDS);
    }
}
