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
}
