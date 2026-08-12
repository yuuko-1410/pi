//! Text utilities for terminal rendering, port of
//! `packages/tui/src/utils.ts`.
//!
//! Documented differences (all approximate, sufficient for terminal width
//! accounting on realistic text):
//! - `Intl.Segmenter` grapheme segmentation → hand-written segmentation:
//!   base char + combining marks, ZWJ sequences, VS16, and regional
//!   indicator pairs. No locale-aware rules.
//! - `get-east-asian-width` → hand-written East Asian Width table covering
//!   the common wide/fullwidth ranges (CJK, Hangul, Hiragana, Katakana,
//!   fullwidth forms, emoji).
//! - Unicode property regexes (Default_Ignorable/Control/Mark/RGI_Emoji)
//!   → approximate scalar ranges.

use std::collections::HashMap;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Unicode helpers
// ---------------------------------------------------------------------------

/// Combining mark ranges (Mn/Mc/Me, approximate).
fn is_mark(cp: u32) -> bool {
    (cp >= 0x0300 && cp <= 0x036F)
        || (cp >= 0x0483 && cp <= 0x0489)
        || (cp >= 0x0591 && cp <= 0x05BD)
        || (cp >= 0x05BF && cp <= 0x05BF)
        || (cp >= 0x05C1 && cp <= 0x05C2)
        || (cp >= 0x05C4 && cp <= 0x05C5)
        || (cp >= 0x0610 && cp <= 0x061A)
        || (cp >= 0x064B && cp <= 0x065F)
        || (cp >= 0x0670 && cp <= 0x0670)
        || (cp >= 0x06D6 && cp <= 0x06DC)
        || (cp >= 0x06DF && cp <= 0x06E4)
        || (cp >= 0x06E7 && cp <= 0x06E8)
        || (cp >= 0x06EA && cp <= 0x06ED)
        || (cp >= 0x0711 && cp <= 0x0711)
        || (cp >= 0x0730 && cp <= 0x074A)
        || (cp >= 0x07A6 && cp <= 0x07B0)
        || (cp >= 0x07EB && cp <= 0x07F3)
        || (cp >= 0x0816 && cp <= 0x0819)
        || (cp >= 0x081B && cp <= 0x0823)
        || (cp >= 0x0825 && cp <= 0x0827)
        || (cp >= 0x0829 && cp <= 0x082D)
        || (cp >= 0x0859 && cp <= 0x085B)
        || (cp >= 0x08D3 && cp <= 0x08E1)
        || (cp >= 0x08E3 && cp <= 0x0902)
        || (cp >= 0x093A && cp <= 0x093A)
        || (cp >= 0x093C && cp <= 0x093C)
        || (cp >= 0x0941 && cp <= 0x0948)
        || (cp >= 0x094D && cp <= 0x094D)
        || (cp >= 0x0951 && cp <= 0x0957)
        || (cp >= 0x0962 && cp <= 0x0963)
        || (cp >= 0x0981 && cp <= 0x0981)
        || (cp >= 0x09BC && cp <= 0x09BC)
        || (cp >= 0x09C1 && cp <= 0x09C4)
        || (cp >= 0x09CD && cp <= 0x09CD)
        || (cp >= 0x0A01 && cp <= 0x0A02)
        || (cp >= 0x0A3C && cp <= 0x0A3C)
        || (cp >= 0x0A41 && cp <= 0x0A42)
        || (cp >= 0x0A47 && cp <= 0x0A48)
        || (cp >= 0x0A4B && cp <= 0x0A4D)
        || (cp >= 0x0A51 && cp <= 0x0A51)
        || (cp >= 0x0A70 && cp <= 0x0A71)
        || (cp >= 0x0A75 && cp <= 0x0A75)
        || (cp >= 0x0A81 && cp <= 0x0A82)
        || (cp >= 0x0ABC && cp <= 0x0ABC)
        || (cp >= 0x0AC1 && cp <= 0x0AC5)
        || (cp >= 0x0AC7 && cp <= 0x0AC8)
        || (cp >= 0x0ACD && cp <= 0x0ACD)
        || (cp >= 0x0AE2 && cp <= 0x0AE3)
        || (cp >= 0x0B01 && cp <= 0x0B01)
        || (cp >= 0x0B3C && cp <= 0x0B3C)
        || (cp >= 0x0B3F && cp <= 0x0B3F)
        || (cp >= 0x0B41 && cp <= 0x0B44)
        || (cp >= 0x0B4D && cp <= 0x0B4D)
        || (cp >= 0x0B55 && cp <= 0x0B56)
        || (cp >= 0x0B62 && cp <= 0x0B63)
        || (cp >= 0x0B82 && cp <= 0x0B82)
        || (cp >= 0x0BC0 && cp <= 0x0BC0)
        || (cp >= 0x0BCD && cp <= 0x0BCD)
        || (cp >= 0x0C00 && cp <= 0x0C00)
        || (cp >= 0x0C3E && cp <= 0x0C40)
        || (cp >= 0x0C46 && cp <= 0x0C48)
        || (cp >= 0x0C4A && cp <= 0x0C4D)
        || (cp >= 0x0C55 && cp <= 0x0C56)
        || (cp >= 0x0C62 && cp <= 0x0C63)
        || (cp >= 0x0C81 && cp <= 0x0C81)
        || (cp >= 0x0CBC && cp <= 0x0CBC)
        || (cp >= 0x0CBF && cp <= 0x0CBF)
        || (cp >= 0x0CC6 && cp <= 0x0CC6)
        || (cp >= 0x0CCC && cp <= 0x0CCD)
        || (cp >= 0x0CE2 && cp <= 0x0CE3)
        || (cp >= 0x0D00 && cp <= 0x0D01)
        || (cp >= 0x0D3B && cp <= 0x0D3C)
        || (cp >= 0x0D41 && cp <= 0x0D44)
        || (cp >= 0x0D4D && cp <= 0x0D4D)
        || (cp >= 0x0D62 && cp <= 0x0D63)
        || (cp >= 0x0DCA && cp <= 0x0DCA)
        || (cp >= 0x0DD2 && cp <= 0x0DD4)
        || (cp >= 0x0DD6 && cp <= 0x0DD6)
        || (cp >= 0x0E31 && cp <= 0x0E31)
        || (cp >= 0x0E34 && cp <= 0x0E3A)
        || (cp >= 0x0E47 && cp <= 0x0E4E)
        || (cp >= 0x0EB1 && cp <= 0x0EB1)
        || (cp >= 0x0EB4 && cp <= 0x0EB9)
        || (cp >= 0x0EBB && cp <= 0x0EBC)
        || (cp >= 0x0EC8 && cp <= 0x0ECD)
        || (cp >= 0x0F18 && cp <= 0x0F19)
        || (cp >= 0x0F35 && cp <= 0x0F35)
        || (cp >= 0x0F37 && cp <= 0x0F37)
        || (cp >= 0x0F39 && cp <= 0x0F39)
        || (cp >= 0x0F71 && cp <= 0x0F7E)
        || (cp >= 0x0F80 && cp <= 0x0F84)
        || (cp >= 0x0F86 && cp <= 0x0F87)
        || (cp >= 0x0F8D && cp <= 0x0F97)
        || (cp >= 0x0F99 && cp <= 0x0FBC)
        || (cp >= 0x0FC6 && cp <= 0x0FC6)
        || (cp >= 0x102D && cp <= 0x1030)
        || (cp >= 0x1032 && cp <= 0x1037)
        || (cp >= 0x1039 && cp <= 0x103A)
        || (cp >= 0x103D && cp <= 0x103E)
        || (cp >= 0x1058 && cp <= 0x1059)
        || (cp >= 0x105E && cp <= 0x1060)
        || (cp >= 0x1071 && cp <= 0x1074)
        || (cp >= 0x1082 && cp <= 0x1082)
        || (cp >= 0x1085 && cp <= 0x1086)
        || (cp >= 0x108D && cp <= 0x108D)
        || (cp >= 0x109D && cp <= 0x109D)
        || (cp >= 0x135D && cp <= 0x135F)
        || (cp >= 0x1712 && cp <= 0x1714)
        || (cp >= 0x1732 && cp <= 0x1734)
        || (cp >= 0x1752 && cp <= 0x1753)
        || (cp >= 0x1772 && cp <= 0x1773)
        || (cp >= 0x17B4 && cp <= 0x17B5)
        || (cp >= 0x17B7 && cp <= 0x17BD)
        || (cp >= 0x17C6 && cp <= 0x17C6)
        || (cp >= 0x17C9 && cp <= 0x17D3)
        || (cp >= 0x17DD && cp <= 0x17DD)
        || (cp >= 0x180B && cp <= 0x180D)
        || (cp >= 0x1885 && cp <= 0x1886)
        || (cp >= 0x18A9 && cp <= 0x18A9)
        || (cp >= 0x1920 && cp <= 0x1922)
        || (cp >= 0x1927 && cp <= 0x1928)
        || (cp >= 0x1932 && cp <= 0x1932)
        || (cp >= 0x1939 && cp <= 0x193B)
        || (cp >= 0x1A17 && cp <= 0x1A18)
        || (cp >= 0x1A1B && cp <= 0x1A1B)
        || (cp >= 0x1A56 && cp <= 0x1A56)
        || (cp >= 0x1A58 && cp <= 0x1A5E)
        || (cp >= 0x1A60 && cp <= 0x1A60)
        || (cp >= 0x1A62 && cp <= 0x1A62)
        || (cp >= 0x1A65 && cp <= 0x1A6C)
        || (cp >= 0x1A73 && cp <= 0x1A7C)
        || (cp >= 0x1A7F && cp <= 0x1A7F)
        || (cp >= 0x1AB0 && cp <= 0x1ABE)
        || (cp >= 0x1B00 && cp <= 0x1B03)
        || (cp >= 0x1B34 && cp <= 0x1B34)
        || (cp >= 0x1B36 && cp <= 0x1B3A)
        || (cp >= 0x1B3C && cp <= 0x1B3C)
        || (cp >= 0x1B42 && cp <= 0x1B42)
        || (cp >= 0x1B6B && cp <= 0x1B73)
        || (cp >= 0x1B80 && cp <= 0x1B81)
        || (cp >= 0x1BA2 && cp <= 0x1BA5)
        || (cp >= 0x1BA8 && cp <= 0x1BA9)
        || (cp >= 0x1BAB && cp <= 0x1BAD)
        || (cp >= 0x1BE6 && cp <= 0x1BE6)
        || (cp >= 0x1BE8 && cp <= 0x1BE9)
        || (cp >= 0x1BED && cp <= 0x1BED)
        || (cp >= 0x1BEF && cp <= 0x1BF1)
        || (cp >= 0x1C2C && cp <= 0x1C33)
        || (cp >= 0x1C36 && cp <= 0x1C37)
        || (cp >= 0x1CD0 && cp <= 0x1CD2)
        || (cp >= 0x1CD4 && cp <= 0x1CE0)
        || (cp >= 0x1CE2 && cp <= 0x1CE8)
        || (cp >= 0x1CED && cp <= 0x1CED)
        || (cp >= 0x1CF4 && cp <= 0x1CF4)
        || (cp >= 0x1CF8 && cp <= 0x1CF9)
        || (cp >= 0x1DC0 && cp <= 0x1DF9)
        || (cp >= 0x1DFB && cp <= 0x1DFF)
        || (cp >= 0x20D0 && cp <= 0x20F0)
        || (cp >= 0x2CEF && cp <= 0x2CF1)
        || (cp >= 0x2D7F && cp <= 0x2D7F)
        || (cp >= 0x2DE0 && cp <= 0x2DFF)
        || (cp >= 0x302A && cp <= 0x302D)
        || (cp >= 0x3099 && cp <= 0x309A)
        || (cp >= 0xA66F && cp <= 0xA672)
        || (cp >= 0xA674 && cp <= 0xA67D)
        || (cp >= 0xA69E && cp <= 0xA69F)
        || (cp >= 0xA6F0 && cp <= 0xA6F1)
        || (cp >= 0xA802 && cp <= 0xA802)
        || (cp >= 0xA806 && cp <= 0xA806)
        || (cp >= 0xA80B && cp <= 0xA80B)
        || (cp >= 0xA825 && cp <= 0xA826)
        || (cp >= 0xA8C4 && cp <= 0xA8C5)
        || (cp >= 0xA8E0 && cp <= 0xA8F1)
        || (cp >= 0xA926 && cp <= 0xA92D)
        || (cp >= 0xA947 && cp <= 0xA951)
        || (cp >= 0xA980 && cp <= 0xA982)
        || (cp >= 0xA9B3 && cp <= 0xA9B3)
        || (cp >= 0xA9B6 && cp <= 0xA9B9)
        || (cp >= 0xA9BC && cp <= 0xA9BC)
        || (cp >= 0xA9E5 && cp <= 0xA9E5)
        || (cp >= 0xAA29 && cp <= 0xAA2E)
        || (cp >= 0xAA31 && cp <= 0xAA32)
        || (cp >= 0xAA35 && cp <= 0xAA36)
        || (cp >= 0xAA43 && cp <= 0xAA43)
        || (cp >= 0xAA4C && cp <= 0xAA4C)
        || (cp >= 0xAA7C && cp <= 0xAA7C)
        || (cp >= 0xAAB0 && cp <= 0xAAB0)
        || (cp >= 0xAAB2 && cp <= 0xAAB4)
        || (cp >= 0xAAB7 && cp <= 0xAAB8)
        || (cp >= 0xAABE && cp <= 0xAABF)
        || (cp >= 0xAAC1 && cp <= 0xAAC1)
        || (cp >= 0xAAEC && cp <= 0xAAED)
        || (cp >= 0xAAF6 && cp <= 0xAAF6)
        || (cp >= 0xABE5 && cp <= 0xABE5)
        || (cp >= 0xABE8 && cp <= 0xABE8)
        || (cp >= 0xABED && cp <= 0xABED)
        || (cp >= 0xFB1E && cp <= 0xFB1E)
        || (cp >= 0xFE00 && cp <= 0xFE0F)
        || (cp >= 0xFE20 && cp <= 0xFE2F)
        || (cp >= 0x101FD && cp <= 0x101FD)
        || (cp >= 0x102E0 && cp <= 0x102E0)
        || (cp >= 0x10376 && cp <= 0x1037A)
        || (cp >= 0x10A01 && cp <= 0x10A03)
        || (cp >= 0x10A05 && cp <= 0x10A06)
        || (cp >= 0x10A0C && cp <= 0x10A0F)
        || (cp >= 0x10A38 && cp <= 0x10A3A)
        || (cp >= 0x10A3F && cp <= 0x10A3F)
        || (cp >= 0x10AE5 && cp <= 0x10AE6)
        || (cp >= 0x11001 && cp <= 0x11001)
        || (cp >= 0x11038 && cp <= 0x11046)
        || (cp >= 0x1107F && cp <= 0x11081)
        || (cp >= 0x110B3 && cp <= 0x110B6)
        || (cp >= 0x110B9 && cp <= 0x110BA)
        || (cp >= 0x11100 && cp <= 0x11102)
        || (cp >= 0x11127 && cp <= 0x1112B)
        || (cp >= 0x1112D && cp <= 0x11134)
        || (cp >= 0x11173 && cp <= 0x11173)
        || (cp >= 0x11180 && cp <= 0x11181)
        || (cp >= 0x111B6 && cp <= 0x111BE)
        || (cp >= 0x111CA && cp <= 0x111CC)
        || (cp >= 0x1122F && cp <= 0x11231)
        || (cp >= 0x11234 && cp <= 0x11234)
        || (cp >= 0x11236 && cp <= 0x11237)
        || (cp >= 0x1123E && cp <= 0x1123E)
        || (cp >= 0x112DF && cp <= 0x112DF)
        || (cp >= 0x112E3 && cp <= 0x112EA)
        || (cp >= 0x11300 && cp <= 0x11301)
        || (cp >= 0x1133C && cp <= 0x1133C)
        || (cp >= 0x11340 && cp <= 0x11340)
        || (cp >= 0x11366 && cp <= 0x1136C)
        || (cp >= 0x11370 && cp <= 0x11374)
        || (cp >= 0x11438 && cp <= 0x1143F)
        || (cp >= 0x11442 && cp <= 0x11444)
        || (cp >= 0x11446 && cp <= 0x11446)
        || (cp >= 0x114B3 && cp <= 0x114B8)
        || (cp >= 0x114BA && cp <= 0x114BA)
        || (cp >= 0x114BF && cp <= 0x114C0)
        || (cp >= 0x114C2 && cp <= 0x114C3)
        || (cp >= 0x115B2 && cp <= 0x115B5)
        || (cp >= 0x115BC && cp <= 0x115BD)
        || (cp >= 0x115BF && cp <= 0x115C0)
        || (cp >= 0x115DC && cp <= 0x115DD)
        || (cp >= 0x11633 && cp <= 0x1163A)
        || (cp >= 0x1163D && cp <= 0x1163D)
        || (cp >= 0x1163F && cp <= 0x11640)
        || (cp >= 0x116AB && cp <= 0x116AB)
        || (cp >= 0x116AD && cp <= 0x116AD)
        || (cp >= 0x116B0 && cp <= 0x116B5)
        || (cp >= 0x116B7 && cp <= 0x116B7)
        || (cp >= 0x1171D && cp <= 0x1171F)
        || (cp >= 0x11722 && cp <= 0x11725)
        || (cp >= 0x11727 && cp <= 0x1172B)
        || (cp >= 0x11A01 && cp <= 0x11A06)
        || (cp >= 0x11A09 && cp <= 0x11A0A)
        || (cp >= 0x11A33 && cp <= 0x11A38)
        || (cp >= 0x11A3B && cp <= 0x11A3E)
        || (cp >= 0x11A47 && cp <= 0x11A47)
        || (cp >= 0x11A51 && cp <= 0x11A56)
        || (cp >= 0x11A59 && cp <= 0x11A5B)
        || (cp >= 0x11A8A && cp <= 0x11A96)
        || (cp >= 0x11A98 && cp <= 0x11A99)
        || (cp >= 0x11C30 && cp <= 0x11C36)
        || (cp >= 0x11C38 && cp <= 0x11C3D)
        || (cp >= 0x11C3F && cp <= 0x11C3F)
        || (cp >= 0x11C92 && cp <= 0x11CA7)
        || (cp >= 0x11CAA && cp <= 0x11CB0)
        || (cp >= 0x11CB2 && cp <= 0x11CB3)
        || (cp >= 0x11CB5 && cp <= 0x11CB6)
        || (cp >= 0x16AF0 && cp <= 0x16AF4)
        || (cp >= 0x16B30 && cp <= 0x16B36)
        || (cp >= 0x16F8F && cp <= 0x16F92)
        || (cp >= 0x1BC9D && cp <= 0x1BC9E)
        || (cp >= 0x1D165 && cp <= 0x1D169)
        || (cp >= 0x1D16D && cp <= 0x1D172)
        || (cp >= 0x1D17B && cp <= 0x1D182)
        || (cp >= 0x1D185 && cp <= 0x1D18B)
        || (cp >= 0x1D1AA && cp <= 0x1D1AD)
        || (cp >= 0x1D242 && cp <= 0x1D244)
        || (cp >= 0x1DA00 && cp <= 0x1DA36)
        || (cp >= 0x1DA3B && cp <= 0x1DA6C)
        || (cp >= 0x1DA75 && cp <= 0x1DA75)
        || (cp >= 0x1DA84 && cp <= 0x1DA84)
        || (cp >= 0x1DA9B && cp <= 0x1DA9F)
        || (cp >= 0x1DAA1 && cp <= 0x1DAAF)
        || (cp >= 0x1E000 && cp <= 0x1E006)
        || (cp >= 0x1E008 && cp <= 0x1E018)
        || (cp >= 0x1E01B && cp <= 0x1E021)
        || (cp >= 0x1E023 && cp <= 0x1E024)
        || (cp >= 0x1E026 && cp <= 0x1E02A)
        || (cp >= 0x1E8D0 && cp <= 0x1E8D6)
        || (cp >= 0x1E944 && cp <= 0x1E94A)
        || (cp >= 0xE0100 && cp <= 0xE01EF)
}

