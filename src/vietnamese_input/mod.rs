//! Vietnamese Input Composer module.
//!
//! This module implements a client-side Vietnamese input composer that
//! intercepts keystrokes, applies Telex/VNI/VNI Windows transformation rules,
//! and sends composed Unicode text to the remote session.
//!
//! This file defines the core interfaces and shared data types used across the
//! composer, buffer, and input-method engine implementations.

pub mod buffer;
pub mod composer;
pub mod config;
pub mod integration;
pub mod syllable;
pub mod telex;
pub mod vni;
pub mod vni_windows;
pub mod unicode_norm;
pub mod tone;
pub mod config_parser;

#[cfg(test)]
mod integration_tests;

pub use buffer::{CompositionBuffer, CompositionState};
pub use composer::VietnameseComposer;
pub use composer::DEFAULT_TIMEOUT;
pub use config::{KeyShortcut, VietnameseInputConfig};
pub use syllable::{Syllable, VowelCluster, VowelUnit};
pub use telex::TelexEngine;
pub use vni::VniEngine;
pub use vni_windows::VniWindowsEngine;
pub use unicode_norm::{NormalizationForm, UnicodeNormalizer};
pub use tone::TonePlacement;
pub use config_parser::{
    ConfigFormat, ConfigMetadata, ConfigParser, InputMethodConfig, ParseError, RuleContext,
    TransformationRule, ValidationError,
};

use hbb_common::message_proto::KeyEvent;

/// Unique session identifier (matches RustDesk's session identifier).
pub type VietSessionId = String;

/// The set of Vietnamese input methods supported by the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMethod {
    /// Telex input method (diacritics typed using letter combinations).
    Telex,
    /// VNI input method (diacritics typed using number keys).
    Vni,
    /// VNI Windows variant input method.
    VniWindows,
    /// Composition disabled; all keystrokes pass through unchanged.
    Off,
}

/// The result produced by the composer after processing a single keystroke.
///
/// Note: this type intentionally does not derive `Eq` because `KeyEvent` (a
/// protobuf-generated type) implements `PartialEq` but not `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub enum ComposerResult {
    /// Keystroke consumed by composition (buffered, nothing sent yet).
    Consumed,
    /// Composition complete; send this Unicode text to the remote session.
    Compose(String),
    /// Not a Vietnamese sequence; pass the original key event through unchanged.
    PassThrough(KeyEvent),
    /// Flush the buffer as raw text (invalid sequence or timeout).
    Flush(String),
}

/// The result produced by an input-method engine after transforming a keystroke
/// against the current composition buffer state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransformResult {
    /// Character was transformed and the buffer was updated.
    Transformed,
    /// Character was a composition trigger (a tone or vowel mark was applied).
    ToneApplied,
    /// Character ends composition (space, punctuation, etc.); commit then pass
    /// the trigger character through.
    CommitAndPass(char),
    /// Character does not match any rule; flush the buffer as raw text.
    InvalidSequence,
    /// Still accumulating input; more keystrokes are needed.
    Buffering,
}

/// Shared trait implemented by all Vietnamese input-method engines
/// (Telex, VNI, VNI Windows).
pub trait InputMethodEngine {
    /// Process a single keystroke against the current buffer state.
    fn process_key(&self, buffer: &mut CompositionBuffer, key: char) -> TransformResult;

    /// Return the human-readable display name of this engine.
    fn name(&self) -> &str;

    /// Validate whether a character could start a composition sequence.
    fn is_composable_start(&self, c: char) -> bool;
}

/// Vietnamese tone marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneMark {
    /// Sắc — acute accent (á).
    Sac,
    /// Huyền — grave accent (à).
    Huyen,
    /// Hỏi — hook above (ả).
    Hoi,
    /// Ngã — tilde (ã).
    Nga,
    /// Nặng — dot below (ạ).
    Nang,
}

/// Vowel modification marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VowelMark {
    /// Circumflex (â, ê, ô).
    Circumflex,
    /// Breve (ă).
    Breve,
    /// Horn (ơ, ư).
    Horn,
}

/// A single transformation step recorded in the composition history, used for
/// last-applied-first-removed (LIFO) backspace reversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransformStep {
    /// A base character was inserted.
    BaseCharacter(char),
    /// A vowel mark was applied, transforming `base` into `result`.
    VowelMark { base: char, result: char },
    /// A tone mark was applied to a target vowel.
    ToneMark { tone: ToneMark, target_vowel: char },
    /// A consonant mark was applied, transforming `base` into `result`.
    ConsonantMark { base: char, result: char },
}
