/// LSB-first bit reader over a byte slice.
///
/// FileCore's new-map fragment bitstream (guide §3.1) is packed LSB-first:
/// bit 0 of the stream is the LSB of the first byte. This is the opposite of
/// the MSB-first convention winnow's `binary::bits` module uses, so it's
/// hand-rolled rather than fought into winnow.
pub struct BitReader<'a> {
    data: &'a [u8],
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    pub fn len_bits(&self) -> usize {
        self.data.len() * 8
    }

    /// Single bit at `pos` (0 = LSB of `data[0]`). `None` if out of range.
    pub fn bit(&self, pos: usize) -> Option<u8> {
        if pos >= self.len_bits() {
            return None;
        }
        Some((self.data[pos / 8] >> (pos % 8)) & 1)
    }

    /// `nbits` bits starting at `pos`, LSB-first (result bit 0 = stream bit
    /// `pos`). `None` if any bit falls outside the buffer, or if `nbits`
    /// exceeds 64 - the result accumulator is a `u64`, and `idlen` (the
    /// usual caller, an untrusted on-disk field with no format-defined
    /// upper bound) could otherwise shift it out of range and panic.
    pub fn bits(&self, pos: usize, nbits: u32) -> Option<u64> {
        if nbits > 64 {
            return None;
        }
        let mut val: u64 = 0;
        for i in 0..nbits {
            val |= (self.bit(pos + i as usize)? as u64) << i;
        }
        Some(val)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lsb_first() {
        // byte 0 = 0b0000_0101 -> bits 0,2 set
        let data = [0b0000_0101u8, 0b0000_0010];
        let r = BitReader::new(&data);
        assert_eq!(r.bit(0), Some(1));
        assert_eq!(r.bit(1), Some(0));
        assert_eq!(r.bit(2), Some(1));
        // 3-bit value spanning within byte 0 starting at bit 1: bits 1,2,3 = 0,1,0 -> 0b010 = 2
        assert_eq!(r.bits(1, 3), Some(0b010));
        // value crossing the byte boundary: bits 6..10 (byte0 bits 6-7 = 00, byte1 bits 0-1 = 10)
        assert_eq!(r.bits(6, 4), Some(0b1000));
        // 200 would shift a u64 accumulator out of range and panic if not
        // rejected up front - simulates an untrusted idlen field.
        assert_eq!(r.bits(0, 200), None);
        assert_eq!(r.bits(15, 2), None);
    }
}