fn is_control(cp: u32) -> bool {
    (cp <= 0x1F) || (cp >= 0x7F && cp <= 0x9F)
}

/// Default_Ignorable_Code_Point approximation: soft hyphen, combining marks
/// are handled separately; here: ZWJ/ZWNJ, ZWSP, variation selectors, tags,
/// word joiner, BOM, Mongolian vowel separators.
fn is_default_ignorable(cp: u32) -> bool {
    cp == 0x00AD        // soft hyphen
        || cp == 0x034F // combining grapheme joiner
        || (cp >= 0x061C && cp <= 0x061C) // Arabic letter mark
        || cp == 0x115F || cp == 0x1160 || cp == 0x17B4 || cp == 0x17B5
        || cp == 0x180B || cp == 0x180C || cp == 0x180D || cp == 0x180E
        || (cp >= 0x200B && cp <= 0x200F)
        || cp == 0x202A || cp == 0x202B || cp == 0x202C || cp == 0x202D || cp == 0x202E
        || cp == 0x2060 || cp == 0x2061 || cp == 0x2062 || cp == 0x2063 || cp == 0x2064
        || cp == 0x2065 || cp == 0x2066 || cp == 0x2067 || cp == 0x2068 || cp == 0x2069
        || cp == 0x206A || cp == 0x206B || cp == 0x206C || cp == 0x206D || cp == 0x206E
        || cp == 0x206F
        || cp == 0xFEFF
        || (cp >= 0xFFF9 && cp <= 0xFFFB)
        || (cp >= 0xE0000 && cp <= 0xE0FFF) // tags
        || cp == 0xFE0F // VS16 handled by emoji logic; treat as ignorable base
}

