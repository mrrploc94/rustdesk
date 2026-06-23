//! Keyboard-pipeline integration for the Vietnamese input composer.
//!
//! This module is the bridge between RustDesk's existing keyboard handling
//! (`src/keyboard.rs`) and the [`VietnameseComposer`]. It owns a process-wide
//! composer instance, exposes a feature gate, and provides
//! [`compose_key_events`] — the single interception point that `keyboard.rs`
//! routes produced [`KeyEvent`]s through before they are sent to the remote
//! session.
//!
//! ## Why a separate module
//!
//! `keyboard.rs` is large and central to RustDesk's input pipeline. To keep the
//! integration non-invasive and easy to reason about, all composer wiring lives
//! here and `keyboard.rs` only adds a tiny, gated call. When the feature gate is
//! off (the default), [`compose_key_events`] is never invoked, so existing
//! keyboard behavior is completely unchanged.
//!
//! ## Interception model
//!
//! RustDesk turns a platform key event into a `Vec<KeyEvent>` (see
//! `keyboard::event_to_key_events`) which is then sent one-by-one. We hook in
//! *after* that vector is produced and *before* it is sent, transforming it:
//!
//! - A printable typed character (a single-`char` `seq` on a key-down event) is
//!   fed to the composer via [`VietnameseComposer::process_key`], and the
//!   resulting [`ComposerResult`] decides the outcome:
//!   - [`ComposerResult::Consumed`] → the event is **dropped** (the keystroke
//!     was buffered into an in-flight composition; nothing is sent yet).
//!   - [`ComposerResult::Compose`] → the event is **replaced** with a key event
//!     carrying the composed Unicode text (sent to the remote as literal text,
//!     the same mechanism RustDesk already uses for unicode in
//!     `try_fill_unicode`).
//!   - [`ComposerResult::Flush`] → the event is **replaced** with a key event
//!     carrying the raw buffered text (an invalid sequence flushed verbatim).
//!   - [`ComposerResult::PassThrough`] → the **original** event is kept
//!     unchanged (preserving modifiers, mode, and platform codes).
//! - Any event that is not a printable typed character (modifiers, function
//!   keys, map-mode keycodes, key-up events, multi-char sequences) is kept
//!   unchanged.
//!
//! ## Deferred wiring
//!
//! - **Enabling the feature** (`set_enabled`) and per-method configuration are
//!   wired from persisted config in task 14.1; the gate defaults to **off**.
//! - **Per-session routing**: the composer is already keyed by
//!   [`SessionID`](super::SessionID), but resolving the *current* session id at
//!   the call site (and the FFI to switch methods / read composition state) is
//!   added in task 12.2. Until then a single default session id is used.
//! - **Keyboard shortcuts** (toggle / cycle method) and intelligent backspace
//!   routing through the composer are added in task 12.3.
//! - **Timeout / application-switch flushing** is added in task 13.1.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use hbb_common::log;
use hbb_common::message_proto::{key_event, ControlKey, KeyEvent};

use super::{
    ComposerResult, InputMethod, NormalizationForm, SessionID, VietnameseComposer, DEFAULT_TIMEOUT,
};

lazy_static::lazy_static! {
    /// Process-wide Vietnamese input composer.
    ///
    /// A single composer holds the per-session [`CompositionBuffer`]s
    /// internally, so one global instance is sufficient and matches the
    /// existing `keyboard.rs` style of process-wide `lazy_static` state (e.g.
    /// `TO_RELEASE`, `MODIFIERS_STATE`).
    static ref GLOBAL_COMPOSER: Mutex<VietnameseComposer> = Mutex::new(VietnameseComposer::new());

    /// Process-wide keyboard-shortcut configuration.
    ///
    /// Holds the configurable toggle and cycle chords (defaults Ctrl+Shift+V and
    /// Ctrl+Shift+I per requirements 9.1 / 9.2). Stored alongside the composer in
    /// a `lazy_static` `Mutex`, matching the surrounding module style. The
    /// settings UI / config layer can rebind these via [`set_toggle_shortcut`]
    /// and [`set_cycle_shortcut`] (task 14.1).
    static ref SHORTCUTS: Mutex<ShortcutConfig> = Mutex::new(ShortcutConfig::default());
}

/// Whether Vietnamese input composition is active.
///
/// Defaults to `false` so the keyboard pipeline behaves exactly as before until
/// the feature is explicitly enabled (from persisted configuration in task
/// 14.1, or the FFI bridge in task 12.2).
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Placeholder session id used until real per-session routing is wired up in
/// task 12.2. Composition state is still fully isolated per id inside the
/// composer; this constant simply gives the gated call site a stable key.
const DEFAULT_SESSION_ID: &str = "__vn_default_session__";

/// Return `true` if Vietnamese input composition is currently enabled.
///
/// This is the feature gate guarding the keyboard-pipeline hook. While it
/// returns `false` (the default), [`compose_key_events`] is never called and
/// the keyboard pipeline is unchanged.
#[inline]
pub fn is_vietnamese_input_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Enable or disable Vietnamese input composition.
///
/// Wired to persisted configuration and the settings UI in task 14.1. When
/// turning the feature off, in-flight composition buffers are cleared so no
/// stale state is retained.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    if !enabled {
        lock_composer().clear_session(&DEFAULT_SESSION_ID.to_string());
    }
}

/// Select the active input method on the global composer.
///
/// Exposed for the FFI bridge (task 12.2) and settings UI (task 14.1).
pub fn set_method(method: InputMethod) {
    lock_composer().set_method(method);
}

/// Set the Unicode normalization form applied to composed output on the global
/// composer.
///
/// Exposed for the configuration layer (task 14.1) so a persisted
/// [`NormalizationForm`] can be activated on startup, and for the settings UI
/// so an NFC/NFD change applies immediately (requirements 5.2, 8.4).
pub fn set_normalization(form: NormalizationForm) {
    lock_composer().set_normalization(form);
}

/// Parse a method identifier coming from the Flutter/UI layer into an
/// [`InputMethod`].
///
/// Accepts the canonical lowercase identifiers used across the FFI bridge and
/// settings UI: `"telex"`, `"vni"`, `"vni_windows"`, and `"off"`. Matching is
/// case-insensitive and tolerant of surrounding whitespace, and a couple of
/// common spellings of the Windows variant (`"vniwindows"`, `"vni-windows"`)
/// are also accepted. Returns `None` for any unrecognized value so the caller
/// can decide how to react (the FFI bridge leaves the active method unchanged).
pub fn parse_method(method: &str) -> Option<InputMethod> {
    match method.trim().to_ascii_lowercase().as_str() {
        "telex" => Some(InputMethod::Telex),
        "vni" => Some(InputMethod::Vni),
        "vni_windows" | "vniwindows" | "vni-windows" => Some(InputMethod::VniWindows),
        "off" => Some(InputMethod::Off),
        _ => None,
    }
}

