//! Telex input-method engine.
//!
//! The [`TelexEngine`] implements the Vietnamese Telex input method, in which
//! diacritics are produced by typing ordinary letters after a base character:
//!
//! - **Tone marks** (applied to the correct nucleus vowel):
//!   `s` = sắc (á), `f` = huyền (à), `r` = hỏi (ả), `x` = ngã (ã), `j` = nặng (ạ)
//! - **Vowel marks** via doubling: `aa` → â, `ee` → ê, `oo` → ô
//! - **Vowel marks** via `w` (context dependent): `aw` → ă, `ow` → ơ, `uw` → ư,
//!   and `uow` → ươ (both vowels receive a horn)
//! - **Consonant mark**: `dd` → đ
//!
//! The engine operates on a [`CompositionBuffer`]: each keystroke either appends
//! a base character or rewrites the trailing vowel/consonant of the in-flight
//! composition. Every mutation is recorded as a [`TransformStep`] so that
//! backspace can reverse the composition in last-applied-first-removed order
//! (see [`CompositionBuffer::record_step`] / [`CompositionBuffer::pop_step`]).
//!
//! Keys that cannot drive a transformation (e.g. a `w` with no preceding
//! `a/o/u`, or a tone key with no vowel in the buffer) are kept verbatim, so an
//! invalid sequence is preserved as raw text and emitted unchanged on flush.

use std::collections::HashMap;

use super::syllable::{base_vowel, is_vietnamese_vowel, tone_mark_of, vowel_mark_of};
use super::tone::TonePlacement;
use super::{CompositionBuffer, InputMethodEngine, ToneMark, TransformResult, TransformStep, VowelMark};

/// Static table of Telex tone keys mapped to the tone they apply.
const TONE_KEYS: &[(char, ToneMark)] = &[
    ('s', ToneMark::Sac),
    ('f', ToneMark::Huyen),
    ('r', ToneMark::Hoi),
    ('x', ToneMark::Nga),
    ('j', ToneMark::Nang),
];

/// Vowel composition table: every Vietnamese vowel glyph indexed by its base
/// vowel, optional vowel mark, and tone.
///
/// Each row is `(base, mark, [no_tone, huyền, sắc, hỏi, ngã, nặng])`. This is the
/// inverse of the decomposition table in [`super::syllable`]; it lets the engine
/// build a composed glyph from a `(base, mark, tone)` triple.
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

/// The Telex input-method engine.
#[derive(Debug, Clone)]
pub struct TelexEngine {
    /// Lookup table from a tone-trigger key to the tone it applies.
    tone_keys: HashMap<char, ToneMark>,
}