/// East Asian Width: 2 for wide/fullwidth, 1 otherwise (approximate table).
fn east_asian_width(cp: u32) -> u32 {
    // CJK Unified Ideographs + extensions
    if (cp >= 0x1100 && cp <= 0x115F) // Hangul Jamo
        || (cp >= 0x2E80 && cp <= 0x303E) // CJK radicals, punctuation, kana
        || (cp >= 0x3041 && cp <= 0x33FF) // Hiragana..CJK compat
        || (cp >= 0x3400 && cp <= 0x4DBF) // CJK ext A
        || (cp >= 0x4E00 && cp <= 0x9FFF) // CJK unified
        || (cp >= 0xA000 && cp <= 0xA4CF) // Yi
        || (cp >= 0xA960 && cp <= 0xA97F) // Hangul Jamo ext A
        || (cp >= 0xAC00 && cp <= 0xD7A3) // Hangul syllables
        || (cp >= 0xF900 && cp <= 0xFAFF) // CJK compat ideographs
        || (cp >= 0xFE10 && cp <= 0xFE19) // vertical forms
        || (cp >= 0xFE30 && cp <= 0xFE52) // CJK compat forms
        || (cp >= 0xFE54 && cp <= 0xFE66)
        || (cp >= 0xFE68 && cp <= 0xFE6B)
        || (cp >= 0xFF00 && cp <= 0xFF60) // fullwidth forms
        || (cp >= 0xFFE0 && cp <= 0xFFE6)
        || (cp >= 0x16FE0 && cp <= 0x16FE4)
        || (cp >= 0x17000 && cp <= 0x187F7) // Tangut
        || (cp >= 0x18800 && cp <= 0x18CD5)
        || (cp >= 0x1B000 && cp <= 0x1B2FB) // Kana supplement
        || (cp >= 0x1F004 && cp <= 0x1F004)
        || (cp >= 0x1F0CF && cp <= 0x1F0CF)
        || (cp >= 0x1F18E && cp <= 0x1F18E)
        || (cp >= 0x1F191 && cp <= 0x1F19A)
        || (cp >= 0x1F200 && cp <= 0x1F320) // enclosure, emoji
        || (cp >= 0x1F32D && cp <= 0x1F335)
        || (cp >= 0x1F337 && cp <= 0x1F37C)
        || (cp >= 0x1F37E && cp <= 0x1F393)
        || (cp >= 0x1F3A0 && cp <= 0x1F3CA)
        || (cp >= 0x1F3CF && cp <= 0x1F3D3)
        || (cp >= 0x1F3E0 && cp <= 0x1F3F0)
        || (cp >= 0x1F3F4 && cp <= 0x1F3F4)
        || (cp >= 0x1F3F8 && cp <= 0x1F43E)
        || (cp >= 0x1F440 && cp <= 0x1F440)
        || (cp >= 0x1F442 && cp <= 0x1F4FC)
        || (cp >= 0x1F4FF && cp <= 0x1F53D)
        || (cp >= 0x1F54B && cp <= 0x1F54E)
        || (cp >= 0x1F550 && cp <= 0x1F567)
        || (cp >= 0x1F57A && cp <= 0x1F57A)
        || (cp >= 0x1F595 && cp <= 0x1F596)
        || (cp >= 0x1F5A4 && cp <= 0x1F5A4)
        || (cp >= 0x1F5FB && cp <= 0x1F64F)
        || (cp >= 0x1F680 && cp <= 0x1F6C5)
        || (cp >= 0x1F6CC && cp <= 0x1F6CC)
        || (cp >= 0x1F6D0 && cp <= 0x1F6D2)
        || (cp >= 0x1F6D5 && cp <= 0x1F6D7)
        || (cp >= 0x1F6EB && cp <= 0x1F6EC)
        || (cp >= 0x1F6F4 && cp <= 0x1F6FC)
        || (cp >= 0x1F7E0 && cp <= 0x1F7EB)
        || (cp >= 0x1F90C && cp <= 0x1F93A)
        || (cp >= 0x1F93C && cp <= 0x1F945)
        || (cp >= 0x1F947 && cp <= 0x1F978)
        || (cp >= 0x1F97A && cp <= 0x1F9CB)
        || (cp >= 0x1F9CD && cp <= 0x1F9FF)
        || (cp >= 0x1FA70 && cp <= 0x1FA74)
        || (cp >= 0x1FA78 && cp <= 0x1FA7A)
        || (cp >= 0x1FA80 && cp <= 0x1FA86)
        || (cp >= 0x1FA90 && cp <= 0x1FAA8)
        || (cp >= 0x1FAB0 && cp <= 0x1FAB6)
        || (cp >= 0x1FAC0 && cp <= 0x1FAC2)
        || (cp >= 0x1FAD0 && cp <= 0x1FAD6)
        || (cp >= 0x20000 && cp <= 0x2FFFD) // CJK ext B+
        || (cp >= 0x30000 && cp <= 0x3FFFD)
    {
        2
    } else {
        1
    }
}

/// Could-be-emoji pre-filter: broad Unicode blocks + VS16 + multi-scalar.
fn could_be_emoji(text: &str) -> bool {
    let mut chars = text.chars();
    let first = chars.next().map(|c| c as u32).unwrap_or(0);
    (0x1F000..=0x1FBFF).contains(&first)
        || (0x2300..=0x23FF).contains(&first)
        || (0x2600..=0x27BF).contains(&first)
        || (0x2B50..=0x2B55).contains(&first)
        || text.contains('\u{FE0F}')
        || text.chars().count() > 2
}

