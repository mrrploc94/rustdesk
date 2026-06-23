//! Unicode normalization for composed Vietnamese text.
//!
//! Vietnamese characters can be represented in multiple Unicode forms. For
//! example, the character `ế` can be encoded as a single precomposed codepoint
//! (`U+1EBF`) or as a base letter followed by combining diacritical marks
//! (`e` + combining circumflex `U+0302` + combining acute `U+0301`).
//!
//! The [`UnicodeNormalizer`] converts composed output into a consistent
//! normalization form so that remote systems receive predictable, canonically
//! ordered text. NFC (Normalization Form C, fully composed) is used by default
//! because it maximizes compatibility with remote operating systems and input
//! pipelines. NFD (Normalization Form D, fully decomposed) is provided for
//! callers that require base characters plus separate combining marks.
//!
//! Both forms guarantee that combining diacritics are emitted in Unicode
//! canonical order.

use unicode_normalization::UnicodeNormalization;

/// The Unicode normalization form applied to composed Vietnamese text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizationForm {
    /// Normalization Form C — canonical composition (single codepoint per
    /// character where possible). This is the default.
    Nfc,
    /// Normalization Form D — canonical decomposition (base character plus
    /// separate combining marks).
    Nfd,
}

impl Default for NormalizationForm {
    fn default() -> Self {
        NormalizationForm::Nfc
    }
}

/// Normalizes composed Vietnamese text into a configured Unicode normalization
/// form, ensuring combining diacritics are in canonical order.
#[derive(Debug, Clone, Default)]
pub struct UnicodeNormalizer {
    form: NormalizationForm,
}

impl UnicodeNormalizer {
    /// Create a new normalizer using the default form (NFC).
    pub fn new() -> Self {
        UnicodeNormalizer {
            form: NormalizationForm::default(),
        }
    }

    /// Create a normalizer with an explicit normalization form.
    pub fn with_form(form: NormalizationForm) -> Self {
        UnicodeNormalizer { form }
    }

    /// Return the currently configured normalization form.
    pub fn form(&self) -> NormalizationForm {
        self.form
    }

    /// Change the normalization form applied by [`normalize`](Self::normalize).
    pub fn set_form(&mut self, form: NormalizationForm) {
        self.form = form;
    }