/// Select the active input method from a string identifier.
///
/// Convenience wrapper over [`parse_method`] + [`set_method`] for the FFI
/// bridge, which receives the method as a plain `String` from Flutter. Returns
/// `true` if the identifier was recognized and applied, or `false` if it was
/// unknown (in which case the active method is left unchanged).
pub fn set_method_str(method: &str) -> bool {
    match parse_method(method) {
        Some(m) => {
            set_method(m);
            true
        }
        None => false,
    }
}

/// Return the current (in-flight) composed text for the default session, if any.
///
/// Drives the composition overlay; the FFI accessor that exposes this to
/// Flutter is added in task 12.2.
pub fn composition_preview() -> Option<String> {
    lock_composer()
        .get_buffer_content(&DEFAULT_SESSION_ID.to_string())
        .map(|s| s.to_string())
}

/// Dump the live composition-buffer state for **all** active sessions as a
/// human-readable diagnostic string (requirement 17.2).
///
/// This is the diagnostic command surfaced to users troubleshooting Vietnamese
/// input: it reports the active input method and, per session, the current
/// composed text, raw keystroke sequence, history depth, and capacity. It reads
/// composer state only and never mutates it, so it is safe to call at any time
/// (e.g. from an FFI debug hook or a logging command). The output is emitted on
/// the dedicated [`LOG_TARGET`](super::composer::LOG_TARGET) channel and also
/// returned to the caller.
pub fn diagnostic_dump() -> String {
    let dump = lock_composer().dump_buffer_state();
    log::info!(target: super::composer::LOG_TARGET, "{}", dump);
    dump
}

// ---------------------------------------------------------------------------
// Timeout / application-switch flushing (task 13.1)
// ---------------------------------------------------------------------------
//
// An in-flight Vietnamese composition is only ever sent to the remote session
// on an explicit commit (space/punctuation), on a buffer overflow, or on one of
// the recovery paths below. Two situations must not leave a partial composition
// dangling:
//
// - **Idle timeout** (requirements 2.6, 12.1): an incomplete VNI sequence or an
//   invalid Telex sequence that receives no further keystrokes within
//   [`DEFAULT_TIMEOUT`] (500ms) is flushed to the remote as the user's literal
//   raw keystrokes.
// - **Application switch / focus loss** (requirement 12.3): when the user
//   switches away from RustDesk, every session's partial composition is flushed
//   as raw text so nothing is silently lost.
//
// ## Why the flush API is exposed rather than self-driven
//
// The composer holds no timer and does not observe OS focus changes: those are
// environment-dependent and belong to RustDesk's event loop / windowing layer,
// not the composition core. This module therefore *exposes* the flush entry
// points so they can be wired to whatever timer or focus-change hook the host
// platform provides:
//
// - A periodic tick (or a per-session idle timer) calls [`flush_all_timed_out`]
//   (or [`flush_timed_out`] for one session) roughly every ~100-250ms.
// - A window focus-lost / app-switch handler calls [`flush_on_app_switch`].
//
// Each function returns the raw text that must be sent to the corresponding
// remote session. Actually delivering that text reuses the same mechanism as
// [`compose_key_events`] (a literal-text [`KeyEvent`] via [`make_text_event`]);
// the helper [`flushed_to_key_events`] builds those events for callers that
// already have a send path. Hooking the timer/focus events into RustDesk's
// platform layer is intentionally left to that layer and is out of scope here.

/// Flush the given session's in-flight composition as raw text **if** it has
/// been idle for at least [`DEFAULT_TIMEOUT`] (500ms).
///
/// Returns the raw keystroke text to send to the remote session, or `None` when
/// there is nothing to flush (no buffer, empty buffer, or not yet timed out).
/// Intended to be driven by an idle timer in RustDesk's event loop.
///
/// _Requirements: 2.6, 12.1, 12.4_
pub fn flush_timed_out(session_id: &SessionID) -> Option<String> {
    lock_composer().flush_if_timed_out(session_id, DEFAULT_TIMEOUT)
}

/// Flush the given session's composition as raw text if it has been idle for at
/// least `timeout`. Like [`flush_timed_out`] but with a caller-chosen window
/// (useful for tests or a configurable `timeout_ms`).
///
/// _Requirements: 2.6, 12.1, 12.4_
pub fn flush_timed_out_with(session_id: &SessionID, timeout: std::time::Duration) -> Option<String> {
    lock_composer().flush_if_timed_out(session_id, timeout)
}

/// Sweep every live session and flush those whose composition has been idle for
/// at least [`DEFAULT_TIMEOUT`], returning each `(session_id, raw_text)` pair.
///
/// Intended to be driven by a single periodic timer rather than one timer per
/// session. The caller sends each returned raw text to its remote session.
///
/// _Requirements: 2.6, 12.1, 12.4_
pub fn flush_all_timed_out() -> Vec<(SessionID, String)> {
    lock_composer().flush_timed_out_sessions(DEFAULT_TIMEOUT)
}

/// Flush **all** sessions' in-flight compositions as raw text immediately,
/// regardless of idle time. Call this on application switch / focus loss so a
/// partial composition is never silently dropped (requirement 12.3).
///
/// Returns each flushed `(session_id, raw_text)` pair for the caller to send.
///
/// _Requirements: 12.3, 12.4_
pub fn flush_on_app_switch() -> Vec<(SessionID, String)> {
    lock_composer().flush_all_sessions()
}

// ---------------------------------------------------------------------------
// Memory monitoring (task 13.2)
// ---------------------------------------------------------------------------
//
// The composer must keep its composition state across all sessions under a 10MB
// budget (requirement 13.6) and, when that budget is exceeded, flush completed
// compositions to the remote and log an overflow warning (requirement 13.7).
//
// As with the timeout/app-switch flush paths, the composer cannot itself send
// the reclaimed text to the remote — only the host integration can — so the
// enforcement entry point is exposed here. A periodic tick (the same one that
// drives `flush_all_timed_out`) or a post-keystroke check can call
// `enforce_memory_limit` and deliver any returned raw text via
// `flushed_to_key_events`.

/// Return the composer's estimated memory footprint, in bytes, across all
/// sessions. Backs diagnostics and the 10MB budget check (requirement 13.6).
pub fn composer_memory_bytes() -> usize {
    lock_composer().estimated_memory_bytes()
}

/// Return `true` when the composer's estimated memory exceeds the 10MB
/// cross-session budget (requirement 13.6).
pub fn is_composer_over_memory_limit() -> bool {
    lock_composer().is_over_memory_limit()
}

/// Enforce the 10MB cross-session memory budget (requirement 13.7).
///
/// When the composer is over budget, its in-flight compositions are flushed to
/// the remote as raw text and a memory-overflow warning is logged. Returns each
/// flushed `(session_id, raw_text)` pair for the caller to deliver (e.g. via
/// [`flushed_to_key_events`]); an empty vector means usage was within budget.
///
/// _Requirements: 13.6, 13.7_
pub fn enforce_memory_limit() -> Vec<(SessionID, String)> {
    lock_composer().enforce_memory_limit()
}

