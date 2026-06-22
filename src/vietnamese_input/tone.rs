//! Vietnamese tone-mark placement logic.
//!
//! Vietnamese syllables carry at most one tone mark, and that mark is rendered
//! on exactly one vowel of the syllable's vowel cluster (nucleus). When the
//! nucleus contains more than one vowel, phonology rules decide which vowel
//! receives the tone.
//!
//! [`TonePlacement::find_tone_target`] applies the following rules, in priority
//! order, to the *first contiguous run of vowels* in a rendered syllable string:
//!
//! 1. If the cluster contains a quality-marked vowel `ơ, ô, â, ă, ê`, the tone
//!    goes on that vowel. (Note that `ư` is deliberately *not* in this set: in
//!    the `ươ` diphthong the tone belongs on `ơ`, so excluding `ư` lets the
//!    higher-priority `ơ` win.)
//! 2. If the cluster has three vowels, the tone goes on the middle vowel.
//! 3. If the cluster has two vowels and the syllable ends with a consonant
//!    (has a coda), the tone goes on the first vowel.
//! 4. If the cluster has two vowels and there is no trailing consonant, the
//!    tone goes on the second vowel.
//!
//! A single-vowel cluster always receives the tone on that lone vowel.
//!
//! The returned index is the **character index** (position within the
//! `char` sequence of the syllable), not a byte offset — Vietnamese vowel
//! glyphs are multi-byte in UTF-8, so callers should index via `.chars()`.

use super::syllable::{base_vowel, is_vietnamese_vowel, vowel_mark_of};
use super::VowelMark;

/// Tone placement helper following Vietnamese phonology rules.
pub struct TonePlacement;

impl TonePlacement {
    /// Determine which vowel of `syllable` should carry the tone mark.
    ///
    /// Returns the character index (0-based position within the syllable's
    /// `char` sequence) of the target vowel, or `None` if the syllable
    /// contains no vowel.
    ///
    /// See the [module documentation](self) for the placement rules.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// // "biên": cluster "iê", ê is a priority vowel -> index of ê (2).
    /// assert_eq!(TonePlacement::find_tone_target("biên"), Some(2));
    /// // "oa": two vowels, no coda -> second vowel (index 1).
    /// assert_eq!(TonePlacement::find_tone_target("oa"), Some(1));
    /// // "oan": two vowels with a coda -> first vowel (index 0).
    /// assert_eq!(TonePlacement::find_tone_target("oan"), Some(0));
    /// ```
    pub fn find_tone_target(syllable: &str) -> Option<usize> {
        let chars: Vec<char> = syllable.chars().collect();

        // Locate the first contiguous run of vowels (the nucleus). Collect the
        // character indices of those vowels.
        let first_vowel = chars.iter().position(|&c| is_vietnamese_vowel(c))?;
        let mut nucleus: Vec<usize> = vec![first_vowel];
        let mut i = first_vowel + 1;
        while i < chars.len() && is_vietnamese_vowel(chars[i]) {
            nucleus.push(i);
            i += 1;
        }

        // Rule 1: a quality-marked vowel (ơ, ô, â, ă, ê) always takes the tone.
        if let Some(&idx) = nucleus.iter().find(|&&idx| is_priority_vowel(chars[idx])) {
            return Some(idx);
        }

        // Whether the syllable has a trailing consonant after the nucleus.
        let last = *nucleus.last().expect("nucleus is non-empty");
        let has_coda = last + 1 < chars.len();

        match nucleus.len() {
            // Single vowel: the tone goes on it.
            1 => Some(nucleus[0]),
            // Rule 3 / Rule 4: two-vowel cluster depends on the coda.
            2 => {
                if has_coda {
                    Some(nucleus[0])
                } else {
                    Some(nucleus[1])
                }
            }
            // Rule 2: three (or more) vowels -> the middle vowel.
            _ => Some(nucleus[nucleus.len() / 2]),
        }
    }
}

