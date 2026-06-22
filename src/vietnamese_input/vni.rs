//! VNI input-method engine.
//!
//! The [`VniEngine`] implements the Vietnamese VNI input method, in which
//! diacritics are produced by typing number keys after a base character:
//!
//! - **Tone marks** (applied to the correct nucleus vowel):
//!   `1` = sắc (á), `2` = huyền (à), `3` = hỏi (ả), `4` = ngã (ã), `5` = nặng (ạ)
//! - **Vowel marks** (context dependent on the preceding base vowel):
//!   - `6` = circumflex/horn: `a` → â, `e` → ê, `o` → ô, `u` → ư
//!   - `7` = horn: `o` → ơ, `u` → ư, and `uo` → ươ (both vowels receive a horn)
//!   - `8` = breve: `a` → ă
//! - **Consonant mark**: `9` after `d` → đ
//!
//! Like the Telex engine, this engine operates on a [`CompositionBuffer`]: each
//! keystroke either appends a base character or rewrites the trailing
//! vowel/consonant of the in-flight composition. Every mutation is recorded as a
//! [`TransformStep`] so that backspace can reverse the composition in
//! last-applied-first-removed order (see [`CompositionBuffer::record_step`] /
//! [`CompositionBuffer::pop_step`]).
//!
//! Modifier keys that cannot drive a transformation (e.g. a `7` with no
//! preceding `o`/`u`, or a tone key with no vowel in the buffer) are kept
//! verbatim, so an invalid sequence is preserved as raw text and emitted
//! unchanged on flush.

use std::collections::HashMap;

use super::syllable::{base_vowel, is_vietnamese_vowel, tone_mark_of, vowel_mark_of};
use super::tone::TonePlacement;
use super::{CompositionBuffer, InputMethodEngine, ToneMark, TransformResult, TransformStep, VowelMark};

/// The vowel-mark category selected by a VNI number key. The concrete
/// [`VowelMark`] applied depends on the preceding base vowel (see
/// [`resolve_vowel_mark`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VniVowelMark {
    /// Key `6`: circumflex on `a`/`e`/`o` (â, ê, ô) or horn on `u` (ư).
    CircumflexOrHorn,
    /// Key `7`: horn on `o`/`u` (ơ, ư).
    Horn,
    /// Key `8`: breve on `a` (ă).
    Breve,
}

/// A VNI modifier produced by a single number key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VniModifier {
    /// A tone mark: `1`..=`5`.
    Tone(ToneMark),
    /// A vowel mark: `6`, `7`, `8`.
    VowelMark(VniVowelMark),
    /// The `đ` consonant mark: `9`.
    ConsonantMark,
}

/// Static table of VNI number keys mapped to the modifier they produce.
const NUMBER_KEYS: &[(char, VniModifier)] = &[
    ('1', VniModifier::Tone(ToneMark::Sac)),
    ('2', VniModifier::Tone(ToneMark::Huyen)),
    ('3', VniModifier::Tone(ToneMark::Hoi)),
    ('4', VniModifier::Tone(ToneMark::Nga)),
    ('5', VniModifier::Tone(ToneMark::Nang)),
    ('6', VniModifier::VowelMark(VniVowelMark::CircumflexOrHorn)),
    ('7', VniModifier::VowelMark(VniVowelMark::Horn)),
    ('8', VniModifier::VowelMark(VniVowelMark::Breve)),
    ('9', VniModifier::ConsonantMark),
];