/// Convert a flushed raw-text string into the key events that deliver it to the
/// remote session, reusing the same literal-text mechanism as composition.
///
/// A convenience for host code that already has a `Vec<KeyEvent>` send path:
/// given the raw text returned by one of the flush functions, it produces a
/// single key-down text event (empty input yields no events).
pub fn flushed_to_key_events(text: &str) -> Vec<KeyEvent> {
    if text.is_empty() {
        Vec::new()
    } else {
        let mut event = KeyEvent::new();
        event.set_seq(text.to_string());
        event.down = true;
        vec![event]
    }
}

/// Transform a vector of key events produced by `event_to_key_events` by
/// routing printable typed characters through the Vietnamese composer.
///
/// See the module-level documentation for the full interception model. This is
/// the single function the `keyboard.rs` hook calls, and only when
/// [`is_vietnamese_input_enabled`] is `true`.
///
/// `session_id` selects which composition buffer to use. Until task 12.2 wires
/// the real current-session id, callers may pass [`default_session_id`].
///
/// _Requirements: 1.8, 2.5, 6.2, 13.1, 13.2_
pub fn compose_key_events(session_id: &SessionID, events: Vec<KeyEvent>) -> Vec<KeyEvent> {
    // Snapshot the configurable shortcuts (cheap `Copy`) before locking the
    // composer, so the two global locks are never held simultaneously.
    let shortcuts = *lock_shortcuts();
    let mut composer = lock_composer();
    compose_key_events_with_config(&mut composer, &shortcuts, session_id, events)
}

/// Core of [`compose_key_events`] operating on an explicit composer with the
/// default shortcut set. Retained as a convenience for the unit tests that
/// predate keyboard shortcuts.
///
/// Kept separate from the global-lock wrapper so the interception logic can be
/// unit-tested against a local [`VietnameseComposer`] without touching
/// process-wide state.
fn compose_key_events_with(
    composer: &mut VietnameseComposer,
    session_id: &SessionID,
    events: Vec<KeyEvent>,
) -> Vec<KeyEvent> {
    compose_key_events_with_config(composer, &ShortcutConfig::default(), session_id, events)
}

/// Interception loop operating on an explicit composer *and* shortcut config.
///
/// Threading the [`ShortcutConfig`] explicitly (rather than reading the global)
/// keeps the logic pure and unit-testable with deterministic, isolated config.
fn compose_key_events_with_config(
    composer: &mut VietnameseComposer,
    shortcuts: &ShortcutConfig,
    session_id: &SessionID,
    events: Vec<KeyEvent>,
) -> Vec<KeyEvent> {
    let mut out = Vec::with_capacity(events.len());
    for event in events {
        // Shortcuts are detected and handled *first*, before composition. They
        // are Ctrl/Shift chords (e.g. Ctrl+Shift+V) that must never reach the
        // composer — which only deals with plain typed characters — nor the
        // remote session. A matched chord is suppressed (dropped) for both its
        // key-down and key-up so no half-shortcut leaks downstream; the action
        // itself fires once, on key-down.
        if let Some(action) = match_shortcut_with(shortcuts, &event) {
            if event.down {
                apply_shortcut(composer, action);
            }
            continue;
        }
        match dispatch_key_event(composer, session_id, &event) {
            // Buffered into an in-flight composition: drop the event.
            DispatchOutcome::Suppress => {}
            // Composition produced text (composed or raw flush): send that text
            // instead of the original keystroke.
            DispatchOutcome::Replace(text) => out.push(make_text_event(&event, &text)),
            // Not part of a Vietnamese composition: forward the original event.
            DispatchOutcome::Keep => out.push(event),
        }
    }
    out
}

/// The default session id used by the gated call site until per-session routing
/// is wired in task 12.2.
#[inline]
pub fn default_session_id() -> SessionID {
    DEFAULT_SESSION_ID.to_string()
}

/// What [`compose_key_events`] should do with a single source key event.
enum DispatchOutcome {
    /// Drop the event (keystroke consumed by an in-flight composition).
    Suppress,
    /// Replace the event with one carrying this Unicode text.
    Replace(String),
    /// Keep the original event unchanged.
    Keep,
}

/// Route a single key event through the composer and decide its fate.
///
/// Only printable typed characters (a one-`char` `seq` on a key-down event) are
/// composed; every other event is kept unchanged. This conservative filter
/// ensures modifiers, function keys, map-mode keycodes, and key-up events are
/// never disturbed by the composer.
fn dispatch_key_event(
    composer: &mut VietnameseComposer,
    session_id: &SessionID,
    event: &KeyEvent,
) -> DispatchOutcome {
    let typed = match typed_char(event) {
        Some(c) => c,
        None => return DispatchOutcome::Keep,
    };

    match composer.process_key(session_id, typed) {
        ComposerResult::Consumed => DispatchOutcome::Suppress,
        ComposerResult::Compose(text) | ComposerResult::Flush(text) => {
            DispatchOutcome::Replace(text)
        }
        // The composer reports a pass-through; keep the richer original event
        // (it preserves modifiers / platform codes the composer cannot).
        ComposerResult::PassThrough(_) => DispatchOutcome::Keep,
    }
}

/// Extract a single printable typed character from a key event, if it carries
/// one. Returns `None` for key-up events, non-text events, and multi-character
/// sequences — none of which the composer should consume.
fn typed_char(event: &KeyEvent) -> Option<char> {
    if !event.down {
        return None;
    }
    let seq = event.seq();
    let mut chars = seq.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// Build a key event that carries `text` as a literal Unicode sequence,
/// reusing the source event's mode/metadata. Mirrors the existing unicode-text
/// mechanism in `keyboard::try_fill_unicode` (`set_seq` + `down = true`).
fn make_text_event(source: &KeyEvent, text: &str) -> KeyEvent {
    let mut event = source.clone();
    event.set_seq(text.to_string());
    event.down = true;
    event
}

/// Lock the global composer, recovering from a poisoned lock so a prior panic
/// in another thread does not wedge Vietnamese input. The composer's methods do
/// not panic, so poisoning is not expected in practice.
fn lock_composer() -> std::sync::MutexGuard<'static, VietnameseComposer> {
    match GLOBAL_COMPOSER.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

// ---------------------------------------------------------------------------
// Keyboard shortcuts (task 12.3)
// ---------------------------------------------------------------------------
//
// Two shortcuts control the composer without leaving the remote session:
//
// - **Toggle** (default **Ctrl+Shift+V**, requirement 9.1) turns Vietnamese
//   input on/off via [`VietnameseComposer::toggle_method`].
// - **Cycle** (default **Ctrl+Shift+I**, requirement 9.2) advances through the
//   input methods via [`VietnameseComposer::cycle_method`].
//
// ## Choice of the cycle shortcut
//
// Requirement 4.7 mentions Ctrl+Shift+M as a cycle shortcut, while requirement
// 9.2 specifies Ctrl+Shift+I as the default. These conflict. We follow **9.2**
// (Ctrl+Shift+I) because requirement 9 is the dedicated, more specific
// "Keyboard Shortcut Handling" requirement and 9.4 makes the binding
// configurable anyway (see [`set_cycle_shortcut`]). A user who prefers
// Ctrl+Shift+M can rebind it through configuration.
//
// ## Why detection lives here
//
// Shortcut chords carry Ctrl/Shift modifiers, whereas composition only ever
// consumes *plain* typed characters. Detecting shortcuts at the single
// interception point ([`compose_key_events`]) — and doing so *before* the
// composer sees the event — keeps the two concerns from interfering: a chord is
// never mistaken for a composable keystroke, and a composable keystroke never
// trips a shortcut.

/// An action triggered by a recognized Vietnamese-input keyboard shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutAction {
    /// Toggle Vietnamese input on/off (default Ctrl+Shift+V — requirement 9.1).
    Toggle,
    /// Cycle through the input methods (default Ctrl+Shift+I — requirement 9.2).
    Cycle,
}