/// RGI_Emoji approximation: any char in the emoji width ranges plus VS16.
fn is_rgi_emoji(text: &str) -> bool {
    text.chars()
        .any(|c| east_asian_width(c as u32) == 2 && (0x1F000..=0x1FAFF).contains(&(c as u32)))
}

/// Segments with spacing marks that terminals allocate cells for
/// (approximation of the JS terminalSpacingMarkRegex).
fn is_terminal_spacing_mark(cp: u32) -> bool {
    (cp >= 0x0903 && cp <= 0x0903) // Devanagari visarga etc — broad spacing-mark ranges
        || (cp >= 0x093B && cp <= 0x093B)
        || (cp >= 0x093E && cp <= 0x0940)
        || (cp >= 0x0949 && cp <= 0x094C)
        || (cp >= 0x094E && cp <= 0x094F)
        || (cp >= 0x0982 && cp <= 0x0983)
        || (cp >= 0x09BE && cp <= 0x09C0)
        || (cp >= 0x09C7 && cp <= 0x09C8)
        || (cp >= 0x09CB && cp <= 0x09CC)
        || (cp >= 0x0A03 && cp <= 0x0A03)
        || (cp >= 0x0A3E && cp <= 0x0A40)
        || (cp >= 0x0A83 && cp <= 0x0A83)
        || (cp >= 0x0ABE && cp <= 0x0AC0)
        || (cp >= 0x0AC9 && cp <= 0x0AC9)
        || (cp >= 0x0ACB && cp <= 0x0ACC)
        || (cp >= 0x0B02 && cp <= 0x0B03)
        || (cp >= 0x0B3E && cp <= 0x0B3E)
        || (cp >= 0x0B40 && cp <= 0x0B40)
        || (cp >= 0x0B47 && cp <= 0x0B48)
        || (cp >= 0x0B4B && cp <= 0x0B4C)
        || (cp >= 0x0BBE && cp <= 0x0BBF)
        || (cp >= 0x0BC1 && cp <= 0x0BC2)
        || (cp >= 0x0BC6 && cp <= 0x0BC8)
        || (cp >= 0x0BCA && cp <= 0x0BCC)
        || (cp >= 0x0C01 && cp <= 0x0C03)
        || (cp >= 0x0C41 && cp <= 0x0C44)
        || (cp >= 0x0C82 && cp <= 0x0C83)
        || (cp >= 0x0CBE && cp <= 0x0CBE)
        || (cp >= 0x0CC0 && cp <= 0x0CC4)
        || (cp >= 0x0CC7 && cp <= 0x0CC8)
        || (cp >= 0x0CCA && cp <= 0x0CCB)
        || (cp >= 0x0D02 && cp <= 0x0D03)
        || (cp >= 0x0D3E && cp <= 0x0D40)
        || (cp >= 0x0D46 && cp <= 0x0D48)
        || (cp >= 0x0D4A && cp <= 0x0D4C)
        || (cp >= 0x0D82 && cp <= 0x0D83)
        || (cp >= 0x0DD0 && cp <= 0x0DD1)
        || (cp >= 0x0DD8 && cp <= 0x0DDE)
        || (cp >= 0x0DF2 && cp <= 0x0DF3)
        || (cp >= 0x0E33 && cp <= 0x0E33) // Thai/Lao AM handled specially
        || (cp >= 0x0EB3 && cp <= 0x0EB3)
        || (cp >= 0x0F3E && cp <= 0x0F3F)
        || (cp >= 0x0F7F && cp <= 0x0F7F)
        || (cp >= 0x102B && cp <= 0x102C)
        || (cp >= 0x1031 && cp <= 0x1031)
        || (cp >= 0x1038 && cp <= 0x1038)
        || (cp >= 0x103B && cp <= 0x103C)
        || (cp >= 0x1056 && cp <= 0x1057)
        || (cp >= 0x1062 && cp <= 0x1064)
        || (cp >= 0x1067 && cp <= 0x106D)
        || (cp >= 0x1083 && cp <= 0x1083)
        || (cp >= 0x1087 && cp <= 0x108C)
        || (cp >= 0x108F && cp <= 0x108F)
        || (cp >= 0x109A && cp <= 0x109C)
        || (cp >= 0x17B6 && cp <= 0x17B6)
        || (cp >= 0x17BE && cp <= 0x17C5)
        || (cp >= 0x17C7 && cp <= 0x17C8)
        || (cp >= 0x1923 && cp <= 0x1926)
        || (cp >= 0x1929 && cp <= 0x192B)
        || (cp >= 0x1930 && cp <= 0x1931)
        || (cp >= 0x1933 && cp <= 0x1938)
        || (cp >= 0x1A19 && cp <= 0x1A1A)
        || (cp >= 0x1A55 && cp <= 0x1A55)
        || (cp >= 0x1A57 && cp <= 0x1A57)
        || (cp >= 0x1A61 && cp <= 0x1A61)
        || (cp >= 0x1A63 && cp <= 0x1A64)
        || (cp >= 0x1A6D && cp <= 0x1A72)
        || (cp >= 0x1B04 && cp <= 0x1B04)
        || (cp >= 0x1B35 && cp <= 0x1B35)
        || (cp >= 0x1B3B && cp <= 0x1B3B)
        || (cp >= 0x1B3D && cp <= 0x1B41)
        || (cp >= 0x1B43 && cp <= 0x1B44)
        || (cp >= 0x1B82 && cp <= 0x1B82)
        || (cp >= 0x1BA1 && cp <= 0x1BA1)
        || (cp >= 0x1BA6 && cp <= 0x1BA7)
        || (cp >= 0x1BAA && cp <= 0x1BAA)
        || (cp >= 0x1BE7 && cp <= 0x1BE7)
        || (cp >= 0x1BEA && cp <= 0x1BEC)
        || (cp >= 0x1BEE && cp <= 0x1BEE)
        || (cp >= 0x1BF2 && cp <= 0x1BF3)
        || (cp >= 0x1C24 && cp <= 0x1C2B)
        || (cp >= 0x1C34 && cp <= 0x1C35)
        || (cp >= 0x1CE1 && cp <= 0x1CE1)
        || (cp >= 0x1CF2 && cp <= 0x1CF3)
        || (cp >= 0x302E && cp <= 0x302F)
        || (cp >= 0xA823 && cp <= 0xA824)
        || (cp >= 0xA827 && cp <= 0xA827)
        || (cp >= 0xA880 && cp <= 0xA881)
        || (cp >= 0xA8B4 && cp <= 0xA8C3)
        || (cp >= 0xA952 && cp <= 0xA953)
        || (cp >= 0xA983 && cp <= 0xA983)
        || (cp >= 0xA9B4 && cp <= 0xA9B5)
        || (cp >= 0xA9BA && cp <= 0xA9BB)
        || (cp >= 0xA9BD && cp <= 0xA9C0)
        || (cp >= 0xAA2F && cp <= 0xAA30)
        || (cp >= 0xAA33 && cp <= 0xAA34)
        || (cp >= 0xAA40 && cp <= 0xAA42)
        || (cp >= 0xAA44 && cp <= 0xAA4B)
        || (cp >= 0xAA7D && cp <= 0xAA7D)
        || (cp >= 0xAAEB && cp <= 0xAAEB)
        || (cp >= 0xAAEE && cp <= 0xAAEF)
        || (cp >= 0xAAF5 && cp <= 0xAAF5)
        || (cp >= 0xABE3 && cp <= 0xABE4)
        || (cp >= 0xABE6 && cp <= 0xABE7)
        || (cp >= 0xABE9 && cp <= 0xABEA)
        || (cp >= 0xABEC && cp <= 0xABEC)
        || (cp >= 0x11000 && cp <= 0x11000)
        || (cp >= 0x11002 && cp <= 0x11002)
        || (cp >= 0x11082 && cp <= 0x11082)
        || (cp >= 0x110B0 && cp <= 0x110B2)
        || (cp >= 0x110B7 && cp <= 0x110B8)
        || (cp >= 0x1112C && cp <= 0x1112C)
        || (cp >= 0x11145 && cp <= 0x11146)
        || (cp >= 0x11182 && cp <= 0x11182)
        || (cp >= 0x111B3 && cp <= 0x111B5)
        || (cp >= 0x111BF && cp <= 0x111C0)
        || (cp >= 0x1122C && cp <= 0x1122E)
        || (cp >= 0x11232 && cp <= 0x11233)
        || (cp >= 0x11235 && cp <= 0x11235)
        || (cp >= 0x112E0 && cp <= 0x112E2)
        || (cp >= 0x11302 && cp <= 0x11303)
        || (cp >= 0x1133E && cp <= 0x1133F)
        || (cp >= 0x11341 && cp <= 0x11344)
        || (cp >= 0x11347 && cp <= 0x11348)
        || (cp >= 0x1134B && cp <= 0x1134D)
        || (cp >= 0x11357 && cp <= 0x11357)
        || (cp >= 0x11362 && cp <= 0x11363)
        || (cp >= 0x11435 && cp <= 0x11437)
        || (cp >= 0x11440 && cp <= 0x11441)
        || (cp >= 0x11445 && cp <= 0x11445)
        || (cp >= 0x114B0 && cp <= 0x114B2)
        || (cp >= 0x114B9 && cp <= 0x114B9)
        || (cp >= 0x114BB && cp <= 0x114BE)
        || (cp >= 0x114C1 && cp <= 0x114C1)
        || (cp >= 0x115AF && cp <= 0x115B1)
        || (cp >= 0x115B8 && cp <= 0x115BB)
        || (cp >= 0x115BE && cp <= 0x115BE)
        || (cp >= 0x11630 && cp <= 0x11632)
        || (cp >= 0x1163B && cp <= 0x1163C)
        || (cp >= 0x1163E && cp <= 0x1163E)
        || (cp >= 0x116AC && cp <= 0x116AC)
        || (cp >= 0x116AE && cp <= 0x116AF)
        || (cp >= 0x116B6 && cp <= 0x116B6)
        || (cp >= 0x11720 && cp <= 0x11721)
        || (cp >= 0x11726 && cp <= 0x11726)
        || (cp >= 0x1182C && cp <= 0x1182E)
        || (cp >= 0x11838 && cp <= 0x11838)
        || (cp >= 0x1193D && cp <= 0x1193F)
        || (cp >= 0x11942 && cp <= 0x11943)
        || (cp >= 0x11940 && cp <= 0x11940)
        || (cp >= 0x11A07 && cp <= 0x11A08)
        || (cp >= 0x11A39 && cp <= 0x11A39)
        || (cp >= 0x11A57 && cp <= 0x11A58)
        || (cp >= 0x11A97 && cp <= 0x11A97)
        || (cp >= 0x11C2F && cp <= 0x11C2F)
        || (cp >= 0x11C3E && cp <= 0x11C3E)
        || (cp >= 0x11CA9 && cp <= 0x11CA9)
        || (cp >= 0x11CB1 && cp <= 0x11CB1)
        || (cp >= 0x11CB4 && cp <= 0x11CB4)
        || (cp >= 0x16F51 && cp <= 0x16F87)
        || (cp >= 0x16FF0 && cp <= 0x16FF1)
        || (cp >= 0x1D165 && cp <= 0x1D166)
        || (cp >= 0x1D16D && cp <= 0x1D172)
}

