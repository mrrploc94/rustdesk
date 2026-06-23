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

    // ======================================================================
    // Task 2.3 — Unit tests for tone placement
    // ======================================================================
    // These cover every two-vowel cluster called out in the task, the
    // priority (quality-marked) vowels, three-vowel clusters resolving to the
    // middle, and single/no-vowel edge cases.

    #[test]
    fn two_vowel_clusters_no_coda_resolve_per_rules() {
        // Plain two-vowel clusters with no coda -> second vowel (index 1).
        assert_eq!(TonePlacement::find_tone_target("oa"), Some(1));
        assert_eq!(TonePlacement::find_tone_target("oe"), Some(1));
        assert_eq!(TonePlacement::find_tone_target("oo"), Some(1));
        assert_eq!(TonePlacement::find_tone_target("ua"), Some(1));
        assert_eq!(TonePlacement::find_tone_target("ue"), Some(1));
        assert_eq!(TonePlacement::find_tone_target("uo"), Some(1));
        // "ưa": ư is NOT a priority vowel -> positional, no coda -> second (1).
        assert_eq!(TonePlacement::find_tone_target("ưa"), Some(1));
    }

    #[test]
    fn two_vowel_clusters_with_priority_vowel() {
        // "uô": ô (index 1) is a priority vowel.
        assert_eq!(TonePlacement::find_tone_target("uô"), Some(1));
        // "ươ": ơ (index 1) wins over the non-priority ư.
        assert_eq!(TonePlacement::find_tone_target("ươ"), Some(1));
        // "iê": ê (index 1) is a priority vowel.
        assert_eq!(TonePlacement::find_tone_target("iê"), Some(1));
        // "uơ": ơ (index 1) is a priority vowel.
        assert_eq!(TonePlacement::find_tone_target("uơ"), Some(1));
    }

    #[test]
    fn priority_vowels_each_receive_tone_first() {
        // Each quality-marked vowel ơ, ô, â, ă, ê takes the tone even when it
        // is not in the positionally-favoured slot. Place each at index 0 of a
        // two-vowel cluster that ends with no coda (positional rule would
        // otherwise pick index 1).
        assert_eq!(TonePlacement::find_tone_target("ôa"), Some(0)); // ô first
        assert_eq!(TonePlacement::find_tone_target("ơa"), Some(0)); // ơ first
        assert_eq!(TonePlacement::find_tone_target("âu"), Some(0)); // â first
        assert_eq!(TonePlacement::find_tone_target("ău"), Some(0)); // ă first
        assert_eq!(TonePlacement::find_tone_target("êu"), Some(0)); // ê first
    }

    #[test]
    fn three_vowel_clusters_resolve_to_middle_or_priority() {
        // "oai": no priority vowel, three vowels -> middle (index 1).
        assert_eq!(TonePlacement::find_tone_target("oai"), Some(1));
        // "uôi": ô (index 1) is the priority vowel (also the middle here).
        assert_eq!(TonePlacement::find_tone_target("uôi"), Some(1));
        // "ươi": ơ (index 1) is the priority vowel.
        assert_eq!(TonePlacement::find_tone_target("ươi"), Some(1));
    }

    #[test]
    fn single_and_no_vowel_edge_cases() {
        // Single vowel always takes the tone, regardless of mark/coda.
        assert_eq!(TonePlacement::find_tone_target("a"), Some(0));
        assert_eq!(TonePlacement::find_tone_target("ư"), Some(0));
        assert_eq!(TonePlacement::find_tone_target("y"), Some(0));
        // No vowel -> None.
        assert_eq!(TonePlacement::find_tone_target("ng"), None);
        assert_eq!(TonePlacement::find_tone_target(""), None);
    }

    // ======================================================================
    // Task 2.2 — Property test for tone placement
    // ======================================================================
    // Feature: vietnamese-input-support, Property 4: Tone Placement Correctness

    use super::super::syllable::VowelUnit;

    /// Independent reference implementation of the tone-placement rules.
    ///
    /// This intentionally derives the target a different way from
    /// [`TonePlacement::find_tone_target`]: it works in terms of an *offset*
    /// within the decomposed nucleus and classifies priority vowels via their
    /// [`VowelUnit`] mark (any modification mark except the horn on `u`),
    /// rather than matching glyph categories directly. If the two ever
    /// disagree, the property test fails.
    fn reference_tone_target(syllable: &str) -> Option<usize> {
        let chars: Vec<char> = syllable.chars().collect();
        let first = chars.iter().position(|&c| is_vietnamese_vowel(c))?;

        // Collect the contiguous nucleus glyphs and note whether a coda follows.
        let mut nucleus: Vec<char> = Vec::new();
        let mut j = first;
        while j < chars.len() && is_vietnamese_vowel(chars[j]) {
            nucleus.push(chars[j]);
            j += 1;
        }
        let has_coda = j < chars.len();

        let units: Vec<VowelUnit> = nucleus
            .iter()
            .map(|&c| VowelUnit::from_char(c).expect("nucleus char is a vowel"))
            .collect();

        // A priority vowel is any marked vowel other than the horned `u` (ư).
        // That set is exactly {ô, ơ, â, ă, ê}.
        let priority = |u: &VowelUnit| -> bool {
            match (u.base, u.mark) {
                (_, None) => false,
                ('u', Some(VowelMark::Horn)) => false,
                (_, Some(_)) => true,
            }
        };

        let offset = if let Some(p) = units.iter().position(priority) {
            p
        } else {
            match units.len() {
                1 => 0,
                2 => {
                    if has_coda {
                        0
                    } else {
                        1
                    }
                }
                _ => units.len() / 2,
            }
        };

        Some(first + offset)
    }

    #[test]
    fn property_tone_placement_correctness() {
        // Consonant-only onsets and codas (no vowels) so the generated
        // nucleus is exactly the vowel run we splice in.
        const ONSETS: &[&str] = &[
            "", "b", "c", "d", "g", "h", "kh", "l", "m", "n", "ng", "nh", "ph", "t", "th", "tr",
            "v", "x",
        ];
        const VOWELS: &[char] = &[
            'a', 'ă', 'â', 'e', 'ê', 'i', 'o', 'ô', 'ơ', 'u', 'ư', 'y',
        ];
        const CODAS: &[&str] = &["", "c", "ch", "m", "n", "ng", "nh", "p", "t"];

        // Deterministic inline LCG PRNG (no external crates).
        let mut seed: u64 = 0xC0FFEE;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            // Use the high bits, which have the best statistical quality.
            seed >> 33
        };

        const CASES: usize = 250;
        for _ in 0..CASES {
            let onset = ONSETS[(next() as usize) % ONSETS.len()];
            let n_vowels = 1 + (next() as usize) % 3; // 1..=3 vowels
            let mut nucleus = String::new();
            for _ in 0..n_vowels {
                nucleus.push(VOWELS[(next() as usize) % VOWELS.len()]);
            }
            let coda = CODAS[(next() as usize) % CODAS.len()];

            let syllable = format!("{onset}{nucleus}{coda}");

            let expected = reference_tone_target(&syllable);
            let actual = TonePlacement::find_tone_target(&syllable);
            assert_eq!(
                actual, expected,
                "tone placement mismatch for syllable {syllable:?} \
                 (onset={onset:?}, nucleus={nucleus:?}, coda={coda:?})"
            );
        }
    }
}