/// A keyboard chord: a set of required modifiers plus a single trigger key.
///
/// The trigger [`key`](Chord::key) is normalized to lowercase ASCII so matching
/// is case-insensitive. Left/right modifier variants are treated equivalently
/// when matching (e.g. either Control or Right-Control satisfies `ctrl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    /// Require the Control modifier (left or right).
    pub ctrl: bool,
    /// Require the Shift modifier (left or right).
    pub shift: bool,
    /// Require the Alt modifier (left or right / AltGr).
    pub alt: bool,
    /// Require the Meta / Super / Windows modifier (left or right).
    pub meta: bool,
    /// The trigger key, stored normalized to lowercase ASCII.
    pub key: char,
}

impl Chord {
    /// Build a chord, normalizing `key` to lowercase ASCII.
    pub fn new(ctrl: bool, shift: bool, alt: bool, meta: bool, key: char) -> Self {
        Self {
            ctrl,
            shift,
            alt,
            meta,
            key: key.to_ascii_lowercase(),
        }
    }

    /// Convenience constructor for the common `Ctrl+Shift+<key>` chord.
    pub fn ctrl_shift(key: char) -> Self {
        Self::new(true, true, false, false, key)
    }

    /// Whether this chord is satisfied by the given modifier set and trigger
    /// character. The trigger comparison is case-insensitive.
    fn matches(&self, mods: &ChordModifiers, key: char) -> bool {
        self.ctrl == mods.ctrl
            && self.shift == mods.shift
            && self.alt == mods.alt
            && self.meta == mods.meta
            && self.key == key.to_ascii_lowercase()
    }
}

/// The configurable set of Vietnamese-input shortcuts (requirement 9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutConfig {
    /// Chord that toggles Vietnamese input on/off.
    pub toggle: Chord,
    /// Chord that cycles through the input methods.
    pub cycle: Chord,
}

impl Default for ShortcutConfig {
    fn default() -> Self {
        Self {
            // Requirement 9.1: default toggle = Ctrl+Shift+V.
            toggle: Chord::ctrl_shift('v'),
            // Requirement 9.2: default cycle = Ctrl+Shift+I.
            cycle: Chord::ctrl_shift('i'),
        }
    }
}

/// The Ctrl/Shift/Alt/Meta modifiers present on a key event, with left/right
/// variants collapsed. Lock modifiers (Caps/Num) are intentionally excluded so
/// they never affect shortcut matching.
struct ChordModifiers {
    ctrl: bool,
    shift: bool,
    alt: bool,
    meta: bool,
}

/// Rebind the toggle shortcut (requirement 9.4). Exposed for the settings UI /
/// configuration layer wired in task 14.1.
pub fn set_toggle_shortcut(chord: Chord) {
    lock_shortcuts().toggle = chord;
}

/// Rebind the cycle shortcut (requirement 9.4). Exposed for the settings UI /
/// configuration layer wired in task 14.1.
pub fn set_cycle_shortcut(chord: Chord) {
    lock_shortcuts().cycle = chord;
}

/// Return the currently configured toggle chord.
pub fn toggle_shortcut() -> Chord {
    lock_shortcuts().toggle
}

/// Return the currently configured cycle chord.
pub fn cycle_shortcut() -> Chord {
    lock_shortcuts().cycle
}

/// Inspect a key event for a configured shortcut chord and, if matched, perform
/// its action on the global composer.
///
/// Returns `Some(action)` when the event is a recognized shortcut — in which
/// case the caller MUST suppress the event so it never reaches the remote
/// session (requirement 9.3's visual indicator is the Flutter side's job in
/// task 16.4; here we only perform the action and report it). Returns `None`
/// when the event is not a shortcut and should continue through the normal
/// composition path.
///
/// The action fires only on key-down; a matching key-up still returns
/// `Some(action)` so the caller suppresses it too, but performs no second
/// toggle/cycle.
///
/// _Requirements: 9.1, 9.2, 9.3, 9.4_
pub fn check_and_handle_shortcut(event: &KeyEvent) -> Option<ShortcutAction> {
    let action = match_shortcut(event)?;
    if event.down {
        let mut composer = lock_composer();
        apply_shortcut(&mut composer, action);
    }
    Some(action)
}

/// Pure matcher: map a key event to the shortcut action it represents, if any,
/// using the global shortcut configuration.
///
/// Reads only the configured chords and the event itself — it never touches the
/// composer — so it is safe to call while the composer lock is already held.
fn match_shortcut(event: &KeyEvent) -> Option<ShortcutAction> {
    let shortcuts = *lock_shortcuts();
    match_shortcut_with(&shortcuts, event)
}

/// Pure matcher against an explicit shortcut configuration.
fn match_shortcut_with(shortcuts: &ShortcutConfig, event: &KeyEvent) -> Option<ShortcutAction> {
    let key = shortcut_key_char(event)?;
    let mods = chord_modifiers(event);
    // A bare key with no Ctrl/Shift/Alt/Meta can never be a shortcut; skip the
    // chord comparison so plain typed characters fall straight through.
    if !(mods.ctrl || mods.shift || mods.alt || mods.meta) {
        return None;
    }
    if shortcuts.toggle.matches(&mods, key) {
        Some(ShortcutAction::Toggle)
    } else if shortcuts.cycle.matches(&mods, key) {
        Some(ShortcutAction::Cycle)
    } else {
        None
    }
}

