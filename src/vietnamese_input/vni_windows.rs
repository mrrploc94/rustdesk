//! VNI Windows input-method engine.
//!
//! The [`VniWindowsEngine`] implements the "VNI for Windows" variant of the
//! Vietnamese VNI input method. It is built by **composition** over the
//! standard [`VniEngine`]: the Windows engine holds a base [`VniEngine`] and
//! delegates to it for all standard behaviour, layering Windows-specific
//! mapping variations on top via an `overrides` table.
//!
//! ## Processing order
//!
//! VNI Windows defines a canonical processing order for a syllable:
//!
//! ```text
//! base character modification (đ)  →  vowel mark (â/ê/ô/ơ/ư/ă)  →  tone mark
//! ```
//!
//! The base [`VniEngine`] already honours this order: applying a tone always
//! re-derives the glyph while preserving the existing vowel mark, and applying
//! a vowel mark preserves any existing tone (see `recompose` in
//! [`super::vni`]). The Windows engine relies on that behaviour so that
//! modifiers typed in any order converge on the canonically ordered result
//! (e.g. `a` `1` `6` and `a` `6` `1` both compose to `ấ`).
//!
//! ## Windows-specific overrides
//!
//! The `overrides` table maps a `(base_vowel, modifier_key)` pair to the glyph
//! that VNI Windows should produce, taking precedence over the standard VNI
//! transformation. This is the customization hook described by Requirement 3.4:
//! a Windows configuration (or a user-supplied custom configuration) populates
//! the table to vary the default number-key mappings.
//!
//! ## Fallback to standard VNI
//!
//! When no Windows configuration is available, the override table is empty and
//! the engine behaves **exactly** like the standard [`VniEngine`]
//! (Requirements 3.3 and 3.5). Construct such an engine with
//! [`VniWindowsEngine::standard`]; the default [`VniWindowsEngine::new`] is an
//! alias for it because no Windows variations are applied unless a
//! configuration supplies them.

use std::collections::HashMap;

use super::syllable::{base_vowel, is_vietnamese_vowel};
use super::vni::VniEngine;
use super::{CompositionBuffer, InputMethodEngine, TransformResult, TransformStep};

/// The VNI Windows input-method engine.
///
/// Composes over a base [`VniEngine`] (rather than inheriting from it) and
/// applies Windows-specific `(base, modifier) → glyph` overrides before falling
/// back to standard VNI behaviour.
#[derive(Debug, Clone)]
pub struct VniWindowsEngine {
    /// Base VNI engine providing all standard VNI transformation logic.
    base: VniEngine,
    /// Windows-specific overrides keyed by `(base_vowel, modifier_key)`.
    ///
    /// When a modifier key is typed and the trailing vowel's base together with
    /// the key match an entry here, the trailing vowel is rewritten to the
    /// mapped glyph instead of using standard VNI behaviour. An empty table
    /// means the engine is a pure standard-VNI fallback.
    overrides: HashMap<(char, char), char>,
}

impl VniWindowsEngine {
    /// Create a new VNI Windows engine.
    ///
    /// With no Windows configuration supplied, the engine falls back to
    /// standard VNI behaviour (Requirement 3.5). Use [`with_overrides`] to load
    /// Windows-specific variations from a configuration.
    ///
    /// [`with_overrides`]: VniWindowsEngine::with_overrides
    pub fn new() -> Self {
        Self::standard()
    }

    /// Create a VNI Windows engine that behaves exactly like standard VNI
    /// (no Windows-specific overrides).
    ///
    /// This is the fallback used when a VNI Windows configuration is
    /// unavailable or invalid (Requirements 3.3, 3.5).
    pub fn standard() -> Self {
        Self {
            base: VniEngine::new(),
            overrides: HashMap::new(),
        }
    }

    /// Create a VNI Windows engine with the given Windows-specific overrides.
    ///
    /// Each entry maps a `(base_vowel, modifier_key)` pair to the glyph that
    /// should be produced, overriding the standard VNI transformation for that
    /// pair (Requirement 3.4). Keys whose pair is absent from the table fall
    /// through to standard VNI behaviour.
    pub fn with_overrides(overrides: HashMap<(char, char), char>) -> Self {
        Self {
            base: VniEngine::new(),
            overrides,
        }
    }

    /// Returns `true` if any Windows-specific overrides are configured.
    pub fn has_overrides(&self) -> bool {
        !self.overrides.is_empty()
    }

