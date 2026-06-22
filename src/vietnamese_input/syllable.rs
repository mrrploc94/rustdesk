//! Vietnamese syllable data models.
//!
//! A Vietnamese syllable has the canonical structure:
//!
//! ```text
//! [onset] nucleus [coda]   (+ optional tone mark on the nucleus)
//! ```
//!
//! where:
//! - `onset`   is an optional initial consonant cluster (e.g. "ng", "tr", "ph"),
//! - `nucleus` is a vowel cluster of one to three vowels (the only mandatory part),
//! - `coda`    is an optional final consonant (e.g. "n", "ng", "c", "t"),
//! - `tone`    is the tone mark applied to the nucleus.
//!
//! This module provides the [`Syllable`], [`VowelCluster`], and [`VowelUnit`]
//! structures together with helper methods for identifying Vietnamese vowels and
//! parsing a rendered syllable string back into its phonological components.

use super::{ToneMark, VowelMark};

/// Tone index order used by [`VOWEL_TABLE`]: the six glyph variants of each
/// (base, mark) pair are listed in the order
/// `[none, huyền, sắc, hỏi, ngã, nặng]`.
const TONE_ORDER: [Option<ToneMark>; 6] = [
    None,
    Some(ToneMark::Huyen),
    Some(ToneMark::Sac),
    Some(ToneMark::Hoi),
    Some(ToneMark::Nga),
    Some(ToneMark::Nang),
];

/// Lookup table mapping every Vietnamese vowel glyph to its base vowel, optional
/// vowel mark, and tone variants.
///
/// Each row is `(base, mark, [no_tone, huyền, sắc, hỏi, ngã, nặng])`. All glyphs
/// are listed in lowercase; uppercase input is folded to lowercase before lookup.
const VOWEL_TABLE: &[(char, Option<VowelMark>, [char; 6])] = &[
    ('a', None, ['a', 'à', 'á', 'ả', 'ã', 'ạ']),
    ('a', Some(VowelMark::Breve), ['ă', 'ằ', 'ắ', 'ẳ', 'ẵ', 'ặ']),
    ('a', Some(VowelMark::Circumflex), ['â', 'ầ', 'ấ', 'ẩ', 'ẫ', 'ậ']),
    ('e', None, ['e', 'è', 'é', 'ẻ', 'ẽ', 'ẹ']),
    ('e', Some(VowelMark::Circumflex), ['ê', 'ề', 'ế', 'ể', 'ễ', 'ệ']),
    ('i', None, ['i', 'ì', 'í', 'ỉ', 'ĩ', 'ị']),
    ('o', None, ['o', 'ò', 'ó', 'ỏ', 'õ', 'ọ']),
    ('o', Some(VowelMark::Circumflex), ['ô', 'ồ', 'ố', 'ổ', 'ỗ', 'ộ']),
    ('o', Some(VowelMark::Horn), ['ơ', 'ờ', 'ớ', 'ở', 'ỡ', 'ợ']),
    ('u', None, ['u', 'ù', 'ú', 'ủ', 'ũ', 'ụ']),
    ('u', Some(VowelMark::Horn), ['ư', 'ừ', 'ứ', 'ử', 'ữ', 'ự']),
    ('y', None, ['y', 'ỳ', 'ý', 'ỷ', 'ỹ', 'ỵ']),
];

/// The set of base Vietnamese vowels (without any diacritics).
const BASE_VOWELS: [char; 6] = ['a', 'e', 'i', 'o', 'u', 'y'];

/// Decompose a single Vietnamese vowel glyph into its base vowel, optional vowel
/// mark, and optional tone mark.
///
/// Returns `None` if `c` is not a Vietnamese vowel. Case is preserved only for
/// the returned base vowel insofar as the input is folded to lowercase for the
/// lookup; the returned base is always lowercase.
fn decompose_vowel(c: char) -> Option<(char, Option<VowelMark>, Option<ToneMark>)> {
    let lower = c.to_lowercase().next().unwrap_or(c);
    for (base, mark, variants) in VOWEL_TABLE.iter() {
        if let Some(idx) = variants.iter().position(|&v| v == lower) {
            return Some((*base, *mark, TONE_ORDER[idx]));
        }
    }
    None
}

