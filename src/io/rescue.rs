use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeStatus {
    Good,
    Bad,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadSectorPolicy {
    Skip,
    NullFill,
    MarkerFill,
}

/// Repeating fill pattern for `MarkerFill`, chosen to be obviously
/// artificial when viewed in a hex editor (distinct from real zero data).
pub const MARKER_PATTERN: &[u8] = b"BAD SECTOR! ";

pub fn fill_marker(buf: &mut [u8]) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b = MARKER_PATTERN[i % MARKER_PATTERN.len()];
    }
}

#[derive(Debug)]
pub struct RescueMapError(pub String);

impl fmt::Display for RescueMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid ddrescue mapfile: {}", self.0)
    }
}
impl std::error::Error for RescueMapError {}

fn parse_int(tok: &str) -> Result<u64, RescueMapError> {
    let cleaned: String = tok.chars().filter(|&c| c != '_').collect();
    let (digits, radix) = if let Some(rest) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        (rest, 16)
    } else if cleaned.len() > 1 && cleaned.starts_with('0') {
        (&cleaned[1..], 8)
    } else {
        (cleaned.as_str(), 10)
    };
    u64::from_str_radix(digits, radix).map_err(|_| RescueMapError(format!("bad integer '{tok}'")))
}

fn parse_status(tok: &str) -> Result<RangeStatus, RescueMapError> {
    match tok {
        "+" => Ok(RangeStatus::Good),
        "?" | "*" | "/" | "-" => Ok(RangeStatus::Bad),
        other => Err(RescueMapError(format!("unknown status char '{other}'"))),
    }
}

/// A parsed ddrescue/gddrescue mapfile: a sorted set of non-overlapping
/// byte ranges, each marked good or bad. Bytes not covered by any range are
/// treated as good (ddrescue mapfiles are normally exhaustive, but treating
/// gaps as good is the safe default for a partial/hand-edited mapfile).
#[derive(Debug, Clone, Default)]
pub struct RescueMap {
    ranges: Vec<(u64, u64, RangeStatus)>,
}

impl RescueMap {
    pub fn parse(text: &str) -> Result<Self, RescueMapError> {
        let mut ranges = Vec::new();
        let mut seen_status_line = false;
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if !seen_status_line {
                seen_status_line = true;
                if parts.len() < 2 {
                    return Err(RescueMapError("expected status line".into()));
                }
                continue;
            }
            if parts.len() != 3 {
                return Err(RescueMapError(format!(
                    "expected 3 fields on data line, got {}",
                    parts.len()
                )));
            }
            let start = parse_int(parts[0])?;
            let len = parse_int(parts[1])?;
            let status = parse_status(parts[2])?;
            ranges.push((start, len, status));
        }
        ranges.sort_by_key(|r| r.0);
        Ok(Self { ranges })
    }

    /// Splits `[start, start+len)` into contiguous sub-segments according to
    /// the overlapping map ranges. Uncovered gaps are reported as `Good`.
    pub fn segments(&self, start: u64, len: u64) -> Vec<(u64, u64, RangeStatus)> {
        if len == 0 {
            return Vec::new();
        }
        let end = start + len;
        let mut out = Vec::new();
        let mut cursor = start;
        let idx = self.ranges.partition_point(|r| r.0 + r.1 <= start);
        for &(rstart, rlen, status) in &self.ranges[idx..] {
            let rend = rstart + rlen;
            if rstart >= end {
                break;
            }
            if rend <= cursor {
                continue;
            }
            if rstart > cursor {
                out.push((cursor, rstart - cursor, RangeStatus::Good));
                cursor = rstart;
            }
            let seg_end = rend.min(end);
            out.push((cursor, seg_end - cursor, status));
            cursor = seg_end;
        }
        if cursor < end {
            out.push((cursor, end - cursor, RangeStatus::Good));
        }
        out
    }

    pub fn all_good(&self, start: u64, len: u64) -> bool {
        self.segments(start, len)
            .iter()
            .all(|&(_, _, s)| s == RangeStatus::Good)
    }

    pub fn all_bad(&self, start: u64, len: u64) -> bool {
        let segs = self.segments(start, len);
        !segs.is_empty() && segs.iter().all(|&(_, _, s)| s == RangeStatus::Bad)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# mapfile. rescued: 100%
0x00120000     +               1
0x00000000  0x00117000  +
0x00117000  0x00000200  -
0x00117200  0x00000e00  +
";

    #[test]
    fn parses_and_segments() {
        let map = RescueMap::parse(SAMPLE).unwrap();
        assert!(map.all_good(0, 0x1000));
        assert!(map.all_bad(0x117000, 0x200));
        let segs = map.segments(0x116f00, 0x400);
        assert_eq!(
            segs,
            vec![
                (0x116f00, 0x100, RangeStatus::Good),
                (0x117000, 0x200, RangeStatus::Bad),
                (0x117200, 0x100, RangeStatus::Good),
            ]
        );
    }

    #[test]
    fn uncovered_gap_is_good() {
        let map = RescueMap::parse("0 + 1\n0x1000 0x100 -\n").unwrap();
        assert!(map.all_good(0, 0x1000));
    }
}