/// Perform a shortcut action against a composer instance.
fn apply_shortcut(composer: &mut VietnameseComposer, action: ShortcutAction) {
    match action {
        ShortcutAction::Toggle => {
            composer.toggle_method();
        }
        ShortcutAction::Cycle => {
            composer.cycle_method();
        }
    }
}

/// Collect the Ctrl/Shift/Alt/Meta modifiers present on a key event, treating
/// left/right variants equivalently and ignoring lock modifiers (Caps/Num).
///
/// `keyboard.rs` records held modifiers in `KeyEvent::modifiers` (see
/// `legacy_modifiers` / `add_lock_modes_modifiers`), each as a [`ControlKey`].
fn chord_modifiers(event: &KeyEvent) -> ChordModifiers {
    let mut mods = ChordModifiers {
        ctrl: false,
        shift: false,
        alt: false,
        meta: false,
    };
    for modifier in &event.modifiers {
        let v = modifier.value();
        if v == ControlKey::Control.value() || v == ControlKey::RControl.value() {
            mods.ctrl = true;
        } else if v == ControlKey::Shift.value() || v == ControlKey::RShift.value() {
            mods.shift = true;
        } else if v == ControlKey::Alt.value() || v == ControlKey::RAlt.value() {
            mods.alt = true;
        } else if v == ControlKey::Meta.value() || v == ControlKey::RWin.value() {
            mods.meta = true;
        }
    }
    mods
}