    /// Normalize `input` according to the configured form.
    ///
    /// The output is canonically equivalent to the input (same rendered glyph)
    /// with all combining diacritics in canonical order.
    pub fn normalize(&self, input: &str) -> String {
        match self.form {
            NormalizationForm::Nfc => input.nfc().collect(),
            NormalizationForm::Nfd => input.nfd().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Base 'e' + combining circumflex (U+0302) + combining acute (U+0301).
    const DECOMPOSED_E_CIRCUMFLEX_ACUTE: &str = "e\u{0302}\u{0301}";
    // Precomposed 'ế' (U+1EBF).
    const PRECOMPOSED_E_CIRCUMFLEX_ACUTE: &str = "\u{1EBF}";

    #[test]
    fn default_form_is_nfc() {
        let normalizer = UnicodeNormalizer::new();
        assert_eq!(normalizer.form(), NormalizationForm::Nfc);
        assert_eq!(NormalizationForm::default(), NormalizationForm::Nfc);
    }

    #[test]
    fn nfc_composes_combining_diacritics() {
        let normalizer = UnicodeNormalizer::new();
        // The decomposed sequence should collapse into the single precomposed
        // codepoint 'ế'.
        let result = normalizer.normalize(DECOMPOSED_E_CIRCUMFLEX_ACUTE);
        assert_eq!(result, PRECOMPOSED_E_CIRCUMFLEX_ACUTE);
        assert_eq!(result.chars().count(), 1);
    }

    #[test]
    fn nfc_is_idempotent_on_precomposed() {
        let normalizer = UnicodeNormalizer::new();
        let result = normalizer.normalize(PRECOMPOSED_E_CIRCUMFLEX_ACUTE);
        assert_eq!(result, PRECOMPOSED_E_CIRCUMFLEX_ACUTE);
    }

    #[test]
    fn nfd_decomposes_precomposed_character() {
        let normalizer = UnicodeNormalizer::with_form(NormalizationForm::Nfd);
        // The precomposed 'ế' should decompose into base 'e' plus combining
        // marks in canonical order (circumflex before acute).
        let result = normalizer.normalize(PRECOMPOSED_E_CIRCUMFLEX_ACUTE);
        assert_eq!(result, DECOMPOSED_E_CIRCUMFLEX_ACUTE);
        assert_eq!(result.chars().count(), 3);
    }

    #[test]
    fn nfc_and_nfd_are_canonically_equivalent() {
        let nfc = UnicodeNormalizer::with_form(NormalizationForm::Nfc);
        let nfd = UnicodeNormalizer::with_form(NormalizationForm::Nfd);
        // Both representations of 'ế' must round-trip to the same NFC form.
        assert_eq!(
            nfc.normalize(DECOMPOSED_E_CIRCUMFLEX_ACUTE),
            nfc.normalize(PRECOMPOSED_E_CIRCUMFLEX_ACUTE)
        );
        // And to the same NFD form.
        assert_eq!(
            nfd.normalize(DECOMPOSED_E_CIRCUMFLEX_ACUTE),
            nfd.normalize(PRECOMPOSED_E_CIRCUMFLEX_ACUTE)
        );
    }

    #[test]
    fn set_form_switches_normalization() {
        let mut normalizer = UnicodeNormalizer::new();
        assert_eq!(
            normalizer.normalize(DECOMPOSED_E_CIRCUMFLEX_ACUTE),
            PRECOMPOSED_E_CIRCUMFLEX_ACUTE
        );
        normalizer.set_form(NormalizationForm::Nfd);
        assert_eq!(normalizer.form(), NormalizationForm::Nfd);
        assert_eq!(
            normalizer.normalize(PRECOMPOSED_E_CIRCUMFLEX_ACUTE),
            DECOMPOSED_E_CIRCUMFLEX_ACUTE
        );
    }

    #[test]
    fn normalizes_full_vietnamese_word() {
        let normalizer = UnicodeNormalizer::new();
        // "Việt" assembled from base letters + combining marks should compose
        // into precomposed Vietnamese characters.
        let decomposed = "Vie\u{0323}\u{0302}t"; // V, i, e + dot below + circumflex, t
        let result = normalizer.normalize(decomposed);
        assert_eq!(result, "Việt");
    }

    #[test]
    fn empty_input_returns_empty() {
        let normalizer = UnicodeNormalizer::new();
        assert_eq!(normalizer.normalize(""), "");
    }

    #[test]
    fn ascii_text_is_unchanged() {
        let nfc = UnicodeNormalizer::new();
        let nfd = UnicodeNormalizer::with_form(NormalizationForm::Nfd);
        assert_eq!(nfc.normalize("hello"), "hello");
        assert_eq!(nfd.normalize("hello"), "hello");
    }

    /// Table of (most) precomposed Vietnamese letters: the base vowels with
    /// every vowel-mark (circumflex/breve/horn) and tone-mark (sắc/huyền/hỏi/
    /// ngã/nặng) combination, plus the consonant đ. Each entry is a single
    /// precomposed (NFC) codepoint.
    const VIETNAMESE_PRECOMPOSED: &[char] = &[
        // a + tones
        'a', 'á', 'à', 'ả', 'ã', 'ạ',
        // ă (breve) + tones
        'ă', 'ắ', 'ằ', 'ẳ', 'ẵ', 'ặ',
        // â (circumflex) + tones
        'â', 'ấ', 'ầ', 'ẩ', 'ẫ', 'ậ',
        // e + tones
        'e', 'é', 'è', 'ẻ', 'ẽ', 'ẹ',
        // ê (circumflex) + tones
        'ê', 'ế', 'ề', 'ể', 'ễ', 'ệ',
        // i + tones
        'i', 'í', 'ì', 'ỉ', 'ĩ', 'ị',
        // o + tones
        'o', 'ó', 'ò', 'ỏ', 'õ', 'ọ',
        // ô (circumflex) + tones
        'ô', 'ố', 'ồ', 'ổ', 'ỗ', 'ộ',
        // ơ (horn) + tones
        'ơ', 'ớ', 'ờ', 'ở', 'ỡ', 'ợ',
        // u + tones
        'u', 'ú', 'ù', 'ủ', 'ũ', 'ụ',
        // ư (horn) + tones
        'ư', 'ứ', 'ừ', 'ử', 'ữ', 'ự',
        // y + tones
        'y', 'ý', 'ỳ', 'ỷ', 'ỹ', 'ỵ',
        // đ
        'đ',
        // a few uppercase variants for breadth
        'Ậ', 'Ế', 'Ộ', 'Ợ', 'Ự', 'Đ',
    ];

    // Feature: vietnamese-input-support, Property 9: Unicode Normalization Preservation
    //
    // **Validates: Requirements 5.1, 5.3, 5.4**
    //
    // For any composed Vietnamese character:
    //   * NFC normalization of both the precomposed form and an NFD-decomposed
    //     form produce the same NFC string (canonical equivalence between forms).
    //   * NFC(NFD(x)) == NFC(x) (round-trip / canonical-order preservation).
    //   * NFC is idempotent on the already-precomposed glyph, and the NFC result
    //     matches the reference `unicode-normalization` nfc iterator (same
    //     rendered glyph, canonical order).
    #[test]
    fn nfc_normalization_preserves_canonical_equivalence_for_vietnamese() {
        let nfc = UnicodeNormalizer::with_form(NormalizationForm::Nfc);
        let nfd = UnicodeNormalizer::with_form(NormalizationForm::Nfd);

        for &ch in VIETNAMESE_PRECOMPOSED {
            let precomposed: String = ch.to_string();

            // The fully decomposed (NFD) representation of the same character.
            let decomposed = nfd.normalize(&precomposed);

            // Reference NFC form straight from the crate's nfc iterator.
            let reference_nfc: String = precomposed.nfc().collect();

            // 5.1 / idempotence: NFC of an already-composed glyph is itself and
            // matches the reference normalization.
            let nfc_precomposed = nfc.normalize(&precomposed);
            assert_eq!(
                nfc_precomposed, reference_nfc,
                "NFC of precomposed {:?} must match the canonical NFC form",
                ch
            );

            // 5.4: both representations are canonically equivalent — NFC of the
            // decomposed form yields the same string as NFC of the precomposed
            // form (same rendered glyph, same semantic meaning).
            let nfc_decomposed = nfc.normalize(&decomposed);
            assert_eq!(
                nfc_decomposed, nfc_precomposed,
                "NFC of decomposed and precomposed {:?} must be canonically equivalent",
                ch
            );

            // 5.3: NFC then NFD then NFC round-trips back to the same NFC form,
            // confirming combining marks are emitted in canonical order.
            let round_trip = nfc.normalize(&nfd.normalize(&nfc_precomposed));
            assert_eq!(
                round_trip, nfc_precomposed,
                "NFC(NFD(NFC(x))) must equal NFC(x) for {:?}",
                ch
            );

            // The canonical NFC form of a single Vietnamese letter is a single
            // codepoint, while its NFD form decomposes into base + marks. The
            // decomposed form must therefore have at least as many codepoints,
            // and re-composing it must restore the original glyph.
            assert!(
                decomposed.chars().count() >= precomposed.chars().count(),
                "NFD of {:?} should not be shorter than its precomposed form",
                ch
            );
            assert_eq!(
                nfc.normalize(&decomposed),
                precomposed,
                "Re-composing the NFD form of {:?} must restore the precomposed glyph",
                ch
            );
        }
    }
}