/// Vowel composition table: every Vietnamese vowel glyph indexed by its base
/// vowel, optional vowel mark, and tone.
///
/// Each row is `(base, mark, [no_tone, huyền, sắc, hỏi, ngã, nặng])`. This lets
/// the engine build a composed glyph from a `(base, mark, tone)` triple. It is
/// kept local to this module (rather than shared with `telex.rs`) to avoid
/// cross-module coupling between the engines.
const COMPOSE_TABLE: &[(char, Option<VowelMark>, [char; 6])] = &[
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

/// Index into a [`COMPOSE_TABLE`] row for a given tone (matching the column
/// order `[none, huyền, sắc, hỏi, ngã, nặng]`).
fn tone_index(tone: Option<ToneMark>) -> usize {
    match tone {
        None => 0,
        Some(ToneMark::Huyen) => 1,
        Some(ToneMark::Sac) => 2,
        Some(ToneMark::Hoi) => 3,
        Some(ToneMark::Nga) => 4,
        Some(ToneMark::Nang) => 5,
    }
}

/// Compose a lowercase Vietnamese vowel glyph from a base vowel, optional vowel
/// mark, and optional tone. Returns `None` if the combination is not a valid
/// Vietnamese vowel (e.g. a horn on `a`).
fn compose_vowel(base: char, mark: Option<VowelMark>, tone: Option<ToneMark>) -> Option<char> {
    let idx = tone_index(tone);
    COMPOSE_TABLE
        .iter()
        .find(|(b, m, _)| *b == base && *m == mark)
        .map(|(_, _, variants)| variants[idx])
}

/// Recompose an existing vowel glyph with a new vowel mark and tone, preserving
/// the original letter case. Returns `None` if `orig` is not a vowel or the
/// requested combination is invalid.
fn recompose(orig: char, mark: Option<VowelMark>, tone: Option<ToneMark>) -> Option<char> {
    let base = base_vowel(orig)?;
    let composed = compose_vowel(base, mark, tone)?;
    if orig.is_uppercase() {
        composed.to_uppercase().next()
    } else {
        Some(composed)
    }
}

/// Resolve the concrete [`VowelMark`] applied by a VNI vowel-mark key to a given
/// base vowel, implementing the key 6/7/8 ambiguity rules. Returns `None` when
/// the key does not apply to that base vowel.
fn resolve_vowel_mark(base: char, mark: VniVowelMark) -> Option<VowelMark> {
    match (mark, base) {
        // Key 6: circumflex on a/e/o, horn on u.
        (VniVowelMark::CircumflexOrHorn, 'a') => Some(VowelMark::Circumflex),
        (VniVowelMark::CircumflexOrHorn, 'e') => Some(VowelMark::Circumflex),
        (VniVowelMark::CircumflexOrHorn, 'o') => Some(VowelMark::Circumflex),
        (VniVowelMark::CircumflexOrHorn, 'u') => Some(VowelMark::Horn),
        // Key 7: horn on o/u.
        (VniVowelMark::Horn, 'o') => Some(VowelMark::Horn),
        (VniVowelMark::Horn, 'u') => Some(VowelMark::Horn),
        // Key 8: breve on a.
        (VniVowelMark::Breve, 'a') => Some(VowelMark::Breve),
        _ => None,
    }
}

/// The VNI input-method engine.
#[derive(Debug, Clone)]
pub struct VniEngine {
    /// Lookup table from a VNI number key to the modifier it produces.
    number_mappings: HashMap<char, VniModifier>,
}

impl VniEngine {
    /// Create a new VNI engine with its static lookup tables initialised.
    pub fn new() -> Self {
        Self {
            number_mappings: NUMBER_KEYS.iter().copied().collect(),
        }
    }

    /// Returns `true` if the current composition contains at least one vowel.
    fn has_vowel(buffer: &CompositionBuffer) -> bool {
        buffer.current.chars().any(is_vietnamese_vowel)
    }

    /// Apply a tone mark to the correct nucleus vowel of the current
    /// composition. Returns `true` if a tone was applied.
    fn apply_tone(&self, buffer: &mut CompositionBuffer, key: char, tone: ToneMark) -> bool {
        let target = match TonePlacement::find_tone_target(&buffer.current) {
            Some(t) => t,
            None => return false,
        };
        let mut chars: Vec<char> = buffer.current.chars().collect();
        let c = chars[target];
        // Preserve the existing vowel mark; replace only the tone.
        let new_c = match recompose(c, vowel_mark_of(c), Some(tone)) {
            Some(x) => x,
            None => return false,
        };
        chars[target] = new_c;
        buffer.current = chars.into_iter().collect();
        buffer.raw_input.push(key);
        buffer.record_step(TransformStep::ToneMark {
            tone,
            target_vowel: new_c,
        });
        true
    }

    /// Apply a vowel mark selected by a VNI number key (`6`, `7`, `8`) to the
    /// trailing vowel of the current composition. A trailing `uo` cluster with
    /// key `7` receives a horn on both vowels (yielding `ươ`). Returns `true` if
    /// a vowel mark was applied.
    fn apply_vowel_mark(&self, buffer: &mut CompositionBuffer, key: char, vmark: VniVowelMark) -> bool {
        let chars: Vec<char> = buffer.current.chars().collect();
        let last = match chars.iter().rposition(|&c| is_vietnamese_vowel(c)) {
            Some(i) => i,
            None => return false,
        };
        let last_base = match base_vowel(chars[last]) {
            Some(b) => b,
            None => return false,
        };
        let mark = match resolve_vowel_mark(last_base, vmark) {
            Some(m) => m,
            None => return false,
        };
        let mut new_chars = chars.clone();
        // `uo` + key 7 → `ươ`: apply a horn to both vowels.
        if vmark == VniVowelMark::Horn
            && last_base == 'o'
            && last > 0
            && base_vowel(chars[last - 1]) == Some('u')
        {
            let prev = match recompose(
                chars[last - 1],
                Some(VowelMark::Horn),
                tone_mark_of(chars[last - 1]),
            ) {
                Some(x) => x,
                None => return false,
            };
            new_chars[last - 1] = prev;
        }
        let result_char = match recompose(chars[last], Some(mark), tone_mark_of(chars[last])) {
            Some(x) => x,
            None => return false,
        };
        new_chars[last] = result_char;
        buffer.current = new_chars.into_iter().collect();
        buffer.raw_input.push(key);
        buffer.record_step(TransformStep::VowelMark {
            base: last_base,
            result: result_char,
        });
        true
    }

    /// Apply the `9` → đ consonant mark when a `9` follows a `d`/`D`.
    /// Returns `true` if the mark was applied.
    fn apply_d9(&self, buffer: &mut CompositionBuffer, key: char) -> bool {
        let chars: Vec<char> = buffer.current.chars().collect();
        let last = match chars.last() {
            Some(&c) => c,
            None => return false,
        };
        if last.to_ascii_lowercase() != 'd' {
            return false;
        }
        let new_c = if last.is_uppercase() { 'Đ' } else { 'đ' };
        let mut new_chars = chars.clone();
        let n = new_chars.len();
        new_chars[n - 1] = new_c;
        buffer.current = new_chars.into_iter().collect();
        buffer.raw_input.push(key);
        buffer.record_step(TransformStep::ConsonantMark {
            base: 'd',
            result: new_c,
        });
        true
    }
}

impl Default for VniEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl InputMethodEngine for VniEngine {
    fn process_key(&self, buffer: &mut CompositionBuffer, key: char) -> TransformResult {
        // VNI modifier keys are the digits 1..=9.
        if let Some(&modifier) = self.number_mappings.get(&key) {
            match modifier {
                // 1..=5: tone marks — apply to the nucleus vowel when one
                // exists, otherwise keep the digit as a literal.
                VniModifier::Tone(tone) => {
                    if Self::has_vowel(buffer) && self.apply_tone(buffer, key, tone) {
                        return TransformResult::ToneApplied;
                    }
                    buffer.push_key(key);
                    return TransformResult::Transformed;
                }
                // 6, 7, 8: vowel marks (context dependent).
                VniModifier::VowelMark(vmark) => {
                    if self.apply_vowel_mark(buffer, key, vmark) {
                        return TransformResult::ToneApplied;
                    }
                    buffer.push_key(key);
                    return TransformResult::Transformed;
                }
                // 9: đ consonant mark.
                VniModifier::ConsonantMark => {
                    if self.apply_d9(buffer, key) {
                        return TransformResult::ToneApplied;
                    }
                    buffer.push_key(key);
                    return TransformResult::Transformed;
                }
            }
        }

        // Alphabetic keys are base characters.
        if key.is_ascii_alphabetic() {
            buffer.push_key(key);
            return TransformResult::Transformed;
        }

        // Anything else (space, punctuation, the digit 0) ends the composition.
        TransformResult::CommitAndPass(key)
    }

    fn name(&self) -> &str {
        "VNI"
    }

    fn is_composable_start(&self, c: char) -> bool {
        c.is_ascii_alphabetic()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{CompositionBuffer, TransformResult};

    /// Feed a sequence of keystrokes through the engine and return the composed
    /// (committed) text.
    fn compose(seq: &str) -> String {
        let engine = VniEngine::new();
        let mut buffer = CompositionBuffer::new("test-session".to_string());
        for ch in seq.chars() {
            engine.process_key(&mut buffer, ch);
        }
        buffer.commit()
    }

    /// Feed a sequence and return the raw keystrokes recorded in the buffer
    /// (what would be emitted if the sequence were flushed as invalid).
    fn raw(seq: &str) -> String {
        let engine = VniEngine::new();
        let mut buffer = CompositionBuffer::new("test-session".to_string());
        for ch in seq.chars() {
            engine.process_key(&mut buffer, ch);
        }
        buffer.flush_raw()
    }

    // --- Tone marks (keys 1..=5) ------------------------------------------

    #[test]
    fn tone_marks_on_single_vowel() {
        assert_eq!(compose("a1"), "á");
        assert_eq!(compose("a2"), "à");
        assert_eq!(compose("a3"), "ả");
        assert_eq!(compose("a4"), "ã");
        assert_eq!(compose("a5"), "ạ");
    }

    #[test]
    fn tone_marks_on_other_base_vowels() {
        assert_eq!(compose("e1"), "é");
        assert_eq!(compose("i1"), "í");
        assert_eq!(compose("o1"), "ó");
        assert_eq!(compose("u1"), "ú");
        assert_eq!(compose("y1"), "ý");
    }

    // --- Vowel marks (keys 6, 7, 8) ---------------------------------------

    #[test]
    fn circumflex_via_key6() {
        assert_eq!(compose("a6"), "â");
        assert_eq!(compose("e6"), "ê");
        assert_eq!(compose("o6"), "ô");
    }

    #[test]
    fn horn_via_key7() {
        assert_eq!(compose("o7"), "ơ");
        assert_eq!(compose("u7"), "ư");
    }

    #[test]
    fn key6_after_u_is_horn() {
        // Key 6 after 'u' resolves to a horn (ư), not a circumflex.
        assert_eq!(compose("u6"), "ư");
    }

    #[test]
    fn breve_via_key8() {
        assert_eq!(compose("a8"), "ă");
    }

    #[test]
    fn uo_cluster_with_key7_gets_double_horn() {
        // "uo7" → "ươ": both vowels receive a horn.
        assert_eq!(compose("uo7"), "ươ");
    }

    // --- Consonant mark (key 9) -------------------------------------------

    #[test]
    fn d9_becomes_d_stroke() {
        assert_eq!(compose("d9"), "đ");
        assert_eq!(compose("d9a"), "đa");
    }

    // --- Ambiguity resolution for keys 6 and 7 ----------------------------

    #[test]
    fn key6_ambiguity_by_base_vowel() {
        assert_eq!(compose("a6"), "â");
        assert_eq!(compose("e6"), "ê");
        assert_eq!(compose("o6"), "ô");
        assert_eq!(compose("u6"), "ư");
    }

    #[test]
    fn key7_ambiguity_by_base_vowel() {
        assert_eq!(compose("o7"), "ơ");
        assert_eq!(compose("u7"), "ư");
    }

    #[test]
    fn key7_after_a_does_not_compose() {
        // Key 7 only applies to o/u; after 'a' it is kept as a literal digit.
        assert_eq!(compose("a7"), "a7");
        assert_eq!(raw("a7"), "a7");
    }

    // --- Tone placement on clusters --------------------------------------

    #[test]
    fn tone_on_marked_vowel_takes_priority() {
        // "e6" → ê, then "1" places sắc on ê.
        assert_eq!(compose("e61"), "ế");
        // "uo7" → ươ, then "5" places nặng on ơ.
        assert_eq!(compose("uo75"), "ượ");
    }

    #[test]
    fn tone_on_two_vowel_cluster_no_coda_second_vowel() {
        // "oa" with sắc and no coda → tone on second vowel: "oá".
        assert_eq!(compose("oa1"), "oá");
    }

    #[test]
    fn tone_on_two_vowel_cluster_with_coda_first_vowel() {
        // A two-vowel cluster with a coda places the tone on the FIRST vowel:
        // "oan" + sắc → tone on 'o' → "óan".
        assert_eq!(compose("oan1"), "óan");
    }

    // --- Classic full-word cases -----------------------------------------

    #[test]
    fn classic_duoc() {
        // đ(d9) + u + o + 7(→ươ) + 5(nặng on ơ) + c → "được".
        assert_eq!(compose("d9uo75c"), "được");
    }

    #[test]
    fn classic_viet() {
        // V i e t + 6(→ê) + 5(nặng on ê) → "Việt".
        assert_eq!(compose("Viet65"), "Việt");
    }

    #[test]
    fn classic_viet_tone_before_coda() {
        // Tone applied before typing the coda also works: V i e 6 5 t.
        assert_eq!(compose("Vie65t"), "Việt");
    }

    #[test]
    fn classic_nguoi() {
        // ng + u + o + 7(→ươ) + 2(huyền on ơ) + i → "người".
        assert_eq!(compose("nguo72i"), "người");
    }

    #[test]
    fn classic_tieng() {
        // t i e + 6(→ê) + 1(sắc on ê) + ng → "tiếng".
        assert_eq!(compose("tie61ng"), "tiếng");
    }

    // --- Uppercase variants ----------------------------------------------

    #[test]
    fn uppercase_tone_and_marks() {
        assert_eq!(compose("A1"), "Á");
        assert_eq!(compose("A6"), "Â");
        assert_eq!(compose("O7"), "Ơ");
        assert_eq!(compose("D9"), "Đ");
    }

    #[test]
    fn mixed_case_word() {
        // "D9o6ng2" → "Đồng": Đ (d9), ô (o6), huyền on ô (2), n, g.
        assert_eq!(compose("D9o6ng2"), "Đồng");
    }

    // --- Invalid sequences → raw output ----------------------------------

    #[test]
    fn invalid_sequences_stay_raw() {
        // '7' with no preceding o/u → kept literally with the consonant.
        assert_eq!(compose("b7"), "b7");
        assert_eq!(raw("b7"), "b7");
        // '9' not following a 'd' → kept literally.
        assert_eq!(compose("a9"), "a9");
        assert_eq!(raw("a9"), "a9");
    }

    #[test]
    fn tone_key_without_vowel_is_literal() {
        // A leading tone-key digit with no vowel is kept verbatim.
        assert_eq!(compose("1"), "1");
        // 'b' before a digit with no vowel keeps the digit literal.
        assert_eq!(compose("b1"), "b1");
        // But a tone key after a vowel applies: "y4" → ngã on y → "ỹ".
        assert_eq!(compose("y4"), "ỹ");
    }

    #[test]
    fn flush_raw_preserves_full_keystroke_sequence() {
        // Even when composition succeeds, the raw keystrokes are retained.
        assert_eq!(raw("d9uo75c"), "d9uo75c");
        assert_eq!(raw("Viet65"), "Viet65");
    }

    // --- Backspace reversal (LIFO via buffer history) --------------------

    #[test]
    fn backspace_reverses_transformations_in_lifo_order() {
        let engine = VniEngine::new();
        let mut buffer = CompositionBuffer::new("s".to_string());
        for ch in "uo75".chars() {
            engine.process_key(&mut buffer, ch);
        }
        assert_eq!(buffer.current_text(), "ượ");

        // Undo tone (5): "ượ" → "ươ".
        buffer.pop_step();
        assert_eq!(buffer.current_text(), "ươ");
        // Undo horn (7): "ươ" → "uo".
        buffer.pop_step();
        assert_eq!(buffer.current_text(), "uo");
        // Undo base 'o': "uo" → "u".
        buffer.pop_step();
        assert_eq!(buffer.current_text(), "u");
        // Undo base 'u': empty.
        buffer.pop_step();
        assert!(buffer.is_empty());
    }

    // --- Sequence-length coverage (1..=5 keystrokes) ----------------------

    #[test]
    fn handles_sequences_of_varying_length() {
        // 1 keystroke.
        assert_eq!(compose("a"), "a");
        // 2 keystrokes.
        assert_eq!(compose("a1"), "á");
        // 3 keystrokes.
        assert_eq!(compose("o7n"), "ơn");
        // 4 keystrokes.
        assert_eq!(compose("uo75"), "ượ");
        // 5 keystrokes.
        assert_eq!(compose("d9uo7"), "đươ");
    }

    // --- Engine metadata --------------------------------------------------

    #[test]
    fn engine_name_and_composable_start() {
        let engine = VniEngine::new();
        assert_eq!(engine.name(), "VNI");
        assert!(engine.is_composable_start('a'));
        assert!(engine.is_composable_start('d'));
        assert!(!engine.is_composable_start('1'));
        assert!(!engine.is_composable_start(' '));
    }

    #[test]
    fn terminator_returns_commit_and_pass() {
        let engine = VniEngine::new();
        let mut buffer = CompositionBuffer::new("test".to_string());
        engine.process_key(&mut buffer, 'a');
        engine.process_key(&mut buffer, '1');
        let result = engine.process_key(&mut buffer, ' ');
        assert_eq!(result, TransformResult::CommitAndPass(' '));
        // The terminator did not alter the composed text.
        assert_eq!(buffer.current_text(), "á");
    }
}