    /// Attempt to apply a Windows-specific override for `key` to the trailing
    /// vowel of the current composition.
    ///
    /// Returns `Some(TransformResult)` when an override matched and was applied,
    /// or `None` when no override applies (in which case the caller should fall
    /// back to standard VNI behaviour). The original letter case of the
    /// trailing vowel is preserved.
    fn try_override(&self, buffer: &mut CompositionBuffer, key: char) -> Option<TransformResult> {
        // Overrides only ever apply to VNI modifier digits.
        if self.overrides.is_empty() || !key.is_ascii_digit() {
            return None;
        }

        let chars: Vec<char> = buffer.current.chars().collect();
        let last = chars.iter().rposition(|&c| is_vietnamese_vowel(c))?;
        let last_base = base_vowel(chars[last])?;
        let mapped = *self.overrides.get(&(last_base, key))?;

        let cased = if chars[last].is_uppercase() {
            mapped.to_uppercase().next().unwrap_or(mapped)
        } else {
            mapped
        };

        let mut new_chars = chars;
        new_chars[last] = cased;
        buffer.current = new_chars.into_iter().collect();
        buffer.raw_input.push(key);
        buffer.record_step(TransformStep::VowelMark {
            base: last_base,
            result: cased,
        });
        Some(TransformResult::ToneApplied)
    }
}

impl Default for VniWindowsEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl InputMethodEngine for VniWindowsEngine {
    fn process_key(&self, buffer: &mut CompositionBuffer, key: char) -> TransformResult {
        // Windows-specific overrides take precedence; otherwise delegate to the
        // base VNI engine, which enforces the canonical processing order
        // (base modification → vowel mark → tone mark).
        if let Some(result) = self.try_override(buffer, key) {
            return result;
        }
        self.base.process_key(buffer, key)
    }

    fn name(&self) -> &str {
        "VNI Windows"
    }