/// Segment text into grapheme clusters (approximate Intl.Segmenter).
pub fn graphemes(text: &str) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut pending_marks = String::new();
    let mut chars = text.chars().peekable();
    let mut ri_pair_open = false;

    while let Some(char) = chars.next() {
        let cp = char as u32;
        if current.is_empty() {
            current.push(char);
            ri_pair_open = (0x1F1E6..=0x1F1FF).contains(&cp);
            continue;
        }
        // Combining marks attach to the base.
        if is_mark(cp) {
            pending_marks.push(char);
            continue;
        }
        // ZWJ sequences join the next char.
        if char == '\u{200D}' {
            current.push(char);
            if let Some(next) = chars.next() {
                current.push(next);
            }
            continue;
        }
        // VS16 joins emoji.
        if char == '\u{FE0F}' {
            current.push(char);
            continue;
        }
        // Regional indicators pair up.
        if (0x1F1E6..=0x1F1FF).contains(&cp) {
            if ri_pair_open {
                current.push(char);
                if !pending_marks.is_empty() {
                    current.push_str(&pending_marks);
                    pending_marks.clear();
                }
                result.push(std::mem::take(&mut current));
                ri_pair_open = false;
                continue;
            }
            if !pending_marks.is_empty() {
                current.push_str(&pending_marks);
                pending_marks.clear();
            }
            result.push(std::mem::take(&mut current));
            current.push(char);
            ri_pair_open = true;
            continue;
        }
        // Any other char ends the current cluster.
        if !pending_marks.is_empty() {
            current.push_str(&pending_marks);
            pending_marks.clear();
        }
        result.push(std::mem::take(&mut current));
        current.push(char);
        ri_pair_open = false;
    }
    if !pending_marks.is_empty() {
        current.push_str(&pending_marks);
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

/// Width of a single grapheme cluster in terminal columns.
pub fn grapheme_width(segment: &str) -> f64 {
    if segment == "\t" {
        return 3.0;
    }

    // Spacing marks that occupy cells even without a base character.
    if segment.chars().count() == 1 {
        let cp = segment.chars().next().unwrap() as u32;
        if is_terminal_spacing_mark(cp) {
            return segment.chars().count() as f64;
        }
    }

    // Zero-width clusters.
    if segment
        .chars()
        .all(|c| is_default_ignorable(c as u32) || is_control(c as u32) || is_mark(c as u32))
    {
        return 0.0;
    }

    // Emoji check with pre-filter.
    if could_be_emoji(segment) && is_rgi_emoji(segment) {
        return 2.0;
    }

    // Base visible codepoint: skip leading non-printing.
    let base = segment
        .chars()
        .find(|c| {
            let cp = *c as u32;
            !is_default_ignorable(cp) && !is_control(cp) && !is_mark(cp)
        })
        .map(|c| c as u32);

    let Some(cp) = base else {
        return 0.0;
    };

    // Regional indicators stay conservative (2).
    if (0x1F1E6..=0x1F1FF).contains(&cp) {
        return 2.0;
    }

    let mut width = east_asian_width(cp);

    // Count trailing visible code points that terminals may allocate cells
    // for (approximation of the JS loop over chars after the base).
    for char in segment.chars().skip(1) {
        let c = char as u32;
        if is_terminal_spacing_mark(c) {
            width += 1;
        } else if c == 0x0E33 || c == 0x0EB3 {
            width += 1;
        } else if (0xFF00..=0xFFEF).contains(&c) {
            width += east_asian_width(c);
        }
    }

    width as f64
}

/// Visible width of a string in terminal columns.
pub fn visible_width(str: &str) -> f64 {
    if str.is_empty() {
        return 0.0;
    }

    // Fast path: pure ASCII printable.
    if str.chars().all(|c| (0x20..=0x7E).contains(&(c as u32))) {
        return str.chars().count() as f64;
    }

    // Cache (bounded).
    {
        let cache = WIDTH_CACHE.lock().unwrap();
        if let Some(cache) = cache.as_ref() {
            if let Some(cached) = cache.get(str) {
                return *cached;
            }
        }
    }

    // Normalize: tabs to 3 spaces, strip ANSI.
    let clean = if str.contains('\t') || str.contains('\x1b') {
        let mut result = String::new();
        let mut i = 0;
        while i < str.len() {
            if let Some(ansi) = extract_ansi_code(str, i) {
                i += ansi.length;
                continue;
            }
            let char = str[i..].chars().next().unwrap();
            if char == '\t' {
                result.push_str("   ");
            } else {
                result.push(char);
            }
            i += char.len_utf8();
        }
        result
    } else {
        str.to_string()
    };

    let mut width = 0.0;
    for segment in graphemes(&clean) {
        width += grapheme_width(&segment);
    }

    let mut cache = WIDTH_CACHE.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    if cache.len() >= WIDTH_CACHE_SIZE {
        if let Some(key) = cache.keys().next().cloned() {
            cache.remove(&key);
        }
    }
    cache.insert(str.to_string(), width);
    width
}

const WIDTH_CACHE_SIZE: usize = 512;
static WIDTH_CACHE: Mutex<Option<HashMap<String, f64>>> = Mutex::new(None);

/// Remove ANSI, OSC, and APC control sequences while preserving visible text.
pub fn strip_terminal_sequences(str: &str) -> String {
    if !str.contains('\x1b') {
        return str.to_string();
    }
    let mut result = String::new();
    let mut i = 0;
    while i < str.len() {
        if let Some(ansi) = extract_ansi_code(str, i) {
            i += ansi.length;
            continue;
        }
        let char = str[i..].chars().next().unwrap();
        result.push(char);
        i += char.len_utf8();
    }
    result
}

/// Extract ANSI escape sequences from a string at the given byte position.
#[derive(Clone, Debug, PartialEq)]
pub struct AnsiCode {
    pub code: String,
    pub length: usize,
}

pub fn extract_ansi_code(str: &str, pos: usize) -> Option<AnsiCode> {
    let bytes = str.as_bytes();
    if pos >= bytes.len() || bytes[pos] != 0x1b {
        return None;
    }
    let next = bytes.get(pos + 1).copied()?;

    // CSI: ESC [ ... m/G/K/H/J
    if next == b'[' {
        let mut j = pos + 2;
        while j < bytes.len() && !matches!(bytes[j], b'm' | b'G' | b'K' | b'H' | b'J') {
            j += 1;
        }
        if j < bytes.len() {
            return Some(AnsiCode {
                code: str[pos..=j].to_string(),
                length: j + 1 - pos,
            });
        }
        return None;
    }

    // OSC: ESC ] ... BEL or ST
    if next == b']' {
        let mut j = pos + 2;
        while j < bytes.len() {
            if bytes[j] == 0x07 {
                return Some(AnsiCode {
                    code: str[pos..=j].to_string(),
                    length: j + 1 - pos,
                });
            }
            if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                return Some(AnsiCode {
                    code: str[pos..=j + 1].to_string(),
                    length: j + 2 - pos,
                });
            }
            j += 1;
        }
        return None;
    }

    // APC: ESC _ ... BEL or ST
    if next == b'_' {
        let mut j = pos + 2;
        while j < bytes.len() {
            if bytes[j] == 0x07 {
                return Some(AnsiCode {
                    code: str[pos..=j].to_string(),
                    length: j + 1 - pos,
                });
            }
            if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                return Some(AnsiCode {
                    code: str[pos..=j + 1].to_string(),
                    length: j + 2 - pos,
                });
            }
            j += 1;
        }
        return None;
    }

    None
}

