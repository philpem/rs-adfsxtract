//! RISC OS's 8-bit character set -> UTF-8. 0x00-0x7F is plain ASCII,
//! 0xA0-0xFF is identical to ISO-8859-1/Latin-1 (Unicode's first 256 code
//! points are Latin-1 by definition, so `byte as char` is exact there too).
//! 0x80-0x9F is RISC OS-specific and replaces what Latin-1/CP1252 use for
//! C1 control codes with typographic characters and window-furniture
//! glyphs, per <https://handwiki.org/wiki/RISC_OS_character_set>.
//!
//! DIM (the reference tool this extractor mirrors DOSFS behaviour from) has
//! no such table at all - it just strips the top bit - so this table is not
//! derived from DIM.

/// Maps one RISC OS-charset byte to its Unicode character.
pub fn decode_byte(b: u8) -> char {
    match b {
        0x80 => '\u{20AC}', // EURO SIGN
        0x81 => '\u{0174}', // LATIN CAPITAL LETTER W WITH CIRCUMFLEX
        0x82 => '\u{0175}', // LATIN SMALL LETTER W WITH CIRCUMFLEX
        // 0x83/0x84 are RISC OS window-furniture glyphs (resize/close
        // icons) with no real Unicode text equivalent.
        0x83 => '\u{FFFD}',
        0x84 => '\u{FFFD}',
        0x85 => '\u{0176}', // LATIN CAPITAL LETTER Y WITH CIRCUMFLEX
        0x86 => '\u{0177}', // LATIN SMALL LETTER Y WITH CIRCUMFLEX
        // Subscript-8-superscript-7 combination glyph; source notes it was
        // never proposed for Unicode.
        0x87 => '\u{FFFD}',
        // Scroll arrows. The source table's exact code points for this
        // range didn't check out against their own Unicode names (e.g. one
        // fetched value was U+21A6 RIGHTWARDS ARROW FROM BAR for what's
        // documented as "leftwards"), so plain directional arrows are used
        // here instead - correct semantics, not necessarily the original
        // glyph shape.
        0x88 => '\u{2190}', // LEFTWARDS ARROW
        0x89 => '\u{2192}', // RIGHTWARDS ARROW
        0x8A => '\u{2193}', // DOWNWARDS ARROW
        0x8B => '\u{2191}', // UPWARDS ARROW
        0x8C => '\u{2026}', // HORIZONTAL ELLIPSIS
        0x8D => '\u{2122}', // TRADE MARK SIGN
        0x8E => '\u{2030}', // PER MILLE SIGN
        0x8F => '\u{2022}', // BULLET
        0x90 => '\u{2018}', // LEFT SINGLE QUOTATION MARK
        0x91 => '\u{2019}', // RIGHT SINGLE QUOTATION MARK
        0x92 => '\u{2039}', // SINGLE LEFT-POINTING ANGLE QUOTATION MARK
        0x93 => '\u{203A}', // SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
        0x94 => '\u{201C}', // LEFT DOUBLE QUOTATION MARK
        0x95 => '\u{201D}', // RIGHT DOUBLE QUOTATION MARK
        0x96 => '\u{201E}', // DOUBLE LOW-9 QUOTATION MARK
        0x97 => '\u{2013}', // EN DASH
        0x98 => '\u{2014}', // EM DASH
        0x99 => '\u{2212}', // MINUS SIGN
        0x9A => '\u{0152}', // LATIN CAPITAL LIGATURE OE
        0x9B => '\u{0153}', // LATIN SMALL LIGATURE OE
        0x9C => '\u{2020}', // DAGGER
        0x9D => '\u{2021}', // DOUBLE DAGGER
        0x9E => '\u{FB01}', // LATIN SMALL LIGATURE FI
        0x9F => '\u{FB02}', // LATIN SMALL LIGATURE FL
        other => other as char,
    }
}

pub fn decode(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| decode_byte(b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_passthrough() {
        assert_eq!(decode(b"Hello, World!"), "Hello, World!");
    }

    #[test]
    fn latin1_range_matches_byte_value() {
        assert_eq!(decode_byte(0xE9), '\u{00E9}'); // e-acute
        assert_eq!(decode_byte(0xA0), '\u{00A0}'); // no-break space
    }

    #[test]
    fn riscos_specific_range() {
        assert_eq!(decode_byte(0x80), '€');
        assert_eq!(decode_byte(0x9E), 'ﬁ');
        assert_eq!(decode_byte(0x99), '\u{2212}');
    }
}