/// Extract the trigger character a shortcut chord would carry, if this event
/// represents a printable letter/character key.
///
/// Two encodings are recognized, mirroring how `keyboard.rs` builds events:
/// - **Legacy mode** encodes the key as `Chr` holding the ASCII char value
///   (e.g. `'v'` = `0x76`), so a value in the printable-ASCII range maps
///   directly back to a character.
/// - **Translate mode** encodes typed text as a single-character `Seq`.
///
/// **Map mode** encodes `Chr` as a platform *scancode* (not an ASCII value);
/// those are deliberately not matched here, so character-based shortcut
/// detection is reliable in Legacy and Translate modes. This is an accepted
/// limitation — map-mode chords would require a platform scancode table, which
/// is out of scope for this task.
fn shortcut_key_char(event: &KeyEvent) -> Option<char> {
    match &event.union {
        Some(key_event::Union::Seq(seq)) => {
            let mut chars = seq.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Some(c.to_ascii_lowercase()),
                _ => None,
            }
        }
        Some(key_event::Union::Chr(code)) => {
            // The low 16 bits carry the value in the modes that use `Chr`;
            // ASCII fits comfortably within them. Cast defensively so this is
            // correct whether the proto field is signed or unsigned.
            let c = char::from_u32((*code as u32) & 0x0000_FFFF)?;
            if c.is_ascii_graphic() {
                Some(c.to_ascii_lowercase())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Lock the global shortcut configuration, recovering from poisoning the same
/// way [`lock_composer`] does.
fn lock_shortcuts() -> std::sync::MutexGuard<'static, ShortcutConfig> {
    match SHORTCUTS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::message_proto::KeyboardMode;

    fn sid() -> SessionID {
        "test-session".to_string()
    }

    /// A printable typed key-down event carrying a single character as `seq`.
    fn typed(c: char) -> KeyEvent {
        let mut ev = KeyEvent::new();
        ev.set_seq(c.to_string());
        ev.down = true;
        ev
    }

    /// A key-up variant of a single-character `seq` event.
    fn typed_up(c: char) -> KeyEvent {
        let mut ev = KeyEvent::new();
        ev.set_seq(c.to_string());
        ev.down = false;
        ev
    }

    /// A non-text key event (no `seq`), e.g. a mapped control/modifier key.
    fn non_text_down() -> KeyEvent {
        let mut ev = KeyEvent::new();
        ev.down = true;
        ev
    }

    /// Run one platform keystroke (a one-element event vector, like the real
    /// pipeline produces in translate mode) through the composer and return the
    /// resulting events.
    fn step(composer: &mut VietnameseComposer, c: char) -> Vec<KeyEvent> {
        compose_key_events_with(composer, &sid(), vec![typed(c)])
    }

    // --- typed_char extraction -------------------------------------------

    #[test]
    fn typed_char_reads_single_down_char() {
        assert_eq!(typed_char(&typed('a')), Some('a'));
    }

    #[test]
    fn typed_char_ignores_key_up() {
        assert_eq!(typed_char(&typed_up('a')), None);
    }

    #[test]
    fn typed_char_ignores_non_text_event() {
        assert_eq!(typed_char(&non_text_down()), None);
    }

    #[test]
    fn typed_char_ignores_multi_char_seq() {
        let mut ev = KeyEvent::new();
        ev.set_seq("abc".to_string());
        ev.down = true;
        assert_eq!(typed_char(&ev), None);
    }

    // --- Consumed → event suppressed -------------------------------------

    #[test]
    fn composable_keystroke_is_suppressed() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        // 'v' begins a composition: the event is buffered and dropped.
        let out = step(&mut composer, 'v');
        assert!(out.is_empty());
        assert_eq!(composer.get_buffer_content(&sid()), Some("v"));
    }

    // --- Compose → event replaced with composed text ---------------------

    #[test]
    fn commit_replaces_event_with_composed_text() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        // "as" buffers, the space commits → composed "á " sent as one event.
        assert!(step(&mut composer, 'a').is_empty());
        assert!(step(&mut composer, 's').is_empty());
        let out = compose_key_events_with(&mut composer, &sid(), vec![typed(' ')]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq(), "á ");
        assert!(out[0].down);
        // Buffer is empty after commit.
        assert!(composer.get_buffer_content(&sid()).is_none());
    }

    #[test]
    fn full_word_composition_through_pipeline() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        // Classic "dduowjc" + space → "được ".
        for c in "dduowjc".chars() {
            assert!(step(&mut composer, c).is_empty());
        }
        let out = compose_key_events_with(&mut composer, &sid(), vec![typed(' ')]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq(), "được ");
    }

    // --- PassThrough → original event kept unchanged ---------------------

    #[test]
    fn off_method_keeps_original_event() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Off);
        let out = step(&mut composer, 'a');
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq(), "a");
        // No composition buffer is created in Off mode.
        assert!(composer.get_buffer_content(&sid()).is_none());
    }

    #[test]
    fn trigger_with_empty_buffer_is_kept() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        // A space with nothing buffered passes through unchanged.
        let out = compose_key_events_with(&mut composer, &sid(), vec![typed(' ')]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq(), " ");
    }

    // --- Non-typed events are never disturbed ----------------------------

    #[test]
    fn non_text_event_is_kept_unchanged() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let out = compose_key_events_with(&mut composer, &sid(), vec![non_text_down()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq(), "");
    }

    #[test]
    fn key_up_event_is_kept_unchanged() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let out = compose_key_events_with(&mut composer, &sid(), vec![typed_up('a')]);
        assert_eq!(out.len(), 1);
        // Untouched: still a key-up 'a', not consumed by the composer.
        assert_eq!(out[0].seq(), "a");
        assert!(!out[0].down);
        assert!(composer.get_buffer_content(&sid()).is_none());
    }

    // --- Multiple events in a single vector ------------------------------

    #[test]
    fn mixed_event_vector_is_transformed_per_event() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        // A non-text event followed by a composable 'v': the first is kept, the
        // second is suppressed (buffered).
        let out = compose_key_events_with(
            &mut composer,
            &sid(),
            vec![non_text_down(), typed('v')],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq(), "");
        assert_eq!(composer.get_buffer_content(&sid()), Some("v"));
    }

    // --- Feature gate -----------------------------------------------------

    #[test]
    fn feature_gate_is_off_by_default() {
        // The keyboard hook only runs when this is true; it must default off so
        // existing behavior is unchanged until later tasks enable the feature.
        assert!(!is_vietnamese_input_enabled());
    }

    // --- parse_method string parsing -------------------------------------

    #[test]
    fn parse_method_accepts_canonical_identifiers() {
        assert_eq!(parse_method("telex"), Some(InputMethod::Telex));
        assert_eq!(parse_method("vni"), Some(InputMethod::Vni));
        assert_eq!(parse_method("vni_windows"), Some(InputMethod::VniWindows));
        assert_eq!(parse_method("off"), Some(InputMethod::Off));
    }

    #[test]
    fn parse_method_is_case_and_whitespace_insensitive() {
        assert_eq!(parse_method("  TELEX "), Some(InputMethod::Telex));
        assert_eq!(parse_method("Vni"), Some(InputMethod::Vni));
        assert_eq!(parse_method("VNI-Windows"), Some(InputMethod::VniWindows));
        assert_eq!(parse_method("vniwindows"), Some(InputMethod::VniWindows));
    }

    #[test]
    fn parse_method_rejects_unknown_identifiers() {
        assert_eq!(parse_method(""), None);
        assert_eq!(parse_method("xyz"), None);
        assert_eq!(parse_method("vn"), None);
    }

    #[test]
    fn set_method_str_reports_recognition() {
        // A recognized identifier is applied and reported as handled.
        assert!(set_method_str("vni"));
        // An unknown identifier is rejected; the active method is left as-is.
        assert!(!set_method_str("bogus"));
        // Restore the default so other tests sharing the global composer are
        // unaffected by method changes made here.
        set_method(InputMethod::Telex);
    }

    // --- make_text_event preserves source metadata -----------------------

    #[test]
    fn make_text_event_preserves_mode_and_sets_seq() {
        let mut source = KeyEvent::new();
        source.mode = KeyboardMode::Translate.into();
        let ev = make_text_event(&source, "ư");
        assert_eq!(ev.seq(), "ư");
        assert!(ev.down);
        assert_eq!(ev.mode, KeyboardMode::Translate.into());
    }

    // --- Keyboard shortcuts (task 12.3) ----------------------------------

    /// A legacy-mode chord event: an ASCII character carried in `Chr` with the
    /// given Ctrl/Shift modifiers — the shape `keyboard.rs` produces for a
    /// chord like Ctrl+Shift+V.
    fn chr_chord(c: char, ctrl: bool, shift: bool, down: bool) -> KeyEvent {
        let mut ev = KeyEvent::new();
        ev.set_chr(c as _);
        ev.down = down;
        if ctrl {
            ev.modifiers.push(ControlKey::Control.into());
        }
        if shift {
            ev.modifiers.push(ControlKey::Shift.into());
        }
        ev
    }

    /// A translate-mode chord event: a single-character `Seq` plus modifiers.
    fn seq_chord(s: &str, ctrl: bool, shift: bool) -> KeyEvent {
        let mut ev = KeyEvent::new();
        ev.set_seq(s.to_string());
        ev.down = true;
        if ctrl {
            ev.modifiers.push(ControlKey::Control.into());
        }
        if shift {
            ev.modifiers.push(ControlKey::Shift.into());
        }
        ev
    }

    fn defaults() -> ShortcutConfig {
        ShortcutConfig::default()
    }

    // --- chord detection (pure, explicit config) -------------------------

    #[test]
    fn ctrl_shift_v_matches_toggle() {
        let ev = chr_chord('v', true, true, true);
        assert_eq!(
            match_shortcut_with(&defaults(), &ev),
            Some(ShortcutAction::Toggle)
        );
    }

    #[test]
    fn ctrl_shift_i_matches_cycle() {
        let ev = chr_chord('i', true, true, true);
        assert_eq!(
            match_shortcut_with(&defaults(), &ev),
            Some(ShortcutAction::Cycle)
        );
    }

    #[test]
    fn shortcut_matching_is_case_insensitive() {
        // Uppercase ASCII (Shift held) still maps to the lowercase trigger key.
        let ev = chr_chord('V', true, true, true);
        assert_eq!(
            match_shortcut_with(&defaults(), &ev),
            Some(ShortcutAction::Toggle)
        );
    }

    #[test]
    fn shortcut_detected_via_translate_seq() {
        // A single-char Seq (translate mode) with the chord modifiers matches.
        let ev = seq_chord("v", true, true);
        assert_eq!(
            match_shortcut_with(&defaults(), &ev),
            Some(ShortcutAction::Toggle)
        );
    }

    #[test]
    fn plain_key_is_not_a_shortcut() {
        // No modifiers: a bare 'v' must never trip the toggle shortcut.
        let ev = chr_chord('v', false, false, true);
        assert_eq!(match_shortcut_with(&defaults(), &ev), None);
    }

    #[test]
    fn ctrl_only_is_not_a_shortcut() {
        // Toggle requires Ctrl *and* Shift; Ctrl alone does not match.
        let ev = chr_chord('v', true, false, true);
        assert_eq!(match_shortcut_with(&defaults(), &ev), None);
    }

    #[test]
    fn wrong_letter_with_chord_is_not_a_shortcut() {
        let ev = chr_chord('q', true, true, true);
        assert_eq!(match_shortcut_with(&defaults(), &ev), None);
    }

    #[test]
    fn modifier_only_event_is_not_a_shortcut() {
        // An event carrying only a ControlKey (no Chr/Seq trigger) is not a
        // chord — there is no trigger character to match.
        let mut ev = KeyEvent::new();
        ev.set_control_key(ControlKey::Control);
        ev.down = true;
        ev.modifiers.push(ControlKey::Shift.into());
        assert_eq!(match_shortcut_with(&defaults(), &ev), None);
    }

    #[test]
    fn right_hand_modifiers_satisfy_the_chord() {
        // Right Ctrl / right Shift are equivalent to their left variants.
        let mut ev = KeyEvent::new();
        ev.set_chr('v' as _);
        ev.down = true;
        ev.modifiers.push(ControlKey::RControl.into());
        ev.modifiers.push(ControlKey::RShift.into());
        assert_eq!(
            match_shortcut_with(&defaults(), &ev),
            Some(ShortcutAction::Toggle)
        );
    }

    #[test]
    fn lock_modifiers_do_not_break_matching() {
        // A stray CapsLock modifier (common on letter keys) must not prevent a
        // chord from matching.
        let mut ev = chr_chord('v', true, true, true);
        ev.modifiers.push(ControlKey::CapsLock.into());
        assert_eq!(
            match_shortcut_with(&defaults(), &ev),
            Some(ShortcutAction::Toggle)
        );
    }

    // --- action + suppression through the interception loop --------------

    #[test]
    fn toggle_shortcut_turns_composer_off_and_suppresses_event() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let out = compose_key_events_with(
            &mut composer,
            &sid(),
            vec![chr_chord('v', true, true, true)],
        );
        // The chord is consumed: nothing is sent to the remote session.
        assert!(out.is_empty());
        // Telex was on, so toggling turns Vietnamese input off.
        assert_eq!(composer.active_method(), InputMethod::Off);
    }

    #[test]
    fn cycle_shortcut_advances_method_and_suppresses_event() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let out = compose_key_events_with(
            &mut composer,
            &sid(),
            vec![chr_chord('i', true, true, true)],
        );
        assert!(out.is_empty());
        // Telex → VNI is the first step of the fixed cycle order.
        assert_eq!(composer.active_method(), InputMethod::Vni);
    }

    #[test]
    fn shortcut_action_fires_once_on_down_not_up() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        // Key-down performs the cycle (Telex → VNI)...
        let down = compose_key_events_with(
            &mut composer,
            &sid(),
            vec![chr_chord('i', true, true, true)],
        );
        assert!(down.is_empty());
        assert_eq!(composer.active_method(), InputMethod::Vni);
        // ...and the matching key-up is suppressed without cycling again.
        let up = compose_key_events_with(
            &mut composer,
            &sid(),
            vec![chr_chord('i', true, true, false)],
        );
        assert!(up.is_empty());
        assert_eq!(composer.active_method(), InputMethod::Vni);
    }

    #[test]
    fn shortcut_is_handled_before_composition() {
        // While a composition is in flight, a Ctrl+Shift+I cycles the method and
        // is suppressed — it is *not* fed to the composer as the letter 'i'.
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        assert!(step(&mut composer, 'a').is_empty());
        let out = compose_key_events_with(
            &mut composer,
            &sid(),
            vec![chr_chord('i', true, true, true)],
        );
        assert!(out.is_empty());
        // The composer cycled, and the in-flight buffer was left untouched: it
        // still holds just "a" (the shortcut 'i' never entered the buffer).
        assert_eq!(composer.active_method(), InputMethod::Vni);
        assert_eq!(composer.get_buffer_content(&sid()), Some("a"));
    }

    #[test]
    fn plain_letter_still_composes_with_shortcuts_present() {
        // A bare composable key is unaffected by shortcut detection.
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let out = compose_key_events_with(&mut composer, &sid(), vec![typed('v')]);
        assert!(out.is_empty());
        assert_eq!(composer.get_buffer_content(&sid()), Some("v"));
    }

    // --- configurable rebinding ------------------------------------------

    #[test]
    fn rebinding_via_config_changes_what_matches() {
        // A config with non-default chords: Ctrl+Shift+J toggles, Ctrl+Shift+K
        // cycles. The old defaults no longer match under it.
        let cfg = ShortcutConfig {
            toggle: Chord::ctrl_shift('j'),
            cycle: Chord::ctrl_shift('k'),
        };
        assert_eq!(
            match_shortcut_with(&cfg, &chr_chord('j', true, true, true)),
            Some(ShortcutAction::Toggle)
        );
        assert_eq!(
            match_shortcut_with(&cfg, &chr_chord('k', true, true, true)),
            Some(ShortcutAction::Cycle)
        );
        assert_eq!(
            match_shortcut_with(&cfg, &chr_chord('v', true, true, true)),
            None
        );
    }

    #[test]
    fn shortcut_setters_and_getters_roundtrip() {
        // Exercises the global, configurable shortcut bindings (requirement
        // 9.4) end-to-end, then restores the defaults so the shared global
        // config is unchanged for other tests.
        let original_toggle = toggle_shortcut();
        let original_cycle = cycle_shortcut();

        set_toggle_shortcut(Chord::ctrl_shift('j'));
        set_cycle_shortcut(Chord::new(true, false, true, false, 'm'));
        assert_eq!(toggle_shortcut(), Chord::ctrl_shift('j'));
        assert_eq!(cycle_shortcut(), Chord::new(true, false, true, false, 'm'));

        // Restore defaults.
        set_toggle_shortcut(original_toggle);
        set_cycle_shortcut(original_cycle);
        assert_eq!(toggle_shortcut(), Chord::ctrl_shift('v'));
        assert_eq!(cycle_shortcut(), Chord::ctrl_shift('i'));
    }

    // --- defaults & construction -----------------------------------------

    #[test]
    fn default_shortcuts_are_ctrl_shift_v_and_i() {
        let cfg = ShortcutConfig::default();
        assert_eq!(cfg.toggle, Chord::ctrl_shift('v'));
        assert_eq!(cfg.cycle, Chord::ctrl_shift('i'));
    }

    #[test]
    fn chord_normalizes_key_to_lowercase() {
        assert_eq!(Chord::ctrl_shift('V').key, 'v');
        assert_eq!(Chord::new(true, true, false, false, 'I').key, 'i');
    }

    // --- Timeout / app-switch flushing (task 13.1) -----------------------

    #[test]
    fn flushed_to_key_events_wraps_text_as_down_seq() {
        let events = flushed_to_key_events("uw");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq(), "uw");
        assert!(events[0].down);
    }

    #[test]
    fn flushed_to_key_events_empty_text_yields_no_events() {
        assert!(flushed_to_key_events("").is_empty());
    }

    #[test]
    fn app_switch_flush_emits_raw_for_local_composer() {
        // Build an in-flight composition on a local composer through the same
        // interception path the pipeline uses, then flush it as on app switch.
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        for c in "dd".chars() {
            let _ = compose_key_events_with(&mut composer, &sid(), vec![typed(c)]);
        }
        assert_eq!(composer.get_buffer_content(&sid()), Some("đ"));
        let flushed = composer.flush_all_sessions();
        assert_eq!(flushed, vec![(sid(), "dd".to_string())]);
        assert!(composer.get_buffer_content(&sid()).is_none());
    }

    #[test]
    fn timeout_flush_emits_raw_for_local_composer() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Vni);
        // Incomplete VNI sequence "u7" → "ư" in flight.
        for c in "u7".chars() {
            let _ = compose_key_events_with(&mut composer, &sid(), vec![typed(c)]);
        }
        // Zero window: already stale, flush as raw "u7".
        let flushed = composer.flush_if_timed_out(&sid(), std::time::Duration::ZERO);
        assert_eq!(flushed.as_deref(), Some("u7"));
        // A fresh, non-stale composition is retained.
        for c in "vi".chars() {
            let _ = compose_key_events_with(&mut composer, &sid(), vec![typed(c)]);
        }
        assert_eq!(
            composer.flush_if_timed_out(&sid(), std::time::Duration::from_secs(3600)),
            None
        );
        assert_eq!(composer.get_buffer_content(&sid()), Some("vi"));
    }

    #[test]
    fn diagnostic_dump_returns_header() {
        // diagnostic_dump reads the process-wide composer; other tests may run
        // concurrently and mutate that shared state, so assert only on the
        // always-present header rather than session-specific contents.
        let dump = diagnostic_dump();
        assert!(
            dump.contains("[vietnamese-input] diagnostics:"),
            "diagnostic dump should include the diagnostics header, got: {dump}"
        );
        assert!(
            dump.contains("active_sessions="),
            "diagnostic dump should report the active session count, got: {dump}"
        );
    }

    // --- Property 14: Ambiguous Sequence Vietnamese Preference (task 13.3) ---
    //
    // Feature: vietnamese-input-support, Property 14: Ambiguous Sequence Vietnamese Preference
    //
    // For any input sequence that is both a valid Vietnamese composition prefix
    // *and* a valid English word fragment, the composer prefers the Vietnamese
    // interpretation until the composition is explicitly committed or
    // invalidated. We exercise this with base-vowel + Telex tone-key combos —
    // e.g. "as"→"á", "is"→"í", "us"→"ú", "os"→"ó", "es"→"é" — each of which is
    // simultaneously a real English word/fragment. While composing (before any
    // commit trigger such as space/punctuation) the in-flight buffer MUST hold
    // the Vietnamese-composed glyph rather than the raw English letters, and
    // nothing may be emitted to the remote session yet. This demonstrates the
    // Vietnamese interpretation is preferred.
    //
    // The test uses a tiny inline deterministic LCG PRNG (no external crates)
    // to generate many base-vowel + tone-key combinations, and checks each
    // against an independent, hand-derived reference of the expected glyph.
    //
    // Validates: Requirements 12.2

    /// Independent reference: the precomposed (NFC) Vietnamese glyph for a base
    /// vowel carrying a Telex tone (`s`=sắc, `f`=huyền, `r`=hỏi, `x`=ngã,
    /// `j`=nặng). Hand-derived from the Telex specification, independent of the
    /// composer's own `COMPOSE_TABLE`.
    fn expected_toned_vowel(base: char, tone: char) -> char {
        match (base, tone) {
            ('a', 's') => 'á', ('a', 'f') => 'à', ('a', 'r') => 'ả', ('a', 'x') => 'ã', ('a', 'j') => 'ạ',
            ('e', 's') => 'é', ('e', 'f') => 'è', ('e', 'r') => 'ẻ', ('e', 'x') => 'ẽ', ('e', 'j') => 'ẹ',
            ('i', 's') => 'í', ('i', 'f') => 'ì', ('i', 'r') => 'ỉ', ('i', 'x') => 'ĩ', ('i', 'j') => 'ị',
            ('o', 's') => 'ó', ('o', 'f') => 'ò', ('o', 'r') => 'ỏ', ('o', 'x') => 'õ', ('o', 'j') => 'ọ',
            ('u', 's') => 'ú', ('u', 'f') => 'ù', ('u', 'r') => 'ủ', ('u', 'x') => 'ũ', ('u', 'j') => 'ụ',
            ('y', 's') => 'ý', ('y', 'f') => 'ỳ', ('y', 'r') => 'ỷ', ('y', 'x') => 'ỹ', ('y', 'j') => 'ỵ',
            _ => unreachable!("unsupported base/tone combo: {base}{tone}"),
        }
    }

    /// Minimal deterministic 64-bit LCG (Knuth MMIX constants). Inline so the
    /// property test needs no external PRNG/property crate. Returns the next
    /// state; callers derive bounded values from its high bits.
    fn lcg_next(state: u64) -> u64 {
        state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407)
    }

    #[test]
    fn property_14_ambiguous_sequence_prefers_vietnamese() {
        // Base vowels and Telex tone keys whose 2-key sequences double as common
        // English words/fragments ("as", "is", "us", "of"→handled elsewhere, …).
        const BASES: [char; 6] = ['a', 'e', 'i', 'o', 'u', 'y'];
        const TONES: [char; 5] = ['s', 'f', 'r', 'x', 'j'];

        // A few explicit ambiguous English words to guarantee coverage of the
        // motivating cases regardless of what the PRNG samples.
        let anchors: [(char, char); 4] = [
            ('a', 's'), // "as"
            ('i', 's'), // "is"
            ('u', 's'), // "us"
            ('o', 's'), // "os"
        ];

        let mut state: u64 = 0x9E37_79B9_7F4A_7C15; // fixed seed → reproducible
        let total_cases = 240usize; // well above the 100-case minimum

        for case in 0..total_cases {
            let (base, tone) = if case < anchors.len() {
                anchors[case]
            } else {
                state = lcg_next(state);
                let base = BASES[((state >> 33) as usize) % BASES.len()];
                state = lcg_next(state);
                let tone = TONES[((state >> 33) as usize) % TONES.len()];
                (base, tone)
            };

            // Fresh composer per case → fully isolated composition state.
            let mut composer = VietnameseComposer::with_method(InputMethod::Telex);

            // Type the base vowel: buffered, nothing emitted to the remote yet.
            let after_base = compose_key_events_with(&mut composer, &sid(), vec![typed(base)]);
            assert!(
                after_base.is_empty(),
                "base vowel '{base}' should be buffered (Vietnamese interpretation), \
                 not emitted as raw English; got {after_base:?}"
            );
            assert_eq!(
                composer.get_buffer_content(&sid()),
                Some(base.to_string().as_str()),
                "buffer should hold the base vowel '{base}' while composing"
            );

            // Type the tone key: still composing, the buffer now holds the
            // Vietnamese glyph and nothing has been sent to the remote.
            let after_tone = compose_key_events_with(&mut composer, &sid(), vec![typed(tone)]);
            assert!(
                after_tone.is_empty(),
                "the ambiguous sequence \"{base}{tone}\" must remain an in-flight \
                 Vietnamese composition (no remote output) before commit; got {after_tone:?}"
            );

            let expected = expected_toned_vowel(base, tone).to_string();
            assert_eq!(
                composer.get_buffer_content(&sid()),
                Some(expected.as_str()),
                "while composing, the buffer for \"{base}{tone}\" must hold the \
                 Vietnamese-composed glyph \"{expected}\" (preferred over the raw \
                 English fragment \"{base}{tone}\")"
            );
        }
    }
}