/// Normalize text for terminal output: Thai/Lao AM vowels are decomposed
/// (same cell width, avoids stale-cell artifacts); visible tabs expand to
/// the fixed width used by layout.
pub fn normalize_terminal_output(str: &str) -> String {
    let normalized = str.replace('\u{0E33}', "\u{0E4D}\u{0E32}").replace('\u{0EB3}', "\u{0ECD}\u{0EB2}");
    if !normalized.contains('\t') {
        return normalized;
    }
    let mut result = String::new();
    let mut i = 0;
    while i < normalized.len() {
        if let Some(ansi) = extract_ansi_code(&normalized, i) {
            result.push_str(&ansi.code);
            i += ansi.length;
            continue;
        }
        let char = normalized[i..].chars().next().unwrap();
        if char == '\t' {
            result.push_str("   ");
        } else {
            result.push(char);
        }
        i += char.len_utf8();
    }
    result
}

/// CJK break regex equivalent: Han/Hiragana/Katakana/Hangul/Bopomofo.
pub fn is_cjk_char(c: char) -> bool {
    let cp = c as u32;
    (cp >= 0x4E00 && cp <= 0x9FFF)
        || (cp >= 0x3040 && cp <= 0x30FF)
        || (cp >= 0xAC00 && cp <= 0xD7A3)
        || (cp >= 0x1100 && cp <= 0x11FF)
        || (cp >= 0x3100 && cp <= 0x312F)
        || (cp >= 0x31A0 && cp <= 0x31BF)
}

pub fn is_whitespace_char(char: char) -> bool {
    char.is_whitespace()
}

pub fn is_punctuation_char(char: char) -> bool {
    matches!(
        char,
        '(' | ')' | '{' | '}' | '[' | ']' | '<' | '>' | '.' | ',' | ';' | ':' | '\'' | '"' | '!' | '?'
            | '+' | '-' | '=' | '*' | '/' | '\\' | '|' | '&' | '%' | '^' | '$' | '#' | '@' | '~' | '`'
    )
}

/// Placeholder for the Kitty graphics protocol image line check (ported with
/// terminal-image.ts).
pub fn is_image_line_placeholder(line: &str) -> bool {
    line.starts_with("\x1b_G")
}

fn is_printable_ascii(str: &str) -> bool {
    str.chars().all(|c| {
        let code = c as u32;
        (0x20..=0x7E).contains(&code)
    })
}