/// Returns `true` if `c` is one of the quality-marked vowels that take
/// tone-placement priority: `ơ, ô, â, ă, ê`.
///
/// The horned `ư` (u + horn) is intentionally excluded so that in the `ươ`
/// diphthong the tone resolves onto `ơ`.
fn is_priority_vowel(c: char) -> bool {
    matches!(
        (base_vowel(c), vowel_mark_of(c)),
        // ô, ơ
        (Some('o'), Some(VowelMark::Circumflex))
            | (Some('o'), Some(VowelMark::Horn))
            // â, ă
            | (Some('a'), Some(VowelMark::Circumflex))
            | (Some('a'), Some(VowelMark::Breve))
            // ê
            | (Some('e'), Some(VowelMark::Circumflex))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Rule 1: priority (quality-marked) vowels -------------------------

    #[test]
    fn rule1_priority_vowel_takes_tone() {
        // "biên": cluster "iê"; ê (index 2) is a priority vowel.
        assert_eq!(TonePlacement::find_tone_target("biên"), Some(2));
        // "đươc": cluster "ươ"; ơ (index 2) wins over the non-priority ư.
        assert_eq!(TonePlacement::find_tone_target("đươc"), Some(2));
        // "uôi": ô (index 1) is the priority vowel.
        assert_eq!(TonePlacement::find_tone_target("uôi"), Some(1));
    }

    #[test]
    fn rule1_each_priority_vowel() {
        // ô at index 0.
        assert_eq!(TonePlacement::find_tone_target("ôn"), Some(0));
        // ơ at index 0.
        assert_eq!(TonePlacement::find_tone_target("ơn"), Some(0));
        // â at index 0.
        assert_eq!(TonePlacement::find_tone_target("ân"), Some(0));
        // ă at index 0.
        assert_eq!(TonePlacement::find_tone_target("ăn"), Some(0));
        // ê at index 0.
        assert_eq!(TonePlacement::find_tone_target("êm"), Some(0));
    }

    #[test]
    fn rule1_horned_u_is_not_priority() {
        // "ưa": ư is NOT a priority vowel, so this falls through to the
        // positional rules (two vowels, no coda -> second vowel, index 1).
        assert_eq!(TonePlacement::find_tone_target("ưa"), Some(1));
    }

    // --- Rule 2: three-vowel cluster -> middle vowel ----------------------

    #[test]
    fn rule2_three_vowels_middle() {
        // "oai": no priority vowel, three vowels -> middle (index 1).
        assert_eq!(TonePlacement::find_tone_target("oai"), Some(1));
        // "uoi" (plain bases): middle vowel index 1.
        assert_eq!(TonePlacement::find_tone_target("uoi"), Some(1));
        // With an onset consonant the indices shift accordingly:
        // "ngoai" -> n(0) g(1) o(2) a(3) i(4); middle of nucleus is index 3.
        assert_eq!(TonePlacement::find_tone_target("ngoai"), Some(3));
    }

    // --- Rule 3: two vowels with a coda -> first vowel --------------------

    #[test]
    fn rule3_two_vowels_with_coda_first() {
        // "oan": two vowels, coda "n" -> first vowel (index 0).
        assert_eq!(TonePlacement::find_tone_target("oan"), Some(0));
        // "oac": coda "c" -> first vowel.
        assert_eq!(TonePlacement::find_tone_target("oac"), Some(0));
        // "uyt" -> two vowels with coda; first vowel index 0.
        assert_eq!(TonePlacement::find_tone_target("uyt"), Some(0));
    }

    // --- Rule 4: two vowels, no coda -> second vowel ----------------------

    #[test]
    fn rule4_two_vowels_no_coda_second() {
        assert_eq!(TonePlacement::find_tone_target("oa"), Some(1));
        assert_eq!(TonePlacement::find_tone_target("oe"), Some(1));
        assert_eq!(TonePlacement::find_tone_target("oo"), Some(1));
        assert_eq!(TonePlacement::find_tone_target("ua"), Some(1));
        assert_eq!(TonePlacement::find_tone_target("ue"), Some(1));
        assert_eq!(TonePlacement::find_tone_target("uo"), Some(1));
    }

    // --- Single vowel & onset handling ------------------------------------

    #[test]
    fn single_vowel_takes_tone() {
        // "an": single vowel at index 0.
        assert_eq!(TonePlacement::find_tone_target("an"), Some(0));
        // "ba": onset "b", vowel "a" at index 1.
        assert_eq!(TonePlacement::find_tone_target("ba"), Some(1));
        // "tho": onset "th", vowel "o" at index 2.
        assert_eq!(TonePlacement::find_tone_target("tho"), Some(2));
    }

    // --- Edge cases -------------------------------------------------------

    #[test]
    fn no_vowel_returns_none() {
        assert_eq!(TonePlacement::find_tone_target("ng"), None);
        assert_eq!(TonePlacement::find_tone_target(""), None);
        assert_eq!(TonePlacement::find_tone_target("123"), None);
    }

    #[test]
    fn priority_overrides_positional_rules() {
        // "uyê" style: "tuyên" -> t(0) u(1) y(2) ê(3) n(4); ê is priority.
        assert_eq!(TonePlacement::find_tone_target("tuyên"), Some(3));
    }
}