    fn is_composable_start(&self, c: char) -> bool {
        self.base.is_composable_start(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{CompositionBuffer, InputMethodEngine, TransformResult};
    use super::super::vni::VniEngine;

    /// Feed a sequence of keystrokes through a Windows engine and return the
    /// composed (committed) text.
    fn compose_win(engine: &VniWindowsEngine, seq: &str) -> String {
        let mut buffer = CompositionBuffer::new("test-session".to_string());
        for ch in seq.chars() {
            engine.process_key(&mut buffer, ch);
        }
        buffer.commit()
    }

    /// Feed a sequence through the standard VNI engine and return the composed
    /// text — used to assert fallback parity.
    fn compose_vni(seq: &str) -> String {
        let engine = VniEngine::new();
        let mut buffer = CompositionBuffer::new("test-session".to_string());
        for ch in seq.chars() {
            engine.process_key(&mut buffer, ch);
        }
        buffer.commit()
    }

    // --- Engine metadata --------------------------------------------------

    #[test]
    fn engine_name_is_vni_windows() {
        let engine = VniWindowsEngine::new();
        assert_eq!(engine.name(), "VNI Windows");
    }

    #[test]
    fn composable_start_matches_base_vni() {
        let engine = VniWindowsEngine::new();
        assert!(engine.is_composable_start('a'));
        assert!(engine.is_composable_start('d'));
        assert!(!engine.is_composable_start('1'));
        assert!(!engine.is_composable_start(' '));
    }

    #[test]
    fn default_engine_has_no_overrides() {
        let engine = VniWindowsEngine::default();
        assert!(!engine.has_overrides());
    }

    // --- Windows processing order: base → vowel mark → tone mark ----------

    #[test]
    fn processing_order_vowel_mark_then_tone() {
        // Canonical order: base 'a', vowel mark (6 → â), tone (1 → sắc) → "ấ".
        let engine = VniWindowsEngine::new();
        assert_eq!(compose_win(&engine, "a61"), "ấ");
    }

    #[test]
    fn processing_order_tone_then_vowel_mark_converges() {
        // Even when the tone is typed before the vowel mark, the canonical
        // ordering is preserved: "a16" also composes to "ấ".
        let engine = VniWindowsEngine::new();
        assert_eq!(compose_win(&engine, "a16"), "ấ");
    }

    #[test]
    fn processing_order_full_word_duoc() {
        // đ(d9) → vowel marks (uo7 → ươ) → tone (5 → nặng) → coda c = "được".
        let engine = VniWindowsEngine::new();
        assert_eq!(compose_win(&engine, "d9uo75c"), "được");
    }

    #[test]
    fn processing_order_full_word_viet() {
        let engine = VniWindowsEngine::new();
        assert_eq!(compose_win(&engine, "Viet65"), "Việt");
    }

    // --- Fallback to standard VNI behaviour -------------------------------

    #[test]
    fn fallback_matches_standard_vni_for_tone_marks() {
        let engine = VniWindowsEngine::standard();
        for seq in ["a1", "a2", "a3", "a4", "a5", "e1", "o1", "u1", "y1"] {
            assert_eq!(
                compose_win(&engine, seq),
                compose_vni(seq),
                "fallback mismatch for {seq}"
            );
        }
    }

    #[test]
    fn fallback_matches_standard_vni_for_vowel_marks() {
        let engine = VniWindowsEngine::standard();
        for seq in ["a6", "e6", "o6", "u6", "o7", "u7", "a8", "uo7", "d9"] {
            assert_eq!(
                compose_win(&engine, seq),
                compose_vni(seq),
                "fallback mismatch for {seq}"
            );
        }
    }

    #[test]
    fn fallback_matches_standard_vni_for_full_words() {
        let engine = VniWindowsEngine::standard();
        for seq in ["d9uo75c", "Viet65", "nguo72i", "tie61ng", "D9o6ng2"] {
            assert_eq!(
                compose_win(&engine, seq),
                compose_vni(seq),
                "fallback mismatch for {seq}"
            );
        }
    }

    #[test]
    fn new_engine_is_equivalent_to_standard_vni() {
        // With no configuration, VNI Windows == standard VNI.
        let engine = VniWindowsEngine::new();
        assert_eq!(compose_win(&engine, "d9uo75c"), compose_vni("d9uo75c"));
    }

    // --- Windows-specific override application ----------------------------

    #[test]
    fn override_applies_windows_specific_mapping() {
        // A Windows configuration remaps key 6 after 'a' to a breve (ă) instead
        // of the standard circumflex (â).
        let mut overrides = HashMap::new();
        overrides.insert(('a', '6'), 'ă');
        let engine = VniWindowsEngine::with_overrides(overrides);

        assert!(engine.has_overrides());
        // Windows override: "a6" → "ă" (standard VNI would give "â").
        assert_eq!(compose_win(&engine, "a6"), "ă");
        assert_eq!(compose_vni("a6"), "â");
    }

    #[test]
    fn override_preserves_letter_case() {
        let mut overrides = HashMap::new();
        overrides.insert(('a', '6'), 'ă');
        let engine = VniWindowsEngine::with_overrides(overrides);
        // Uppercase base vowel yields an uppercase override result.
        assert_eq!(compose_win(&engine, "A6"), "Ă");
    }

    #[test]
    fn override_falls_through_when_pair_absent() {
        // Override only covers ('a','6'); other pairs use standard VNI.
        let mut overrides = HashMap::new();
        overrides.insert(('a', '6'), 'ă');
        let engine = VniWindowsEngine::with_overrides(overrides);
        // ('o','6') is not overridden → standard VNI circumflex "ô".
        assert_eq!(compose_win(&engine, "o6"), "ô");
        // ('o','7') is not overridden → standard VNI horn "ơ".
        assert_eq!(compose_win(&engine, "o7"), "ơ");
    }

    #[test]
    fn override_only_triggers_on_digit_keys() {
        // An override keyed on a non-digit must never fire; alphabetic keys are
        // always treated as base characters.
        let mut overrides = HashMap::new();
        overrides.insert(('a', 'x'), 'z');
        let engine = VniWindowsEngine::with_overrides(overrides);
        // "ax" → base 'a' then base 'x' (no override, 'x' is not a VNI digit).
        assert_eq!(compose_win(&engine, "ax"), "ax");
    }

    // --- Terminator handling (delegated to base) --------------------------

    #[test]
    fn terminator_returns_commit_and_pass() {
        let engine = VniWindowsEngine::new();
        let mut buffer = CompositionBuffer::new("test".to_string());
        engine.process_key(&mut buffer, 'a');
        engine.process_key(&mut buffer, '1');
        let result = engine.process_key(&mut buffer, ' ');
        assert_eq!(result, TransformResult::CommitAndPass(' '));
        assert_eq!(buffer.current_text(), "á");
    }
}