fn truncate_fragment_to_width(text: &str, max_width: f64) -> (String, f64) {
    if max_width <= 0.0 || text.is_empty() {
        return (String::new(), 0.0);
    }

    if is_printable_ascii(text) {
        let clipped: String = text.chars().take(max_width as usize).collect();
        return (clipped.clone(), clipped.chars().count() as f64);
    }

    let mut result = String::new();
    let mut width = 0.0;
    let mut i = 0;
    let mut pending_ansi = String::new();
    let bytes = text.as_bytes();

    while i < bytes.len() {
        if let Some(ansi) = extract_ansi_code(text, i) {
            pending_ansi.push_str(&ansi.code);
            i += ansi.length;
            continue;
        }
        if bytes[i] == b'\t' {
            if width + 3.0 > max_width {
                break;
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push('\t');
            width += 3.0;
            i += 1;
            continue;
        }
        let char = text[i..].chars().next().unwrap();
        let cluster_start = i;
        i += char.len_utf8();
        // Collect full grapheme.
        let mut cluster = char.to_string();
        loop {
            let Some(next) = text[i..].chars().next() else { break };
            if extract_ansi_code(text, i).is_some() {
                break;
            }
            let next_cp = next as u32;
            if is_mark(next_cp) || next == '\u{FE0F}' || next == '\u{200D}' {
                cluster.push(next);
                i += next.len_utf8();
                if next == '\u{200D}' {
                    if let Some(joined) = text[i..].chars().next() {
                        cluster.push(joined);
                        i += joined.len_utf8();
                    }
                }
            } else {
                break;
            }
        }
        let _ = cluster_start;
        let w = grapheme_width(&cluster);
        if width + w > max_width {
            break;
        }
        if !pending_ansi.is_empty() {
            result.push_str(&pending_ansi);
            pending_ansi.clear();
        }
        result.push_str(&cluster);
        width += w;
    }
    (result, width)
}

fn get_active_osc8_close(prefix: &str) -> String {
    if !prefix.contains("\x1b]8;") {
        return String::new();
    }
    let mut active_hyperlink: Option<(String, String)> = None; // (params, terminator)
    let mut i = 0;
    while i < prefix.len() {
        if let Some(ansi) = extract_ansi_code(prefix, i) {
            if let Some(hyperlink) = parse_osc8_hyperlink(&ansi.code) {
                active_hyperlink = hyperlink;
            }
            i += ansi.length;
        } else {
            i += prefix[i..].chars().next().unwrap().len_utf8();
        }
    }
    match active_hyperlink {
        Some((_, terminator)) => format!("\x1b]8;;{terminator}"),
        None => String::new(),
    }
}

/// Parse an OSC 8 hyperlink; None when not an OSC 8, Some(None) when it is
/// an empty link (close).
fn parse_osc8_hyperlink(ansi_code: &str) -> Option<Option<(String, String)>> {
    if !ansi_code.starts_with("\x1b]8;") {
        return None;
    }
    let terminator = if ansi_code.ends_with('\x07') {
        "\x07".to_string()
    } else if ansi_code.ends_with("\x1b\\") {
        "\x1b\\".to_string()
    } else {
        return Some(None);
    };
    let body = &ansi_code[4..ansi_code.len() - terminator.len()];
    let Some(separator_index) = body.find(';') else {
        return Some(None);
    };
    let params = body[..separator_index].to_string();
    let url = body[separator_index + 1..].to_string();
    if url.is_empty() {
        return Some(None);
    }
    Some(Some((params, url)))
}

fn finalize_truncated_result(
    prefix: &str,
    prefix_width: f64,
    ellipsis: &str,
    ellipsis_width: f64,
    max_width: f64,
    pad: bool,
) -> String {
    let reset = "\x1b[0m";
    let hyperlink_close = get_active_osc8_close(prefix);
    let visible_width = prefix_width + ellipsis_width;
    let mut result = if ellipsis.is_empty() {
        format!("{prefix}{hyperlink_close}{reset}")
    } else {
        format!("{prefix}{hyperlink_close}{reset}{ellipsis}{reset}")
    };
    if pad {
        result += &" ".repeat((max_width - visible_width).max(0.0) as usize);
    }
    result
}

/// Truncate text to fit within a maximum visible width, adding ellipsis.
pub fn truncate_to_width(text: &str, max_width: f64, ellipsis: &str, pad: bool) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    if text.is_empty() {
        return if pad { " ".repeat(max_width as usize) } else { String::new() };
    }

    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= max_width {
        let text_width = visible_width(text);
        if text_width <= max_width {
            return if pad {
                format!("{text}{}", " ".repeat((max_width - text_width) as usize))
            } else {
                text.to_string()
            };
        }
        let (clipped, clipped_width) = truncate_fragment_to_width(ellipsis, max_width);
        if clipped_width == 0.0 {
            return if pad { " ".repeat(max_width as usize) } else { String::new() };
        }
        return finalize_truncated_result("", 0.0, &clipped, clipped_width, max_width, pad);
    }

    if is_printable_ascii(text) {
        let text_chars = text.chars().count() as f64;
        if text_chars <= max_width {
            return if pad {
                format!("{text}{}", " ".repeat((max_width - text_chars) as usize))
            } else {
                text.to_string()
            };
        }
        let target_width = max_width - ellipsis_width;
        let clipped: String = text.chars().take(target_width as usize).collect();
        return finalize_truncated_result(&clipped, target_width, ellipsis, ellipsis_width, max_width, pad);
    }

    let target_width = max_width - ellipsis_width;
    let mut result = String::new();
    let mut pending_ansi = String::new();
    let mut visible_so_far = 0.0;
    let mut kept_width = 0.0;
    let mut keep_contiguous_prefix = true;
    let mut overflowed = false;
    let mut exhausted_input = true;
    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');

    let mut i = 0;
    let bytes = text.as_bytes();
    if !has_ansi && !has_tabs {
        for cluster in graphemes(text) {
            let width = grapheme_width(&cluster);
            if keep_contiguous_prefix && kept_width + width <= target_width {
                result += &cluster;
                kept_width += width;
            } else {
                keep_contiguous_prefix = false;
            }
            visible_so_far += width;
            if visible_so_far > max_width {
                overflowed = true;
                exhausted_input = false;
                break;
            }
        }
    } else {
        while i < bytes.len() {
            if let Some(ansi) = extract_ansi_code(text, i) {
                pending_ansi += &ansi.code;
                i += ansi.length;
                continue;
            }
            if bytes[i] == b'\t' {
                if keep_contiguous_prefix && kept_width + 3.0 <= target_width {
                    if !pending_ansi.is_empty() {
                        result += &pending_ansi;
                        pending_ansi.clear();
                    }
                    result.push('\t');
                    kept_width += 3.0;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += 3.0;
                if visible_so_far > max_width {
                    overflowed = true;
                    exhausted_input = false;
                    break;
                }
                i += 1;
                continue;
            }
            let char = text[i..].chars().next().unwrap();
            let mut cluster = char.to_string();
            i += char.len_utf8();
            loop {
                let Some(next) = text[i..].chars().next() else { break };
                if extract_ansi_code(text, i).is_some() {
                    break;
                }
                let next_cp = next as u32;
                if is_mark(next_cp) || next == '\u{FE0F}' || next == '\u{200D}' {
                    cluster.push(next);
                    i += next.len_utf8();
                    if next == '\u{200D}' {
                        if let Some(joined) = text[i..].chars().next() {
                            cluster.push(joined);
                            i += joined.len_utf8();
                        }
                    }
                } else {
                    break;
                }
            }
            let width = grapheme_width(&cluster);
            if keep_contiguous_prefix && kept_width + width <= target_width {
                if !pending_ansi.is_empty() {
                    result += &pending_ansi;
                    pending_ansi.clear();
                }
                result += &cluster;
                kept_width += width;
            } else {
                keep_contiguous_prefix = false;
                pending_ansi.clear();
            }
            visible_so_far += width;
            if visible_so_far > max_width {
                overflowed = true;
                exhausted_input = false;
                break;
            }
        }
    }

    if !overflowed && exhausted_input {
        return if pad {
            format!("{text}{}", " ".repeat((max_width - visible_so_far).max(0.0) as usize))
        } else {
            text.to_string()
        };
    }

    finalize_truncated_result(&result, kept_width, ellipsis, ellipsis_width, max_width, pad)
}

/// Extract a range of visible columns from a line.
pub fn slice_by_column(line: &str, start_col: f64, length: f64, strict: bool) -> String {
    slice_with_width(line, start_col, length, strict).0
}

/// Like slice_by_column but also returns the actual visible width.
pub fn slice_with_width(line: &str, start_col: f64, length: f64, strict: bool) -> (String, f64) {
    if length <= 0.0 {
        return (String::new(), 0.0);
    }
    let end_col = start_col + length;
    let mut result = String::new();
    let mut result_width = 0.0;
    let mut current_col = 0.0;
    let mut pending_ansi = String::new();
    let mut i = 0;
    let bytes = line.as_bytes();

    while i < bytes.len() {
        if let Some(ansi) = extract_ansi_code(line, i) {
            if current_col >= start_col && current_col < end_col {
                result += &ansi.code;
            } else if current_col < start_col {
                pending_ansi += &ansi.code;
            }
            i += ansi.length;
            continue;
        }
        let char = line[i..].chars().next().unwrap();
        let mut cluster = char.to_string();
        i += char.len_utf8();
        loop {
            let Some(next) = line[i..].chars().next() else { break };
            if extract_ansi_code(line, i).is_some() {
                break;
            }
            let next_cp = next as u32;
            if is_mark(next_cp) || next == '\u{FE0F}' || next == '\u{200D}' {
                cluster.push(next);
                i += next.len_utf8();
                if next == '\u{200D}' {
                    if let Some(joined) = line[i..].chars().next() {
                        cluster.push(joined);
                        i += joined.len_utf8();
                    }
                }
            } else {
                break;
            }
        }
        let w = grapheme_width(&cluster);
        let in_range = current_col >= start_col && current_col < end_col;
        let fits = !strict || current_col + w <= end_col;
        if in_range && fits {
            if !pending_ansi.is_empty() {
                result += &pending_ansi;
                pending_ansi.clear();
            }
            result += &cluster;
            result_width += w;
        }
        current_col += w;
        if current_col >= end_col {
            break;
        }
    }
    (result, result_width)
}

/// Extract "before" and "after" segments from a line in a single pass,
/// preserving styling across the overlay region.
pub fn extract_segments(
    line: &str,
    before_end: f64,
    after_start: f64,
    after_len: f64,
    strict_after: bool,
) -> (String, f64, String, f64) {
    let mut before = String::new();
    let mut before_width = 0.0;
    let mut after = String::new();
    let mut after_width = 0.0;
    let mut current_col = 0.0;
    let mut pending_ansi_before = String::new();
    let mut after_started = false;
    let after_end = after_start + after_len;
    let mut i = 0;
    let bytes = line.as_bytes();

    // Track styling state (approximate: SGR codes only).
    let mut tracker = AnsiCodeTracker::new();

    while i < bytes.len() {
        if let Some(ansi) = extract_ansi_code(line, i) {
            tracker.process(&ansi.code);
            if current_col < before_end {
                pending_ansi_before += &ansi.code;
            } else if current_col >= after_start && current_col < after_end && after_started {
                after += &ansi.code;
            }
            i += ansi.length;
            continue;
        }
        let char = line[i..].chars().next().unwrap();
        let mut cluster = char.to_string();
        i += char.len_utf8();
        loop {
            let Some(next) = line[i..].chars().next() else { break };
            if extract_ansi_code(line, i).is_some() {
                break;
            }
            let next_cp = next as u32;
            if is_mark(next_cp) || next == '\u{FE0F}' || next == '\u{200D}' {
                cluster.push(next);
                i += next.len_utf8();
                if next == '\u{200D}' {
                    if let Some(joined) = line[i..].chars().next() {
                        cluster.push(joined);
                        i += joined.len_utf8();
                    }
                }
            } else {
                break;
            }
        }
        let w = grapheme_width(&cluster);

        if current_col < before_end && current_col + w <= before_end {
            if !pending_ansi_before.is_empty() {
                before += &pending_ansi_before;
                pending_ansi_before.clear();
            }
            before += &cluster;
            before_width += w;
        } else if current_col >= after_start && current_col < after_end {
            let fits = !strict_after || current_col + w <= after_end;
            if fits {
                if !after_started {
                    after += &tracker.get_active_codes();
                    after_started = true;
                }
                after += &cluster;
                after_width += w;
            }
        }

        current_col += w;
        if after_len <= 0.0 {
            if current_col >= before_end {
                break;
            }
        } else if current_col >= after_end {
            break;
        }
    }
    (before, before_width, after, after_width)
}

/// Tracks active ANSI SGR codes to preserve styling across line breaks.
#[derive(Clone)]
pub struct AnsiCodeTracker {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    fg_color: Option<String>,
    bg_color: Option<String>,
    active_hyperlink: Option<(String, String)>,
}

impl AnsiCodeTracker {
    pub fn new() -> Self {
        Self {
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            blink: false,
            inverse: false,
            hidden: false,
            strikethrough: false,
            fg_color: None,
            bg_color: None,
            active_hyperlink: None,
        }
    }

    pub fn process(&mut self, ansi_code: &str) {
        if let Some(hyperlink) = parse_osc8_hyperlink(ansi_code) {
            self.active_hyperlink = hyperlink;
            return;
        }
        if !ansi_code.ends_with('m') {
            return;
        }
        let Some(params) = ansi_code
            .strip_prefix("\x1b[")
            .and_then(|rest| rest.strip_suffix('m'))
        else {
            return;
        };
        if params.is_empty() || params == "0" {
            self.reset();
            return;
        }
        let parts: Vec<&str> = params.split(';').collect();
        let mut i = 0;
        while i < parts.len() {
            let code = parts[i].parse::<u32>().unwrap_or(0);
            if code == 38 || code == 48 {
                if parts.get(i + 1) == Some(&"5") && parts.get(i + 2).is_some() {
                    let color_code = format!("{};{};{}", parts[i], parts[i + 1], parts[i + 2]);
                    if code == 38 {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 3;
                    continue;
                }
                if parts.get(i + 1) == Some(&"2") && parts.get(i + 4).is_some() {
                    let color_code = format!(
                        "{};{};{};{};{}",
                        parts[i],
                        parts[i + 1],
                        parts[i + 2],
                        parts[i + 3],
                        parts[i + 4]
                    );
                    if code == 38 {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 5;
                    continue;
                }
            }
            match code {
                0 => self.reset(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                5 => self.blink = true,
                7 => self.inverse = true,
                8 => self.hidden = true,
                9 => self.strikethrough = true,
                21 => self.bold = false,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                25 => self.blink = false,
                27 => self.inverse = false,
                28 => self.hidden = false,
                29 => self.strikethrough = false,
                39 => self.fg_color = None,
                49 => self.bg_color = None,
                _ => {
                    if (30..=37).contains(&code) || (90..=97).contains(&code) {
                        self.fg_color = Some(code.to_string());
                    } else if (40..=47).contains(&code) || (100..=107).contains(&code) {
                        self.bg_color = Some(code.to_string());
                    }
                }
            }
            i += 1;
        }
    }

    fn reset(&mut self) {
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
        self.blink = false;
        self.inverse = false;
        self.hidden = false;
        self.strikethrough = false;
        self.fg_color = None;
        self.bg_color = None;
    }

    pub fn clear(&mut self) {
        self.reset();
        self.active_hyperlink = None;
    }

    pub fn get_active_codes(&self) -> String {
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".to_string());
        }
        if self.dim {
            codes.push("2".to_string());
        }
        if self.italic {
            codes.push("3".to_string());
        }
        if self.underline {
            codes.push("4".to_string());
        }
        if self.blink {
            codes.push("5".to_string());
        }
        if self.inverse {
            codes.push("7".to_string());
        }
        if self.hidden {
            codes.push("8".to_string());
        }
        if self.strikethrough {
            codes.push("9".to_string());
        }
        if let Some(fg) = &self.fg_color {
            codes.push(fg.clone());
        }
        if let Some(bg) = &self.bg_color {
            codes.push(bg.clone());
        }
        let mut result = if codes.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", codes.join(";"))
        };
        if let Some((params, url)) = &self.active_hyperlink {
            result += &format!("\x1b]8;{params};{url}\x1b\\");
        }
        result
    }

    pub fn has_active_codes(&self) -> bool {
        self.bold
            || self.dim
            || self.italic
            || self.underline
            || self.blink
            || self.inverse
            || self.hidden
            || self.strikethrough
            || self.fg_color.is_some()
            || self.bg_color.is_some()
            || self.active_hyperlink.is_some()
    }

    /// Get reset codes for attributes that need to be turned off at line end.
    pub fn get_line_end_reset(&self) -> String {
        let mut result = String::new();
        if self.underline {
            result += "\x1b[24m";
        }
        if let Some((_, terminator)) = &self.active_hyperlink {
            result += &format!("\x1b]8;;{terminator}");
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_width_ascii() {
        assert_eq!(visible_width(""), 0.0);
        assert_eq!(visible_width("hello"), 5.0);
    }

    #[test]
    fn visible_width_cjk_and_emoji() {
        assert_eq!(visible_width("你好"), 4.0);
        assert_eq!(visible_width("a你b"), 4.0);
        assert_eq!(visible_width("🙂"), 2.0);
    }

    #[test]
    fn visible_width_strips_ansi_and_tabs() {
        assert_eq!(visible_width("\x1b[31mred\x1b[0m"), 3.0);
        assert_eq!(visible_width("a\tb"), 5.0); // tab = 3
    }

    #[test]
    fn visible_width_combining_marks_zero() {
        // e + combining acute = 1 column.
        assert_eq!(visible_width("e\u{0301}"), 1.0);
        // Pure combining mark cluster = 0.
        assert_eq!(grapheme_width("\u{0301}"), 0.0);
    }

    #[test]
    fn strip_sequences() {
        assert_eq!(strip_terminal_sequences("\x1b[1mhi\x1b[0m"), "hi");
        assert_eq!(strip_terminal_sequences("plain"), "plain");
        assert_eq!(strip_terminal_sequences("\x1b]8;;http://x\x07link\x1b]8;;\x07"), "link");
    }

    #[test]
    fn extract_ansi_variants() {
        assert_eq!(
            extract_ansi_code("\x1b[31mred", 0),
            Some(AnsiCode {
                code: "\x1b[31m".to_string(),
                length: 5
            })
        );
        assert_eq!(
            extract_ansi_code("\x1b]8;;http://x\x07link", 0).unwrap().code,
            "\x1b]8;;http://x\x07"
        );
        assert_eq!(
            extract_ansi_code("\x1b_APC\x1b\\", 0).unwrap().code,
            "\x1b_APC\x1b\\"
        );
        assert_eq!(extract_ansi_code("plain", 0), None);
    }

    #[test]
    fn normalize_output() {
        assert_eq!(normalize_terminal_output("a\tb"), "a   b");
        assert_eq!(normalize_terminal_output("\u{0E33}"), "\u{0E4D}\u{0E32}");
        assert_eq!(normalize_terminal_output("plain"), "plain");
    }

    #[test]
    fn truncate_ascii_with_ellipsis() {
        // finalizeTruncatedResult appends an SGR reset (JS behavior, even
        // without input ANSI codes).
        assert_eq!(
            truncate_to_width("hello world", 10.0, "...", false),
            "hello w\x1b[0m...\x1b[0m"
        );
        assert_eq!(truncate_to_width("hello", 10.0, "...", false), "hello");
        assert_eq!(truncate_to_width("hello", 3.0, "...", false), "\x1b[0m...\x1b[0m");
        // Ellipsis wider than maxWidth: truncated ellipsis only.
        assert_eq!(truncate_to_width("hello", 1.0, "...", false), "\x1b[0m.\x1b[0m");
        assert_eq!(truncate_to_width("", 10.0, "...", false), "");
        assert_eq!(truncate_to_width("abc", 0.0, "...", false), "");
    }

    #[test]
    fn truncate_pads() {
        let result = truncate_to_width("hi", 5.0, "...", true);
        assert_eq!(visible_width(&result), 5.0);
        assert!(result.ends_with("  "));
    }

    #[test]
    fn truncate_ansi_keeps_codes() {
        let result = truncate_to_width("\x1b[31mhello world\x1b[0m", 7.0, "...", false);
        assert!(result.contains("\x1b[31m"));
        assert_eq!(visible_width(&result), 7.0);
    }

    #[test]
    fn slice_columns() {
        assert_eq!(slice_by_column("hello world", 6.0, 5.0, false), "world");
        // 你 occupies columns 0-2, 好 columns 2-4.
        assert_eq!(slice_by_column("你好世界", 2.0, 2.0, false), "好");
        assert_eq!(slice_by_column("你好世界", 0.0, 2.0, false), "你");
        assert_eq!(slice_by_column("abc", 0.0, 0.0, false), "");
    }

    #[test]
    fn slice_strict_excludes_wide_boundary() {
        // Strict: a wide char at the boundary extends past the range.
        assert_eq!(slice_by_column("ab你好", 1.0, 1.0, true), "b");
        // 你 spans columns 2-4; range 1-3 cannot fit it strictly.
        assert_eq!(slice_by_column("ab你好", 1.0, 2.0, true), "b");
        assert_eq!(slice_by_column("ab你好", 1.0, 3.0, true), "b你");
    }

    #[test]
    fn graphemes_split() {
        assert_eq!(graphemes("abc"), vec!["a", "b", "c"]);
        assert_eq!(graphemes("e\u{0301}"), vec!["e\u{0301}"]);
        assert_eq!(graphemes("👨\u{200D}👩"), vec!["👨\u{200D}👩"]);
        assert_eq!(graphemes("🇺🇸"), vec!["🇺🇸"]);
        assert_eq!(graphemes("a😀b"), vec!["a", "😀", "b"]);
    }

    #[test]
    fn char_classification() {
        assert!(is_whitespace_char(' '));
        assert!(!is_whitespace_char('a'));
        assert!(is_punctuation_char('.'));
        assert!(is_punctuation_char('!'));
        assert!(!is_punctuation_char('a'));
        assert!(is_cjk_char('你'));
        assert!(!is_cjk_char('a'));
    }

    #[test]
    fn tracker_tracks_sgr() {
        let mut tracker = AnsiCodeTracker::new();
        tracker.process("\x1b[1;31m");
        assert_eq!(tracker.get_active_codes(), "\x1b[1;31m");
        assert!(tracker.has_active_codes());
        tracker.process("\x1b[0m");
        assert!(!tracker.has_active_codes());
        assert_eq!(tracker.get_active_codes(), "");
    }

    #[test]
    fn tracker_256_and_rgb_colors() {
        let mut tracker = AnsiCodeTracker::new();
        tracker.process("\x1b[38;5;240m");
        assert_eq!(tracker.get_active_codes(), "\x1b[38;5;240m");
        let mut tracker = AnsiCodeTracker::new();
        tracker.process("\x1b[48;2;1;2;3m");
        assert_eq!(tracker.get_active_codes(), "\x1b[48;2;1;2;3m");
    }

    #[test]
    fn tracker_line_end_reset() {
        let mut tracker = AnsiCodeTracker::new();
        tracker.process("\x1b[4m");
        assert_eq!(tracker.get_line_end_reset(), "\x1b[24m");
    }

    #[test]
    fn tracker_hyperlink_preserved() {
        let mut tracker = AnsiCodeTracker::new();
        tracker.process("\x1b]8;;http://example.com\x1b\\");
        assert!(tracker.get_active_codes().contains("http://example.com"));
    }

    #[test]
    fn extract_segments_split() {
        let (before, before_width, after, after_width) = extract_segments("abcdef", 2.0, 3.0, 2.0, false);
        assert_eq!(before, "ab");
        assert_eq!(before_width, 2.0);
        assert_eq!(after, "de");
        assert_eq!(after_width, 2.0);
    }

    #[test]
    fn extract_segments_inherits_style() {
        // Style is reset before the overlay: no codes are inherited.
        let (_, _, after, _) = extract_segments("\x1b[1mbold\x1b[0mxyz", 4.0, 5.0, 2.0, false);
        assert_eq!(after, "yz");
        // Without the reset, bold style carries into the after segment.
        let (_, _, after, _) = extract_segments("\x1b[1mboldxyz", 4.0, 5.0, 2.0, false);
        assert!(after.starts_with("\x1b[1m"));
    }
}