/// Returns `true` if `c` is a Vietnamese vowel — either a plain base vowel
/// (`a e i o u y`) or any of its marked/toned variants (e.g. `â`, `ơ`, `ế`).
///
/// The check is case-insensitive.
pub fn is_vietnamese_vowel(c: char) -> bool {
    decompose_vowel(c).is_some()
}

/// Returns `true` if `c` is a plain, unmarked base Vietnamese vowel
/// (`a e i o u y`), ignoring case.
pub fn is_base_vowel(c: char) -> bool {
    let lower = c.to_lowercase().next().unwrap_or(c);
    BASE_VOWELS.contains(&lower)
}

/// Returns the underlying base vowel for any Vietnamese vowel glyph, or `None`
/// if `c` is not a vowel. For example, `ế` → `e`, `ư` → `u`, `á` → `a`.
pub fn base_vowel(c: char) -> Option<char> {
    decompose_vowel(c).map(|(base, _, _)| base)
}

/// Returns the [`VowelMark`] carried by a vowel glyph, if any.
pub fn vowel_mark_of(c: char) -> Option<VowelMark> {
    decompose_vowel(c).and_then(|(_, mark, _)| mark)
}

/// Returns the [`ToneMark`] carried by a vowel glyph, if any.
pub fn tone_mark_of(c: char) -> Option<ToneMark> {
    decompose_vowel(c).and_then(|(_, _, tone)| tone)
}

/// A single vowel within a vowel cluster: a base vowel plus an optional
/// modification mark (circumflex, breve, or horn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VowelUnit {
    /// The base vowel character: one of `a e i o u y`.
    pub base: char,
    /// The optional vowel modification mark applied to the base.
    pub mark: Option<VowelMark>,
}

impl VowelUnit {
    /// Create a new [`VowelUnit`] from a base vowel and optional mark.
    pub fn new(base: char, mark: Option<VowelMark>) -> Self {
        VowelUnit { base, mark }
    }

    /// Build a [`VowelUnit`] from a rendered vowel glyph, discarding any tone
    /// mark (tones belong to the syllable, not the vowel unit).
    ///
    /// Returns `None` if `c` is not a Vietnamese vowel.
    pub fn from_char(c: char) -> Option<Self> {
        decompose_vowel(c).map(|(base, mark, _)| VowelUnit { base, mark })
    }
}

/// The nucleus of a syllable: an ordered cluster of one or more [`VowelUnit`]s.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VowelCluster {
    /// The vowels making up the cluster, in written order.
    pub vowels: Vec<VowelUnit>,
}

impl VowelCluster {
    /// Create an empty vowel cluster.
    pub fn new() -> Self {
        VowelCluster { vowels: Vec::new() }
    }

    /// Build a vowel cluster from a slice of [`VowelUnit`]s.
    pub fn from_units(units: Vec<VowelUnit>) -> Self {
        VowelCluster { vowels: units }
    }

    /// Parse a string of contiguous vowel glyphs into a [`VowelCluster`].
    ///
    /// Returns `None` if any character in `s` is not a Vietnamese vowel.
    pub fn parse(s: &str) -> Option<Self> {
        let mut vowels = Vec::new();
        for c in s.chars() {
            vowels.push(VowelUnit::from_char(c)?);
        }
        Some(VowelCluster { vowels })
    }

    /// The number of vowels in the cluster.
    pub fn len(&self) -> usize {
        self.vowels.len()
    }

    /// Returns `true` if the cluster contains no vowels.
    pub fn is_empty(&self) -> bool {
        self.vowels.is_empty()
    }

    /// Returns the base-vowel string of the cluster, ignoring marks and tones
    /// (e.g. a cluster rendered "ươ" yields "uo").
    pub fn base_string(&self) -> String {
        self.vowels.iter().map(|v| v.base).collect()
    }

    /// Returns `true` if any vowel in the cluster carries a modification mark
    /// (circumflex, breve, or horn). Such vowels take tone-placement priority.
    pub fn has_marked_vowel(&self) -> bool {
        self.vowels.iter().any(|v| v.mark.is_some())
    }
}