impl TelexEngine {
    /// Create a new Telex engine with its static lookup tables initialised.
    pub fn new() -> Self {
        Self {
            tone_keys: TONE_KEYS.iter().copied().collect(),
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

    /// Apply the `w` modifier: a horn to a trailing `o`/`u` (and to both vowels
    /// of a trailing `uo` cluster, yielding `ươ`), or a breve to a trailing `a`.
    /// Returns `true` if a vowel mark was applied.
    fn apply_w(&self, buffer: &mut CompositionBuffer, key: char) -> bool {
        let chars: Vec<char> = buffer.current.chars().collect();
        let last = match chars.iter().rposition(|&c| is_vietnamese_vowel(c)) {
            Some(i) => i,
            None => return false,
        };
        let last_base = match base_vowel(chars[last]) {
            Some(b) => b,
            None => return false,
        };
        let mut new_chars = chars.clone();
        let result_char;
        match last_base {
            'o' => {
                // `uo` + w → `ươ`: apply a horn to both vowels.
                if last > 0 && base_vowel(chars[last - 1]) == Some('u') {
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
                result_char = match recompose(chars[last], Some(VowelMark::Horn), tone_mark_of(chars[last])) {
                    Some(x) => x,
                    None => return false,
                };
                new_chars[last] = result_char;
            }
            'u' => {
                result_char = match recompose(chars[last], Some(VowelMark::Horn), tone_mark_of(chars[last])) {
                    Some(x) => x,
                    None => return false,
                };
                new_chars[last] = result_char;
            }
            'a' => {
                result_char = match recompose(chars[last], Some(VowelMark::Breve), tone_mark_of(chars[last])) {
                    Some(x) => x,
                    None => return false,
                };
                new_chars[last] = result_char;
            }
            // `w` after any other vowel does not compose.
            _ => return false,
        }
        buffer.current = new_chars.into_iter().collect();
        buffer.raw_input.push(key);
        buffer.record_step(TransformStep::VowelMark {
            base: last_base,
            result: result_char,
        });
        true
    }

    /// Apply a circumflex when a vowel is doubled (`aa` → â, `ee` → ê, `oo` → ô).
    /// `lower` is the lowercased typed key. Returns `true` if a mark was applied.
    fn apply_circumflex_double(&self, buffer: &mut CompositionBuffer, key: char, lower: char) -> bool {
        let chars: Vec<char> = buffer.current.chars().collect();
        let last = match chars.last() {
            Some(&c) => c,
            None => return false,
        };
        // The preceding character must be the same plain (unmarked) base vowel.
        if base_vowel(last) != Some(lower) || vowel_mark_of(last).is_some() {
            return false;
        }
        let new_c = match recompose(last, Some(VowelMark::Circumflex), tone_mark_of(last)) {
            Some(x) => x,
            None => return false,
        };
        let mut new_chars = chars.clone();
        let n = new_chars.len();
        new_chars[n - 1] = new_c;
        buffer.current = new_chars.into_iter().collect();
        buffer.raw_input.push(key);
        buffer.record_step(TransformStep::VowelMark {
            base: lower,
            result: new_c,
        });
        true
    }

    /// Apply the `dd` → đ consonant mark when a `d` follows another `d`.
    /// Returns `true` if the mark was applied.
    fn apply_dd(&self, buffer: &mut CompositionBuffer, key: char) -> bool {
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

impl Default for TelexEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl InputMethodEngine for TelexEngine {
    fn process_key(&self, buffer: &mut CompositionBuffer, key: char) -> TransformResult {
        // Non-letter keys (space, punctuation, digits) end the composition.
        if !key.is_ascii_alphabetic() {
            return TransformResult::CommitAndPass(key);
        }

        let lower = key.to_ascii_lowercase();

        // 1. Tone keys — apply to the nucleus vowel when one exists, otherwise
        //    keep the key as a literal consonant.
        if let Some(&tone) = self.tone_keys.get(&lower) {
            if Self::has_vowel(buffer) && self.apply_tone(buffer, key, tone) {
                return TransformResult::ToneApplied;
            }
            buffer.push_key(key);
            return TransformResult::Transformed;
        }

        // 2. `w` — horn/breve vowel mark (context dependent).
        if lower == 'w' {
            if self.apply_w(buffer, key) {
                return TransformResult::ToneApplied;
            }
            buffer.push_key(key);
            return TransformResult::Transformed;
        }

        // 3. Vowel doubling for circumflex: aa → â, ee → ê, oo → ô.
        if matches!(lower, 'a' | 'e' | 'o') && self.apply_circumflex_double(buffer, key, lower) {
            return TransformResult::ToneApplied;
        }

        // 4. Consonant mark: dd → đ.
        if lower == 'd' && self.apply_dd(buffer, key) {
            return TransformResult::ToneApplied;
        }

        // 5. Any other letter is a base character.
        buffer.push_key(key);
        TransformResult::Transformed
    }

    fn name(&self) -> &str {
        "Telex"
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
        let engine = TelexEngine::new();
        let mut buffer = CompositionBuffer::new("test-session".to_string());
        for ch in seq.chars() {
            engine.process_key(&mut buffer, ch);
        }
        buffer.commit()
    }

    /// Feed a sequence and return the raw keystrokes recorded in the buffer
    /// (what would be emitted if the sequence were flushed as invalid).
    fn raw(seq: &str) -> String {
        let engine = TelexEngine::new();
        let mut buffer = CompositionBuffer::new("test-session".to_string());
        for ch in seq.chars() {
            engine.process_key(&mut buffer, ch);
        }
        buffer.flush_raw()
    }

    // --- Tone marks -------------------------------------------------------

    #[test]
    fn tone_marks_on_single_vowel() {
        assert_eq!(compose("as"), "á");
        assert_eq!(compose("af"), "à");
        assert_eq!(compose("ar"), "ả");
        assert_eq!(compose("ax"), "ã");
        assert_eq!(compose("aj"), "ạ");
    }

    #[test]
    fn tone_marks_on_other_base_vowels() {
        assert_eq!(compose("es"), "é");
        assert_eq!(compose("is"), "í");
        assert_eq!(compose("os"), "ó");
        assert_eq!(compose("us"), "ú");
        assert_eq!(compose("ys"), "ý");
    }

    // --- Vowel marks ------------------------------------------------------

    #[test]
    fn circumflex_via_doubling() {
        assert_eq!(compose("aa"), "â");
        assert_eq!(compose("ee"), "ê");
        assert_eq!(compose("oo"), "ô");
    }

    #[test]
    fn horn_and_breve_via_w() {
        assert_eq!(compose("aw"), "ă");
        assert_eq!(compose("ow"), "ơ");
        assert_eq!(compose("uw"), "ư");
    }

    #[test]
    fn uo_cluster_with_w_gets_double_horn() {
        // "uow" → "ươ": both vowels receive a horn.
        assert_eq!(compose("uow"), "ươ");
    }

    // --- Consonant mark ---------------------------------------------------

    #[test]
    fn dd_becomes_d_stroke() {
        assert_eq!(compose("dd"), "đ");
        assert_eq!(compose("dda"), "đa");
    }

    // --- Tone placement on clusters --------------------------------------

    #[test]
    fn tone_on_marked_vowel_takes_priority() {
        // "ee" → ê, then "s" places the tone on ê.
        assert_eq!(compose("ees"), "ế");
        // "uow" → ươ, then "j" places nặng on ơ.
        assert_eq!(compose("uowj"), "ượ");
    }

    #[test]
    fn tone_on_two_vowel_cluster_no_coda_second_vowel() {
        // "oa" with sắc and no coda → tone on second vowel: "oá".
        assert_eq!(compose("oas"), "oá");
    }

    #[test]
    fn tone_on_two_vowel_cluster_with_coda_first_vowel() {
        // Per the tone-placement module, a two-vowel cluster with a coda places
        // the tone on the FIRST vowel: "oan" + sắc → tone on 'o' → "óan".
        assert_eq!(compose("oans"), "óan");
    }

    // --- Classic full-word cases -----------------------------------------

    #[test]
    fn classic_duoc() {
        assert_eq!(compose("dduowjc"), "được");
    }

    #[test]
    fn classic_viet() {
        assert_eq!(compose("Vieejt"), "Việt");
    }

    #[test]
    fn classic_nguoi() {
        // "ngwowif"? Build "người": ng + uo + w (→ươ) + f (huyền on ơ) + i.
        assert_eq!(compose("nguowif"), "người");
    }

    #[test]
    fn classic_tieng_viet() {
        assert_eq!(compose("tieesng"), "tiếng");
    }

    // --- Uppercase variants ----------------------------------------------

    #[test]
    fn uppercase_tone_and_marks() {
        assert_eq!(compose("AS"), "Á");
        assert_eq!(compose("AA"), "Â");
        assert_eq!(compose("OW"), "Ơ");
        assert_eq!(compose("DD"), "Đ");
    }

    #[test]
    fn mixed_case_word() {
        // "DDoongf" → "Đồng": Đ (dd), ô (oo), tone huyền on the marked ô.
        assert_eq!(compose("DDoongf"), "Đồng");
    }

    // --- Invalid sequences → raw output ----------------------------------

    #[test]
    fn invalid_sequences_stay_raw() {
        // 'w' with no preceding a/o/u → raw, and a leading-consonant 'q' stays.
        assert_eq!(compose("qw"), "qw");
        assert_eq!(raw("qw"), "qw");
        // 'z' then tone-key 's' with no vowel → both kept literally.
        assert_eq!(compose("zs"), "zs");
        assert_eq!(raw("zs"), "zs");
    }

    #[test]
    fn tone_key_without_vowel_is_literal() {
        // A leading tone-key letter with no vowel is kept as a consonant.
        assert_eq!(compose("s"), "s");
        // 'x' before any vowel is literal; the following 'y' is just a vowel.
        assert_eq!(compose("xy"), "xy");
        // But a tone key after a vowel applies: "yx" → ngã on y → "ỹ".
        assert_eq!(compose("yx"), "ỹ");
    }

    #[test]
    fn flush_raw_preserves_full_keystroke_sequence() {
        // Even when composition succeeds, the raw keystrokes are retained.
        assert_eq!(raw("dduowjc"), "dduowjc");
        assert_eq!(raw("Vieejt"), "Vieejt");
    }

    // --- Backspace reversal (LIFO via buffer history) --------------------

    #[test]
    fn backspace_reverses_transformations_in_lifo_order() {
        let engine = TelexEngine::new();
        let mut buffer = CompositionBuffer::new("s".to_string());
        for ch in "uowj".chars() {
            engine.process_key(&mut buffer, ch);
        }
        assert_eq!(buffer.current_text(), "ượ");

        // Undo tone (j): "ượ" → "ươ".
        buffer.pop_step();
        assert_eq!(buffer.current_text(), "ươ");
        // Undo horn (w): "ươ" → "uo".
        buffer.pop_step();
        assert_eq!(buffer.current_text(), "uo");
        // Undo base 'o': "uo" → "u".
        buffer.pop_step();
        assert_eq!(buffer.current_text(), "u");
        // Undo base 'u': empty.
        buffer.pop_step();
        assert!(buffer.is_empty());
    }

    // --- Engine metadata --------------------------------------------------

    #[test]
    fn engine_name_and_composable_start() {
        let engine = TelexEngine::new();
        assert_eq!(engine.name(), "Telex");
        assert!(engine.is_composable_start('a'));
        assert!(engine.is_composable_start('d'));
        assert!(!engine.is_composable_start('1'));
        assert!(!engine.is_composable_start(' '));
    }

    #[test]
    fn terminator_returns_commit_and_pass() {
        let engine = TelexEngine::new();
        let mut buffer = CompositionBuffer::new("test".to_string());
        engine.process_key(&mut buffer, 'a');
        engine.process_key(&mut buffer, 's');
        let result = engine.process_key(&mut buffer, ' ');
        assert_eq!(result, TransformResult::CommitAndPass(' '));
        // The terminator did not alter the composed text.
        assert_eq!(buffer.current_text(), "á");
    }

    // --- Additional uppercase vowel-mark + tone variants -----------------

    #[test]
    fn uppercase_vowel_marks_and_tones() {
        // Uppercase circumflex/horn/breve marks not covered above.
        assert_eq!(compose("EE"), "Ê");
        assert_eq!(compose("OO"), "Ô");
        assert_eq!(compose("UW"), "Ư");
        assert_eq!(compose("AW"), "Ă");
        // Uppercase tones on a single vowel beyond "AS".
        assert_eq!(compose("AF"), "À");
        assert_eq!(compose("AR"), "Ả");
        assert_eq!(compose("AX"), "Ã");
        assert_eq!(compose("AJ"), "Ạ");
    }

    // --- Property 1: Telex Composition Correctness -----------------------
    // Feature: vietnamese-input-support, Property 1: Telex Composition Correctness

    /// Deterministic linear-congruential PRNG (no external dependency).
    ///
    /// Uses the classic Numerical Recipes constants. `next` advances the state
    /// and returns a bounded index, giving reproducible "random" selections.
    struct Lcg {
        state: u64,
    }

    impl Lcg {
        fn new(seed: u64) -> Self {
            Self { state: seed }
        }

        /// Advance the state and return a value in `[0, bound)`.
        fn next(&mut self, bound: usize) -> usize {
            self.state = self
                .state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            // Use the high bits, which have the best statistical quality.
            ((self.state >> 33) as usize) % bound
        }
    }

    /// A "valid Telex syllable" building block: the keystroke prefix that
    /// produces a (possibly marked) base vowel, paired with that vowel's six
    /// tone variants in the canonical order
    /// `[none, huyền, sắc, hỏi, ngã, nặng]`.
    ///
    /// This is an INDEPENDENT reference lookup table written directly from the
    /// standard Telex specification — it is not derived from the engine's own
    /// `COMPOSE_TABLE`, so the property test cross-checks the engine against a
    /// separately authored source of truth.
    const TELEX_REFERENCE: &[(&str, [char; 6])] = &[
        // Plain base vowels.
        ("a", ['a', 'à', 'á', 'ả', 'ã', 'ạ']),
        ("e", ['e', 'è', 'é', 'ẻ', 'ẽ', 'ẹ']),
        ("i", ['i', 'ì', 'í', 'ỉ', 'ĩ', 'ị']),
        ("o", ['o', 'ò', 'ó', 'ỏ', 'õ', 'ọ']),
        ("u", ['u', 'ù', 'ú', 'ủ', 'ũ', 'ụ']),
        ("y", ['y', 'ỳ', 'ý', 'ỷ', 'ỹ', 'ỵ']),
        // Circumflex via vowel doubling: aa→â, ee→ê, oo→ô.
        ("aa", ['â', 'ầ', 'ấ', 'ẩ', 'ẫ', 'ậ']),
        ("ee", ['ê', 'ề', 'ế', 'ể', 'ễ', 'ệ']),
        ("oo", ['ô', 'ồ', 'ố', 'ổ', 'ỗ', 'ộ']),
        // Breve / horn via w: aw→ă, ow→ơ, uw→ư.
        ("aw", ['ă', 'ằ', 'ắ', 'ẳ', 'ẵ', 'ặ']),
        ("ow", ['ơ', 'ờ', 'ớ', 'ở', 'ỡ', 'ợ']),
        ("uw", ['ư', 'ừ', 'ứ', 'ử', 'ữ', 'ự']),
    ];

    /// Tone-trigger keys paired with their column index in `TELEX_REFERENCE`.
    /// `None` represents the neutral (no-tone) case.
    const TELEX_TONE_KEYS: &[(Option<char>, usize)] = &[
        (None, 0),
        (Some('f'), 1),
        (Some('s'), 2),
        (Some('r'), 3),
        (Some('x'), 4),
        (Some('j'), 5),
    ];

    /// Uppercase a single composed glyph, mirroring how the engine preserves
    /// case from an uppercase keystroke.
    fn to_upper_char(c: char) -> char {
        c.to_uppercase().next().unwrap_or(c)
    }

    #[test]
    fn property_telex_composition_correctness() {
        // For any valid Telex input sequence (base vowel, optional vowel-mark
        // modifier, optional tone key), the engine's composed output must equal
        // the glyph the standard Telex specification prescribes.
        let mut rng = Lcg::new(0x5193_7A21_C0FF_EE42);
        let cases = 500; // Well above the >=100-case minimum.

        for _ in 0..cases {
            let (prefix, variants) = TELEX_REFERENCE[rng.next(TELEX_REFERENCE.len())];
            let (tone_key, tone_idx) = TELEX_TONE_KEYS[rng.next(TELEX_TONE_KEYS.len())];
            let uppercase = rng.next(2) == 1;

            // Build the keystroke sequence: vowel-mark prefix + optional tone.
            let mut input: String = prefix.to_string();
            if let Some(tk) = tone_key {
                input.push(tk);
            }

            // Expected glyph from the independent reference table.
            let mut expected = variants[tone_idx].to_string();

            if uppercase {
                input = input.to_uppercase();
                expected = expected.chars().map(to_upper_char).collect();
            }

            let actual = compose(&input);
            assert_eq!(
                actual, expected,
                "Telex sequence {:?} should compose to {:?} but produced {:?}",
                input, expected, actual
            );
        }
    }
}