/// A parsed Vietnamese syllable decomposed into onset, nucleus, coda, and tone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Syllable {
    /// Optional initial consonant cluster (e.g. "ng", "tr", "ph").
    pub onset: Option<String>,
    /// The vowel cluster forming the syllable nucleus (mandatory).
    pub nucleus: VowelCluster,
    /// Optional final consonant cluster (e.g. "n", "ng", "c", "t").
    pub coda: Option<String>,
    /// The tone mark applied to the syllable, if any.
    pub tone: Option<ToneMark>,
}

impl Syllable {
    /// Construct a syllable from its components.
    pub fn new(
        onset: Option<String>,
        nucleus: VowelCluster,
        coda: Option<String>,
        tone: Option<ToneMark>,
    ) -> Self {
        Syllable {
            onset,
            nucleus,
            coda,
            tone,
        }
    }

    /// Parse a rendered syllable string into its phonological components.
    ///
    /// The parser scans left to right, collecting leading consonants as the
    /// onset, the contiguous run of vowels as the nucleus, and trailing
    /// consonants as the coda. The tone is extracted from whichever nucleus
    /// vowel carries it.
    ///
    /// Returns `None` if the input contains no vowel (Vietnamese syllables must
    /// have a nucleus) or contains characters that are neither vowels nor ASCII
    /// letters.
    pub fn parse(s: &str) -> Option<Self> {
        if s.is_empty() {
            return None;
        }

        let chars: Vec<char> = s.chars().collect();

        // Locate the start of the nucleus (first vowel) and the end of the
        // contiguous vowel run that follows it.
        let first_vowel = chars.iter().position(|&c| is_vietnamese_vowel(c))?;
        let mut last_vowel = first_vowel;
        while last_vowel + 1 < chars.len() && is_vietnamese_vowel(chars[last_vowel + 1]) {
            last_vowel += 1;
        }

        let onset_chars = &chars[..first_vowel];
        let nucleus_chars = &chars[first_vowel..=last_vowel];
        let coda_chars = &chars[last_vowel + 1..];

        // Onset and coda must be ASCII letters (consonants); reject anything
        // else (digits, punctuation, stray vowels in the coda).
        if onset_chars.iter().any(|c| !c.is_ascii_alphabetic())
            || coda_chars.iter().any(|c| !c.is_ascii_alphabetic())
        {
            return None;
        }

        let nucleus = VowelCluster::parse(&nucleus_chars.iter().collect::<String>())?;

        // The tone is whatever tone any nucleus vowel carries (a well-formed
        // syllable has at most one).
        let tone = nucleus_chars.iter().find_map(|&c| tone_mark_of(c));

        let onset = if onset_chars.is_empty() {
            None
        } else {
            Some(onset_chars.iter().collect())
        };
        let coda = if coda_chars.is_empty() {
            None
        } else {
            Some(coda_chars.iter().collect())
        };

        Some(Syllable {
            onset,
            nucleus,
            coda,
            tone,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_vietnamese_vowel_accepts_base_and_marked() {
        for c in ['a', 'e', 'i', 'o', 'u', 'y'] {
            assert!(is_vietnamese_vowel(c), "{c} should be a vowel");
        }
        for c in ['â', 'ă', 'ê', 'ô', 'ơ', 'ư', 'á', 'ế', 'ự'] {
            assert!(is_vietnamese_vowel(c), "{c} should be a vowel");
        }
    }

    #[test]
    fn is_vietnamese_vowel_rejects_consonants_and_symbols() {
        for c in ['b', 'c', 'd', 'đ', 'n', 'g', '1', ' ', '.'] {
            assert!(!is_vietnamese_vowel(c), "{c} should not be a vowel");
        }
    }

    #[test]
    fn is_vietnamese_vowel_is_case_insensitive() {
        assert!(is_vietnamese_vowel('A'));
        assert!(is_vietnamese_vowel('Ơ'));
        assert!(is_vietnamese_vowel('Ế'));
    }

    #[test]
    fn base_vowel_strips_marks_and_tones() {
        assert_eq!(base_vowel('ế'), Some('e'));
        assert_eq!(base_vowel('ư'), Some('u'));
        assert_eq!(base_vowel('á'), Some('a'));
        assert_eq!(base_vowel('ă'), Some('a'));
        assert_eq!(base_vowel('b'), None);
    }

    #[test]
    fn vowel_mark_of_identifies_marks() {
        assert_eq!(vowel_mark_of('â'), Some(VowelMark::Circumflex));
        assert_eq!(vowel_mark_of('ă'), Some(VowelMark::Breve));
        assert_eq!(vowel_mark_of('ơ'), Some(VowelMark::Horn));
        assert_eq!(vowel_mark_of('a'), None);
    }

    #[test]
    fn tone_mark_of_identifies_tones() {
        assert_eq!(tone_mark_of('á'), Some(ToneMark::Sac));
        assert_eq!(tone_mark_of('à'), Some(ToneMark::Huyen));
        assert_eq!(tone_mark_of('ả'), Some(ToneMark::Hoi));
        assert_eq!(tone_mark_of('ã'), Some(ToneMark::Nga));
        assert_eq!(tone_mark_of('ạ'), Some(ToneMark::Nang));
        assert_eq!(tone_mark_of('a'), None);
        // A marked vowel with a tone (ế = ê + sắc).
        assert_eq!(tone_mark_of('ế'), Some(ToneMark::Sac));
    }

    #[test]
    fn vowel_unit_from_char_keeps_mark_drops_tone() {
        // ế = base e, circumflex mark, sắc tone -> unit keeps e + circumflex.
        let unit = VowelUnit::from_char('ế').unwrap();
        assert_eq!(unit.base, 'e');
        assert_eq!(unit.mark, Some(VowelMark::Circumflex));

        let unit = VowelUnit::from_char('ự').unwrap();
        assert_eq!(unit.base, 'u');
        assert_eq!(unit.mark, Some(VowelMark::Horn));

        assert_eq!(VowelUnit::from_char('b'), None);
    }

    #[test]
    fn vowel_cluster_parse_and_base_string() {
        let cluster = VowelCluster::parse("ươ").unwrap();
        assert_eq!(cluster.len(), 2);
        assert_eq!(cluster.base_string(), "uo");
        assert!(cluster.has_marked_vowel());

        let plain = VowelCluster::parse("oa").unwrap();
        assert_eq!(plain.base_string(), "oa");
        assert!(!plain.has_marked_vowel());

        assert!(VowelCluster::parse("ab").is_none());
        assert!(VowelCluster::new().is_empty());
    }

    #[test]
    fn syllable_parse_onset_nucleus_coda_tone() {
        // "được" = onset "d"? Actually rendered đ; use ASCII-ish test below.
        let syl = Syllable::parse("nhà").unwrap();
        assert_eq!(syl.onset.as_deref(), Some("nh"));
        assert_eq!(syl.nucleus.base_string(), "a");
        assert_eq!(syl.coda, None);
        assert_eq!(syl.tone, Some(ToneMark::Huyen));
    }

    #[test]
    fn syllable_parse_with_coda() {
        let syl = Syllable::parse("toán").unwrap();
        assert_eq!(syl.onset.as_deref(), Some("t"));
        assert_eq!(syl.nucleus.base_string(), "oa");
        assert_eq!(syl.coda.as_deref(), Some("n"));
        assert_eq!(syl.tone, Some(ToneMark::Sac));
    }

    #[test]
    fn syllable_parse_no_onset() {
        let syl = Syllable::parse("ăn").unwrap();
        assert_eq!(syl.onset, None);
        assert_eq!(syl.nucleus.base_string(), "a");
        assert_eq!(syl.nucleus.vowels[0].mark, Some(VowelMark::Breve));
        assert_eq!(syl.coda.as_deref(), Some("n"));
        assert_eq!(syl.tone, None);
    }

    #[test]
    fn syllable_parse_three_vowel_cluster() {
        let syl = Syllable::parse("người").unwrap();
        assert_eq!(syl.onset.as_deref(), Some("ng"));
        assert_eq!(syl.nucleus.base_string(), "uoi");
        assert_eq!(syl.coda, None);
        assert_eq!(syl.tone, Some(ToneMark::Huyen));
    }

    #[test]
    fn syllable_parse_rejects_no_vowel() {
        assert!(Syllable::parse("ng").is_none());
        assert!(Syllable::parse("").is_none());
    }
}
