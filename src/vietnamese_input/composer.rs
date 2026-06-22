//! The Vietnamese input composer (main orchestrator).
//!
//! The [`VietnameseComposer`] is the central component of the Vietnamese input
//! pipeline. It owns the per-session [`CompositionBuffer`]s, holds an instance
//! of every input-method engine (Telex, VNI, VNI Windows), and dispatches each
//! keystroke to the engine selected by the currently active [`InputMethod`].
//!
//! ## Responsibilities
//!
//! - **Engine routing**: forwards keystrokes to the active engine, or passes
//!   them straight through when the method is [`InputMethod::Off`].
//! - **Per-session state**: maintains an independent [`CompositionBuffer`] for
//!   each [`SessionID`], created lazily on the first keystroke and removed when
//!   the session closes. This guarantees composition state never leaks between
//!   remote sessions.
//! - **Commit handling**: when an engine reports a commit trigger (space,
//!   punctuation, or any non-composable character via
//!   [`TransformResult::CommitAndPass`]), the buffered composition is committed,
//!   Unicode-normalized, and emitted together with the trigger character.
//! - **Normalization**: composed output is normalized (NFC by default) via the
//!   [`UnicodeNormalizer`] before being sent to the remote session.
//!
//! ## Keystroke representation
//!
//! [`process_key`](VietnameseComposer::process_key) operates on a `char`. The
//! real keyboard hook (added in a later task in `keyboard.rs`) is responsible
//! for extracting the typed character from the platform [`KeyEvent`] before
//! calling the composer, and for turning a [`ComposerResult`] back into the
//! appropriate network action (suppress, send text, or send a raw key event).
//! For pass-through results the composer synthesizes a minimal text
//! [`KeyEvent`] carrying the original character; the hook may substitute the
//! original event if it needs to preserve modifiers.
//!
//! ## Configuration
//!
//! For now the composer holds only the pieces of configuration it needs — the
//! active method and a [`UnicodeNormalizer`]. The full `VietnameseInputConfig`
//! (persistence, shortcuts, overlay options, custom rule directories) is
//! introduced in a later task and will be threaded through here.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use hbb_common::log;
use hbb_common::message_proto::KeyEvent;

use super::{
    CompositionBuffer, ComposerResult, CompositionState, InputMethod, InputMethodEngine,
    NormalizationForm, SessionID, TelexEngine, TransformResult, UnicodeNormalizer, VniEngine,
    VniWindowsEngine,
};

/// Default time a session's in-flight composition may sit idle before it is
/// flushed to the remote session as raw text.
///
/// An incomplete VNI sequence or an invalid Telex sequence that receives no
/// further keystrokes within this window is treated as "done" and emitted
/// verbatim, so the user's literal keystrokes are never lost (requirements
/// 2.6, 12.1). Five hundred milliseconds matches the configured
/// `timeout_ms` default in the design's configuration schema.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(500);

/// Per-keystroke processing budget.
///
/// Each keystroke should be processed (from reception to composition-state
/// update) within this window so input never feels laggy (requirements 6.2,
/// 13.1). When a keystroke takes longer the composer cannot un-do the work
/// after the fact, so — per requirement 13.5 — it degrades gracefully: it logs
/// a performance-degradation warning and continues with best-effort processing
/// rather than dropping the (already-applied) composition.
pub const PROCESSING_BUDGET: Duration = Duration::from_millis(10);

/// Number of *consecutive* over-budget keystrokes that escalates from a
/// per-keystroke warning to a sustained performance-degradation event.
///
/// A single slow keystroke can be a scheduling hiccup; three in a row indicates
/// the system genuinely cannot keep up, which is logged once as a degradation
/// event (requirement 13.5).
pub const SLOW_KEYSTROKE_THRESHOLD: u32 = 3;

/// Maximum memory the composer may use for composition state across **all**
/// sessions (10 MB, requirement 13.6).
///
/// When [`estimated_memory_bytes`](VietnameseComposer::estimated_memory_bytes)
/// exceeds this budget the composer flushes completed compositions and logs an
/// overflow warning (requirement 13.7) via
/// [`enforce_memory_limit`](VietnameseComposer::enforce_memory_limit).
pub const MEMORY_LIMIT_BYTES: usize = 10 * 1024 * 1024;

/// Dedicated log target for all Vietnamese-input diagnostics (requirement
/// 17.5).
///
/// Every composer/integration log statement is emitted with this `target` *and*
/// a human-readable `[vietnamese-input]` message prefix. The distinct target
/// lets operators route Vietnamese-input logs to a dedicated log file separate
/// from the main application logs — `hbb_common::log` is the standard `log`
/// crate, so a `log4rs`/`flexi_logger` appender can be bound to the
/// `"vietnamese_input"` target without touching the call sites here. Until such
/// a dedicated appender is configured, the message prefix still allows the logs
/// to be filtered out of the shared log.
pub const LOG_TARGET: &str = "vietnamese_input";

/// The central Vietnamese input orchestrator.
///
/// Holds one instance of every input-method engine and an independent
/// composition buffer per session, dispatching keystrokes to the engine
/// selected by [`active_method`](VietnameseComposer::active_method).
#[derive(Debug)]
pub struct VietnameseComposer {
    /// Per-session composition buffers, keyed by session identifier. Created
    /// lazily on the first composable keystroke for a session.
    buffers: HashMap<SessionID, CompositionBuffer>,
    /// The currently active input method. When [`InputMethod::Off`], all
    /// keystrokes pass through unchanged.
    active_method: InputMethod,
    /// The most recently active *composing* method (never [`InputMethod::Off`]).
    ///
    /// Remembered when composition is turned off via
    /// [`toggle_method`](VietnameseComposer::toggle_method) so the same method
    /// can be restored when it is turned back on. Defaults to
    /// [`InputMethod::Telex`].
    last_active_method: InputMethod,
    /// Telex engine instance.
    telex: TelexEngine,
    /// VNI engine instance.
    vni: VniEngine,
    /// VNI Windows engine instance.
    vni_windows: VniWindowsEngine,
    /// Normalizes composed output before it is emitted (NFC by default).
    normalizer: UnicodeNormalizer,
    /// Number of consecutive keystrokes whose processing exceeded
    /// [`PROCESSING_BUDGET`]. Reset to zero by any keystroke processed within
    /// budget. Drives the sustained performance-degradation event logged once
    /// [`SLOW_KEYSTROKE_THRESHOLD`] consecutive slow keystrokes is reached
    /// (requirement 13.5).
    consecutive_slow_keystrokes: u32,
    /// Whether a memory-overflow warning is currently in effect. Used to log
    /// the over-limit warning only on the transition into the over-budget state
    /// (rather than on every keystroke) and to note recovery when usage falls
    /// back under [`MEMORY_LIMIT_BYTES`] (requirement 13.7).
    memory_warning_active: bool,
}

impl VietnameseComposer {
    /// Create a new composer with composition disabled ([`InputMethod::Off`])
    /// and NFC normalization.
    ///
    /// Use [`set_method`](Self::set_method) to activate an input method.
    pub fn new() -> Self {
        Self {
            buffers: HashMap::new(),
            active_method: InputMethod::Off,
            last_active_method: InputMethod::Telex,
            telex: TelexEngine::new(),
            vni: VniEngine::new(),
            vni_windows: VniWindowsEngine::new(),
            normalizer: UnicodeNormalizer::new(),
            consecutive_slow_keystrokes: 0,
            memory_warning_active: false,
        }
    }

    /// Create a new composer with the given starting input method and NFC
    /// normalization.
    pub fn with_method(method: InputMethod) -> Self {
        let mut composer = Self::new();
        composer.set_method(method);
        composer
    }

    /// Return the currently active input method.
    pub fn active_method(&self) -> InputMethod {
        self.active_method
    }

    /// Set the active input method.
    ///
    /// Existing per-session buffers are preserved across the switch so that an
    /// in-flight composition is not lost when the method changes. Whenever a
    /// *composing* method (anything other than [`InputMethod::Off`]) is
    /// selected, it is remembered as the last-active method so that
    /// [`toggle_method`](Self::toggle_method) can restore it later.
    pub fn set_method(&mut self, method: InputMethod) {
        self.active_method = method;
        if method != InputMethod::Off {
            self.last_active_method = method;
        }
    }

    /// Advance to the next input method in the fixed cycling order and return
    /// the newly active method.
    ///
    /// The order wraps deterministically:
    /// `Telex → VNI → VNI Windows → Off → Telex`. Per-session buffers are
    /// preserved across the switch (via [`set_method`](Self::set_method)), so an
    /// in-flight composition is not lost when cycling. Switching the active
    /// engine is a constant-time enum update, well within the 100ms switching
    /// budget.
    ///
    /// _Requirements: 4.1, 4.2, 4.7, 4.8_
    pub fn cycle_method(&mut self) -> InputMethod {
        let next = match self.active_method {
            InputMethod::Telex => InputMethod::Vni,
            InputMethod::Vni => InputMethod::VniWindows,
            InputMethod::VniWindows => InputMethod::Off,
            InputMethod::Off => InputMethod::Telex,
        };
        self.set_method(next);
        next
    }

    /// Toggle Vietnamese composition on or off and return the newly active
    /// method.
    ///
    /// - When a composing method is active, composition is turned **off**: the
    ///   current method is remembered (as the last-active method) and the active
    ///   method becomes [`InputMethod::Off`].
    /// - When composition is **off**, it is turned back **on** by restoring the
    ///   last-active composing method (defaulting to [`InputMethod::Telex`] if
    ///   none has been used yet).
    ///
    /// Per-session buffers are preserved across the toggle so an in-flight
    /// composition is not lost. Switching the active engine is a constant-time
    /// enum update, well within the 100ms switching budget.
    ///
    /// _Requirements: 4.1, 4.2, 4.7_
    pub fn toggle_method(&mut self) -> InputMethod {
        if self.active_method == InputMethod::Off {
            // Turn composition back on with the remembered method.
            let restore = self.last_active_method;
            self.active_method = restore;
            restore
        } else {
            // Turn composition off, remembering the current method.
            self.last_active_method = self.active_method;
            self.active_method = InputMethod::Off;
            InputMethod::Off
        }
    }

    /// Set the Unicode normalization form applied to composed output.
    pub fn set_normalization(&mut self, form: NormalizationForm) {
        self.normalizer.set_form(form);
    }

    /// Process a single typed character for the given session.
    ///
    /// Returns a [`ComposerResult`] describing what the caller should do:
    ///
    /// - [`ComposerResult::Consumed`] — the keystroke was buffered as part of an
    ///   in-flight composition; nothing should be sent yet.
    /// - [`ComposerResult::Compose`] — a composition completed; the contained
    ///   normalized Unicode text (composed syllable plus the trigger character)
    ///   should be sent to the remote session.
    /// - [`ComposerResult::PassThrough`] — the character is not part of a
    ///   Vietnamese composition (method is `Off`, or a trigger arrived with an
    ///   empty buffer); the contained key event should be sent unchanged.
    /// - [`ComposerResult::Flush`] — the buffer held an invalid sequence; its
    ///   raw keystrokes should be sent verbatim.
    ///
    /// ## Resource guards
    ///
    /// Each call is wrapped with two best-effort resource guards so the composer
    /// degrades gracefully under load rather than blocking or growing without
    /// bound:
    ///
    /// - **Processing-time guard** — the elapsed processing time is measured and,
    ///   if it exceeds [`PROCESSING_BUDGET`] (10ms), a warning is logged and a
    ///   sustained performance-degradation event is logged once
    ///   [`SLOW_KEYSTROKE_THRESHOLD`] consecutive slow keystrokes occur. The
    ///   already-computed result is still returned (best-effort, requirement
    ///   13.5); the work cannot be un-done after timing it.
    /// - **Memory guard** — when estimated composer memory crosses
    ///   [`MEMORY_LIMIT_BYTES`] (10MB) a memory-overflow warning is logged once
    ///   (requirement 13.7). Reclaiming memory by flushing completed
    ///   compositions requires delivering the flushed text to the remote, which
    ///   only the integration layer can do, so it is exposed separately via
    ///   [`enforce_memory_limit`](Self::enforce_memory_limit).
    ///
    /// _Requirements: 6.2, 6.3, 13.1, 13.2, 13.5, 13.6, 13.7_
    pub fn process_key(&mut self, session_id: &SessionID, key: char) -> ComposerResult {
        let start = Instant::now();
        let result = self.process_key_inner(session_id, key);
        // Best-effort resource guards (never alter `result`).
        self.guard_processing_time(session_id, key, start.elapsed());
        self.guard_memory_usage();
        result
    }

    /// Core keystroke processing, wrapped by [`process_key`](Self::process_key)
    /// with the resource guards. Kept separate so the timing measurement covers
    /// the full composition work for a single keystroke.
    fn process_key_inner(&mut self, session_id: &SessionID, key: char) -> ComposerResult {
        // Per-keystroke diagnostics (requirement 17.1). Naturally gated by the
        // debug log level, so this is silent in normal operation.
        log::debug!(
            target: LOG_TARGET,
            "[vietnamese-input] keystroke received: session={}, key={:?}, method={:?}",
            session_id,
            key,
            self.active_method
        );
        // When composition is disabled, every keystroke passes through.
        if self.active_method == InputMethod::Off {
            return ComposerResult::PassThrough(passthrough_event(key));
        }

        let method = self.active_method;

        // Lazily create the buffer for this session on first use.
        let buffer = self
            .buffers
            .entry(session_id.clone())
            .or_insert_with(|| CompositionBuffer::new(session_id.clone()));

        // Record composition activity for timeout-based flushing: this keystroke
        // refreshes the session's idle timer (see `flush_if_timed_out`).
        buffer.touch();

        // Dispatch to the active engine. Borrowing `self.telex`/`self.vni`/
        // `self.vni_windows` immutably alongside the mutable `buffer` borrow of
        // `self.buffers` is sound because they are disjoint struct fields.
        let result = match method {
            InputMethod::Telex => self.telex.process_key(buffer, key),
            InputMethod::Vni => self.vni.process_key(buffer, key),
            InputMethod::VniWindows => self.vni_windows.process_key(buffer, key),
            // Handled above; kept exhaustive for clarity.
            InputMethod::Off => return ComposerResult::PassThrough(passthrough_event(key)),
        };

        match result {
            // Still composing — buffer updated, nothing to send yet.
            TransformResult::Transformed
            | TransformResult::ToneApplied
            | TransformResult::Buffering => {
                // Log the transformation applied and the resulting buffer state
                // (requirement 17.1).
                log::debug!(
                    target: LOG_TARGET,
                    "[vietnamese-input] transformation applied: session={}, result={:?}, buffer={:?}, method={:?}",
                    session_id,
                    result,
                    buffer.current_text(),
                    method
                );
                ComposerResult::Consumed
            }

            // A commit trigger (space, punctuation, non-Vietnamese char) ended
            // the composition. Emit the composed syllable plus the trigger.
            TransformResult::CommitAndPass(trigger) => {
                if buffer.is_empty() {
                    // Nothing was being composed; pass the trigger through.
                    self.discard_if_empty(session_id);
                    ComposerResult::PassThrough(passthrough_event(trigger))
                } else {
                    // Capture the raw sequence before `commit` empties the
                    // buffer so the diagnostics can report what produced the
                    // composed text (requirement 17.1).
                    let raw_sequence: String = buffer.raw_input.iter().collect();
                    let composed = buffer.commit();
                    let mut out = self.normalizer.normalize(&composed);
                    out.push(trigger);
                    log::debug!(
                        target: LOG_TARGET,
                        "[vietnamese-input] composed text sent: session={}, raw_sequence={:?}, composed={:?}, output={:?}, method={:?}",
                        session_id,
                        raw_sequence,
                        composed,
                        out,
                        method
                    );
                    ComposerResult::Compose(out)
                }
            }

            // The sequence cannot compose; flush the raw keystrokes verbatim.
            TransformResult::InvalidSequence => {
                // Capture context before the flush empties the buffer
                // (requirement 17.3: input sequence, buffer state, method).
                let buffer_state = buffer.current_text().to_string();
                let raw = buffer.flush_raw();
                log::debug!(
                    target: LOG_TARGET,
                    "[vietnamese-input] invalid sequence flushed as raw text: session={}, raw={:?}, buffer_state={:?}, method={:?}",
                    session_id,
                    raw,
                    buffer_state,
                    method
                );
                ComposerResult::Flush(raw)
            }
        }
    }

    /// Return the current (in-flight) composed text for a session, if any.
    ///
    /// Returns `None` when no buffer exists for the session or the buffer is
    /// empty. Used to drive the composition overlay.
    pub fn get_buffer_content(&self, session_id: &SessionID) -> Option<&str> {
        self.buffers
            .get(session_id)
            .filter(|b| !b.is_empty())
            .map(|b| b.current_text())
    }

    /// Produce a human-readable diagnostic dump of the composition state for
    /// **every** active session (requirement 17.2).
    ///
    /// The dump reports the composer's active input method and, for each session
    /// with a live composition buffer, its current composed text, the raw
    /// keystroke sequence that produced it, the depth of the backspace-reversal
    /// history, and the buffer capacity. Sessions are listed in sorted
    /// `session_id` order so the output is deterministic (and therefore
    /// testable). When there are no active buffers this is noted explicitly.
    ///
    /// This backs the diagnostic command exposed by the integration layer
    /// (`diagnostic_dump`) so a user troubleshooting Vietnamese input can dump
    /// the live buffer state for all remote sessions on demand. It reads state
    /// only and never mutates the composer.
    ///
    /// _Requirements: 17.2_
    pub fn dump_buffer_state(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        let _ = writeln!(
            out,
            "[vietnamese-input] diagnostics: active_method={:?}, active_sessions={}",
            self.active_method,
            self.buffers.len()
        );

        if self.buffers.is_empty() {
            let _ = writeln!(out, "  (no active composition buffers)");
            return out;
        }

        // Sort by session id so the dump is deterministic regardless of the
        // HashMap's iteration order.
        let mut session_ids: Vec<&SessionID> = self.buffers.keys().collect();
        session_ids.sort();
        for sid in session_ids {
            let buffer = &self.buffers[sid];
            let raw: String = buffer.raw_input.iter().collect();
            let _ = writeln!(
                out,
                "  session={}: current={:?}, raw_input={:?}, history_len={}, capacity={}",
                sid,
                buffer.current,
                raw,
                buffer.history.len(),
                buffer.capacity
            );
        }
        out
    }

    /// Flush a session's in-flight composition as raw keystrokes.
    ///
    /// Returns the raw keystroke text (empty if there was nothing buffered), or
    /// `None` if the session has no buffer. Used when an incomplete sequence
    /// must be emitted as-is (e.g. on timeout or application switch).
    pub fn flush_session(&mut self, session_id: &SessionID) -> Option<String> {
        self.buffers.get_mut(session_id).map(|b| b.flush_raw())
    }

    /// Flush a single session's in-flight composition as raw keystrokes **if**
    /// it has been idle for at least `timeout`.
    ///
    /// This is the timeout-based recovery path for incomplete or invalid
    /// sequences (requirements 2.6, 12.1): an in-flight composition whose most
    /// recent keystroke is older than `timeout` is emitted verbatim — the
    /// user's literal keystrokes — rather than left dangling. Returns the raw
    /// text that should be sent to the remote session, or `None` when the
    /// session has no buffer, the buffer is empty, or it has not yet timed out.
    ///
    /// The composer has no internal timer; a focus/idle handler or a periodic
    /// timer in RustDesk's event loop is expected to call this (e.g. with
    /// [`DEFAULT_TIMEOUT`]). Wiring that timer is environment-dependent and
    /// lives outside the composer; the integration layer exposes thin wrappers
    /// (`flush_timed_out`, `flush_all_timed_out`) over this method.
    ///
    /// _Requirements: 2.6, 12.1, 12.4_
    pub fn flush_if_timed_out(
        &mut self,
        session_id: &SessionID,
        timeout: Duration,
    ) -> Option<String> {
        // Copy the active method out before the mutable buffer borrow so the
        // borrow checker is satisfied (both are disjoint fields of `self`, but
        // the logging read happens while `buffer` is mutably borrowed).
        let method = self.active_method;
        let flushed = {
            let buffer = self.buffers.get_mut(session_id)?;
            if !buffer.is_timed_out(timeout) {
                return None;
            }
            // Capture context for diagnostics *before* the flush empties the
            // buffer (requirement 12.4: log input sequence, buffer state, method).
            let raw_sequence: String = buffer.raw_input.iter().collect();
            let buffer_state = buffer.current_text().to_string();
            let raw = buffer.flush_raw();
            log::debug!(
                "[vietnamese-input] timeout flush after {:?}: session={}, raw_sequence={:?}, buffer_state={:?}, method={:?}",
                timeout, session_id, raw_sequence, buffer_state, method
            );
            raw
        };
        // Drop the now-empty buffer so no stale entry is retained.
        self.discard_if_empty(session_id);
        Some(flushed)
    }

    /// Flush every session whose in-flight composition has been idle for at
    /// least `timeout`, returning each flushed `(session_id, raw_text)` pair.
    ///
    /// Convenience over [`flush_if_timed_out`](Self::flush_if_timed_out) for a
    /// periodic sweep across all live sessions. The caller is responsible for
    /// sending each returned raw text to the corresponding remote session.
    ///
    /// _Requirements: 2.6, 12.1, 12.4_
    pub fn flush_timed_out_sessions(&mut self, timeout: Duration) -> Vec<(SessionID, String)> {
        let session_ids: Vec<SessionID> = self.buffers.keys().cloned().collect();
        let mut flushed = Vec::new();
        for sid in session_ids {
            if let Some(raw) = self.flush_if_timed_out(&sid, timeout) {
                flushed.push((sid, raw));
            }
        }
        flushed
    }

    /// Flush **all** sessions' in-flight compositions as raw keystrokes,
    /// regardless of how long they have been idle.
    ///
    /// This is the application-switch / focus-loss recovery path (requirement
    /// 12.3): when the user switches away from RustDesk, any partial
    /// composition must not be silently dropped, so each session's raw
    /// keystrokes are emitted verbatim. Returns every flushed
    /// `(session_id, raw_text)` pair; the caller sends each to its remote
    /// session. Empty buffers are skipped and dropped.
    ///
    /// As with the timeout path, the composer does not observe focus changes
    /// itself — RustDesk's window/focus handling calls this (the integration
    /// layer exposes it as `flush_on_app_switch`).
    ///
    /// _Requirements: 12.3, 12.4_
    pub fn flush_all_sessions(&mut self) -> Vec<(SessionID, String)> {
        let method = self.active_method;
        let session_ids: Vec<SessionID> = self.buffers.keys().cloned().collect();
        let mut flushed = Vec::new();
        for sid in session_ids {
            let raw = {
                let buffer = match self.buffers.get_mut(&sid) {
                    Some(b) if !b.is_empty() => b,
                    _ => continue,
                };
                let raw_sequence: String = buffer.raw_input.iter().collect();
                let buffer_state = buffer.current_text().to_string();
                let raw = buffer.flush_raw();
                log::debug!(
                    "[vietnamese-input] app-switch flush: session={}, raw_sequence={:?}, buffer_state={:?}, method={:?}",
                    sid, raw_sequence, buffer_state, method
                );
                raw
            };
            self.discard_if_empty(&sid);
            flushed.push((sid, raw));
        }
        flushed
    }

    /// Clear and remove a session's composition buffer.
    ///
    /// Called when a remote session closes so that no stale composition state
    /// or memory is retained for it.
    pub fn clear_session(&mut self, session_id: &SessionID) {
        self.buffers.remove(session_id);
    }

    /// Return the number of sessions that currently have a composition buffer.
    ///
    /// A buffer is created lazily on the first composable keystroke for a
    /// session ([`process_key`](Self::process_key)) and removed when the
    /// session closes ([`clear_session`](Self::clear_session)) or when an
    /// in-flight composition is fully reverted or committed. This count
    /// therefore reflects the number of sessions with *live* Vietnamese input
    /// state rather than every connected remote session.
    ///
    /// It backs the multi-session support guarantees (at least 10 concurrent
    /// sessions with independent state) and is the basis for later memory
    /// monitoring of composer state across all sessions.
    ///
    /// _Requirements: 6.1, 11.4_
    pub fn session_count(&self) -> usize {
        self.buffers.len()
    }

    /// Return `true` if the given session currently has a composition buffer.
    ///
    /// Useful for verifying keystroke routing and confirming that closing a
    /// session releases its state.
    ///
    /// _Requirements: 6.4, 11.1_
    pub fn has_session(&self, session_id: &SessionID) -> bool {
        self.buffers.contains_key(session_id)
    }

    /// Reverse the most recent transformation step for a session (backspace).
    ///
    /// Backspace during Vietnamese composition is *intelligent*: instead of
    /// deleting a whole composed glyph, it reverses composition one
    /// transformation at a time in last-applied-first-removed (LIFO) order, so
    /// the user steps back through the exact intermediate states that produced
    /// the current text (e.g. `ượ` → `ươ` → `uo` → `u`). This mirrors the
    /// history the active engine records in the [`CompositionBuffer`] via
    /// [`record_step`](CompositionBuffer::record_step), where each applied tone
    /// mark, vowel mark, consonant mark, and base character is a separate step.
    ///
    /// Behavior:
    ///
    /// - **Composition in flight** — [`CompositionBuffer::pop_step`] removes the
    ///   most recently applied step and restores the buffer to the immediately
    ///   preceding state. The keystroke is reported as
    ///   [`ComposerResult::Consumed`] so the caller suppresses it and refreshes
    ///   the composition overlay with the reverted text. Popping the final
    ///   base-character step empties the buffer; the now-empty buffer is
    ///   discarded so no stale entry is retained.
    /// - **Empty buffer or no buffer** — when there is no composition history
    ///   to reverse (the session has no buffer, or its history is exhausted),
    ///   the backspace is *not* consumed: a raw backspace key event is
    ///   forwarded to the remote session via [`ComposerResult::PassThrough`] so
    ///   the remote system performs the deletion.
    ///
    /// _Requirements: 1.7, 2.8, 10.1, 10.2, 10.3, 10.4, 10.5, 10.6, 10.9_
    pub fn handle_backspace(&mut self, session_id: &SessionID) -> ComposerResult {
        // Attempt to reverse one transformation step. A missing buffer behaves
        // the same as an exhausted history: there is nothing local to revert.
        let reverted = self
            .buffers
            .get_mut(session_id)
            .and_then(|buffer| buffer.pop_step())
            .is_some();

        if reverted {
            // One transformation was undone (LIFO). If the reversal emptied the
            // buffer (the base character was removed) drop it so we don't keep
            // an empty entry around; the caller updates the overlay from the
            // restored buffer state.
            self.discard_if_empty(session_id);
            ComposerResult::Consumed
        } else {
            // No in-flight composition to reverse: forward a raw backspace to
            // the remote session and discard any lingering empty buffer.
            self.discard_if_empty(session_id);
            ComposerResult::PassThrough(passthrough_event(BACKSPACE))
        }
    }

    /// Estimate the total memory, in bytes, currently held by composition state
    /// across **all** sessions.
    ///
    /// This sums the approximate heap footprint of every live
    /// [`CompositionBuffer`]: the session id, the current composed text, the raw
    /// keystroke sequence, and each retained history snapshot (its text and raw
    /// input), plus the fixed size of the owning structs. It is an estimate, not
    /// an exact allocator accounting — string/`Vec` *capacity* (not just length)
    /// is used so reserved-but-unused buffer space is counted, which is the
    /// conservative choice for a memory budget.
    ///
    /// Backs the 10MB cross-session memory budget (requirement 13.6) and the
    /// overflow handling in [`enforce_memory_limit`](Self::enforce_memory_limit)
    /// (requirement 13.7).
    ///
    /// _Requirements: 13.6, 13.7_
    pub fn estimated_memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .buffers
                .iter()
                .map(|(id, buffer)| {
                    // The HashMap stores the key (an owned SessionID) alongside
                    // the buffer; count the key's heap bytes too.
                    id.capacity() + estimate_buffer_bytes(buffer)
                })
                .sum::<usize>()
    }

    /// Returns `true` when estimated composer memory exceeds the
    /// [`MEMORY_LIMIT_BYTES`] (10MB) cross-session budget (requirement 13.6).
    pub fn is_over_memory_limit(&self) -> bool {
        self.estimated_memory_bytes() > MEMORY_LIMIT_BYTES
    }

    /// Enforce the cross-session memory budget (requirement 13.7).
    ///
    /// When estimated composer memory exceeds [`MEMORY_LIMIT_BYTES`] (10MB), the
    /// in-flight compositions are flushed to the remote session as raw text — so
    /// the user's literal keystrokes are never lost — and a memory-overflow
    /// warning is logged. The flushed `(session_id, raw_text)` pairs are
    /// returned for the caller (the integration layer) to deliver to each remote
    /// session, mirroring the timeout / app-switch flush paths. When usage is
    /// within budget this is a no-op returning an empty vector.
    ///
    /// The composer cannot itself send text to the remote, which is why the
    /// reclaim step is exposed as a method to be driven by the integration layer
    /// rather than performed silently inside
    /// [`process_key`](Self::process_key).
    ///
    /// _Requirements: 13.6, 13.7_
    pub fn enforce_memory_limit(&mut self) -> Vec<(SessionID, String)> {
        let used = self.estimated_memory_bytes();
        if used <= MEMORY_LIMIT_BYTES {
            self.memory_warning_active = false;
            return Vec::new();
        }
        log::warn!(
            "[vietnamese-input] memory overflow: estimated {} bytes of composition state across {} session(s) exceeds the {} byte budget; flushing completed compositions to remote",
            used,
            self.buffers.len(),
            MEMORY_LIMIT_BYTES
        );
        self.memory_warning_active = true;
        // Flush every session's in-flight composition as raw text to reclaim
        // memory. `flush_all_sessions` already drops the emptied buffers.
        self.flush_all_sessions()
    }

    /// Best-effort processing-time guard: log when a single keystroke exceeds
    /// [`PROCESSING_BUDGET`] and escalate to a sustained degradation event after
    /// [`SLOW_KEYSTROKE_THRESHOLD`] consecutive slow keystrokes.
    ///
    /// The composition work has already been applied by the time this runs, so
    /// the keystroke is never discarded; per requirement 13.5 the composer
    /// continues with best-effort processing and simply records the latency
    /// degradation.
    ///
    /// _Requirements: 6.2, 6.3, 13.1, 13.5_
    fn guard_processing_time(&mut self, session_id: &SessionID, key: char, elapsed: Duration) {
        if elapsed <= PROCESSING_BUDGET {
            // A keystroke within budget breaks any run of slow keystrokes.
            self.consecutive_slow_keystrokes = 0;
            return;
        }
        self.consecutive_slow_keystrokes = self.consecutive_slow_keystrokes.saturating_add(1);
        log::warn!(
            "[vietnamese-input] keystroke processing exceeded the {:?} budget ({:?}): session={}, key={:?}; continuing best-effort",
            PROCESSING_BUDGET,
            elapsed,
            session_id,
            key
        );
        if self.consecutive_slow_keystrokes >= SLOW_KEYSTROKE_THRESHOLD {
            log::warn!(
                "[vietnamese-input] performance degradation: {} consecutive keystrokes exceeded the {:?} processing budget",
                self.consecutive_slow_keystrokes,
                PROCESSING_BUDGET
            );
        }
    }

    /// Best-effort memory guard run after each keystroke: log a one-shot
    /// memory-overflow warning when estimated usage first crosses
    /// [`MEMORY_LIMIT_BYTES`], and note recovery when it falls back under.
    ///
    /// Actually reclaiming memory (flushing completed compositions to the
    /// remote) is deferred to [`enforce_memory_limit`](Self::enforce_memory_limit)
    /// because it must deliver the flushed text, which only the integration
    /// layer can do. Logging is edge-triggered via `memory_warning_active` so a
    /// sustained over-limit condition does not spam the log on every keystroke.
    ///
    /// _Requirements: 13.6, 13.7_
    fn guard_memory_usage(&mut self) {
        let over = self.is_over_memory_limit();
        if over && !self.memory_warning_active {
            log::warn!(
                "[vietnamese-input] memory budget exceeded: estimated {} bytes of composition state across {} session(s) exceeds the {} byte budget; call enforce_memory_limit to flush completed compositions",
                self.estimated_memory_bytes(),
                self.buffers.len(),
                MEMORY_LIMIT_BYTES
            );
            self.memory_warning_active = true;
        } else if !over && self.memory_warning_active {
            // Usage dropped back under budget (e.g. after a commit/flush).
            self.memory_warning_active = false;
        }
    }

    /// Remove a session's buffer if it exists and is empty, to avoid retaining
    /// empty entries created by a lone trigger keystroke.
    fn discard_if_empty(&mut self, session_id: &SessionID) {
        if self
            .buffers
            .get(session_id)
            .map(|b| b.is_empty())
            .unwrap_or(false)
        {
            self.buffers.remove(session_id);
        }
    }
}

impl Default for VietnameseComposer {
    fn default() -> Self {
        Self::new()
    }
}

/// The ASCII backspace control character (`U+0008`). Forwarded to the remote
/// session (wrapped in a pass-through key event) when a backspace arrives with
/// no in-flight composition to reverse.
const BACKSPACE: char = '\u{0008}';

/// Build a minimal text [`KeyEvent`] carrying a single character, used for
/// pass-through results. The real keyboard hook may replace this with the
/// original platform event when it needs to preserve modifier state.
fn passthrough_event(key: char) -> KeyEvent {
    let mut event = KeyEvent::new();
    event.set_seq(key.to_string());
    event
}

/// Approximate the heap + struct footprint of a single composition buffer.
///
/// Used by [`VietnameseComposer::estimated_memory_bytes`] to enforce the 10MB
/// cross-session budget (requirements 13.6, 13.7). String/`Vec` *capacity* is
/// counted (not just length) so reserved-but-unused space is included, which is
/// the conservative choice for a memory budget. `char` history/raw input is
/// counted at `size_of::<char>()` (4 bytes) per scalar — the in-memory `Vec<char>`
/// representation, not the UTF-8 byte length.
fn estimate_buffer_bytes(buffer: &CompositionBuffer) -> usize {
    let mut bytes = std::mem::size_of::<CompositionBuffer>();
    bytes += buffer.session_id.capacity();
    bytes += buffer.current.capacity();
    bytes += buffer.raw_input.capacity() * std::mem::size_of::<char>();
    for state in &buffer.history {
        bytes += std::mem::size_of::<CompositionState>();
        bytes += state.text.capacity();
        bytes += state.raw.capacity() * std::mem::size_of::<char>();
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionID {
        "session-1".to_string()
    }

    /// Feed a whole sequence into the composer for one session, returning the
    /// final result. Intermediate results are asserted to be `Consumed`.
    fn feed(composer: &mut VietnameseComposer, sid: &SessionID, seq: &str) -> ComposerResult {
        let mut last = ComposerResult::Consumed;
        for ch in seq.chars() {
            last = composer.process_key(sid, ch);
        }
        last
    }

    // --- Construction / method state -------------------------------------

    #[test]
    fn new_composer_defaults_to_off() {
        let composer = VietnameseComposer::new();
        assert_eq!(composer.active_method(), InputMethod::Off);
    }

    #[test]
    fn set_method_changes_active_method() {
        let mut composer = VietnameseComposer::new();
        composer.set_method(InputMethod::Telex);
        assert_eq!(composer.active_method(), InputMethod::Telex);
        composer.set_method(InputMethod::Vni);
        assert_eq!(composer.active_method(), InputMethod::Vni);
    }

    // --- Off pass-through -------------------------------------------------

    #[test]
    fn off_mode_passes_every_key_through() {
        let mut composer = VietnameseComposer::new(); // Off by default
        let sid = session();
        match composer.process_key(&sid, 'a') {
            ComposerResult::PassThrough(ev) => assert_eq!(ev.seq(), "a"),
            other => panic!("expected PassThrough, got {other:?}"),
        }
        // No buffer should be created in Off mode.
        assert!(composer.get_buffer_content(&sid).is_none());
    }

    // --- Method routing: Telex vs VNI ------------------------------------

    #[test]
    fn routes_to_telex_engine() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        // "as " → "á" + space via Telex tone key 's'.
        feed(&mut composer, &sid, "as");
        match composer.process_key(&sid, ' ') {
            ComposerResult::Compose(text) => assert_eq!(text, "á "),
            other => panic!("expected Compose, got {other:?}"),
        }
    }

    #[test]
    fn routes_to_vni_engine() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Vni);
        let sid = session();
        // "a1 " → "á" + space via VNI tone key '1'.
        feed(&mut composer, &sid, "a1");
        match composer.process_key(&sid, ' ') {
            ComposerResult::Compose(text) => assert_eq!(text, "á "),
            other => panic!("expected Compose, got {other:?}"),
        }
    }

    #[test]
    fn routes_to_vni_windows_engine() {
        let mut composer = VietnameseComposer::with_method(InputMethod::VniWindows);
        let sid = session();
        // "a61 " → "ấ" + space via VNI Windows (vowel mark then tone).
        feed(&mut composer, &sid, "a61");
        match composer.process_key(&sid, ' ') {
            ComposerResult::Compose(text) => assert_eq!(text, "ấ "),
            other => panic!("expected Compose, got {other:?}"),
        }
    }

    // --- Buffering / Consumed --------------------------------------------

    #[test]
    fn composable_keys_are_consumed_while_building() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        assert_eq!(composer.process_key(&sid, 'v'), ComposerResult::Consumed);
        assert_eq!(composer.process_key(&sid, 'i'), ComposerResult::Consumed);
        assert_eq!(composer.process_key(&sid, 'e'), ComposerResult::Consumed);
        assert_eq!(composer.get_buffer_content(&sid), Some("vie"));
    }

    // --- Commit triggers + normalization ---------------------------------

    #[test]
    fn space_commits_and_appends_trigger() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        // Classic "được" composition then a committing space.
        feed(&mut composer, &sid, "dduowjc");
        match composer.process_key(&sid, ' ') {
            ComposerResult::Compose(text) => assert_eq!(text, "được "),
            other => panic!("expected Compose, got {other:?}"),
        }
        // Buffer is empty after commit.
        assert!(composer.get_buffer_content(&sid).is_none());
    }

    #[test]
    fn punctuation_commits_composition() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        feed(&mut composer, &sid, "as");
        match composer.process_key(&sid, '.') {
            ComposerResult::Compose(text) => assert_eq!(text, "á."),
            other => panic!("expected Compose, got {other:?}"),
        }
    }

    #[test]
    fn commit_output_is_nfc_normalized() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        // "Vieejt" → "Việt"; verify NFC single-codepoint form for the vowel.
        feed(&mut composer, &sid, "Vieejt");
        match composer.process_key(&sid, ' ') {
            ComposerResult::Compose(text) => {
                assert_eq!(text, "Việt ");
                // 'ệ' is a single precomposed scalar in NFC: V i ệ t space = 5.
                assert_eq!(text.chars().count(), 5);
            }
            other => panic!("expected Compose, got {other:?}"),
        }
    }

    #[test]
    fn nfd_normalization_decomposes_output() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        composer.set_normalization(NormalizationForm::Nfd);
        let sid = session();
        feed(&mut composer, &sid, "as");
        match composer.process_key(&sid, ' ') {
            ComposerResult::Compose(text) => {
                // NFD: base 'a' + combining acute (U+0301) + space = 3 scalars.
                assert_eq!(text, "a\u{0301} ");
                assert_eq!(text.chars().count(), 3);
            }
            other => panic!("expected Compose, got {other:?}"),
        }
    }

    // --- Trigger with empty buffer passes through ------------------------

    #[test]
    fn trigger_with_empty_buffer_passes_through() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        match composer.process_key(&sid, ' ') {
            ComposerResult::PassThrough(ev) => assert_eq!(ev.seq(), " "),
            other => panic!("expected PassThrough, got {other:?}"),
        }
        // No lingering empty buffer is retained.
        assert!(composer.get_buffer_content(&sid).is_none());
    }

    // --- Per-session buffer creation & isolation -------------------------

    #[test]
    fn buffers_are_created_per_session() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let a = "session-a".to_string();
        let b = "session-b".to_string();
        feed(&mut composer, &a, "vi");
        feed(&mut composer, &b, "ng");
        assert_eq!(composer.get_buffer_content(&a), Some("vi"));
        assert_eq!(composer.get_buffer_content(&b), Some("ng"));
    }

    #[test]
    fn sessions_do_not_share_state() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let a = "session-a".to_string();
        let b = "session-b".to_string();
        // Interleave keystrokes across two sessions.
        composer.process_key(&a, 'd');
        composer.process_key(&b, 'm');
        composer.process_key(&a, 'd'); // dd → đ in session A only
        composer.process_key(&b, 'e');
        assert_eq!(composer.get_buffer_content(&a), Some("đ"));
        assert_eq!(composer.get_buffer_content(&b), Some("me"));
    }

    // --- Session count helper --------------------------------------------

    #[test]
    fn session_count_tracks_live_buffers() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        assert_eq!(composer.session_count(), 0);

        let a = "session-a".to_string();
        let b = "session-b".to_string();
        feed(&mut composer, &a, "vi");
        assert_eq!(composer.session_count(), 1);
        assert!(composer.has_session(&a));
        assert!(!composer.has_session(&b));

        feed(&mut composer, &b, "ng");
        assert_eq!(composer.session_count(), 2);
        assert!(composer.has_session(&b));

        // Closing a session releases its state and decrements the count.
        composer.clear_session(&a);
        assert_eq!(composer.session_count(), 1);
        assert!(!composer.has_session(&a));
        assert!(composer.has_session(&b));
    }

    #[test]
    fn off_mode_creates_no_session_buffers() {
        let mut composer = VietnameseComposer::new(); // Off by default
        let sid = session();
        composer.process_key(&sid, 'a');
        composer.process_key(&sid, 's');
        assert_eq!(composer.session_count(), 0);
        assert!(!composer.has_session(&sid));
    }

    // --- 10+ concurrent sessions (Requirements 11.1–11.4) ----------------

    #[test]
    fn ten_concurrent_sessions_compose_independently() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);

        // Ten distinct sessions, each with its own distinct composition.
        let sessions: Vec<SessionID> = (0..10).map(|i| format!("session-{i}")).collect();

        // Drive every session with the classic "dduowjc" → "được" sequence,
        // interleaving one keystroke at a time across all sessions so that a
        // round of input touches all ten buffers before any of them advances.
        // This catches any cross-session state leakage in routing.
        let sequence: Vec<char> = "dduowjc".chars().collect();
        for &key in &sequence {
            for sid in &sessions {
                assert_eq!(composer.process_key(sid, key), ComposerResult::Consumed);
            }
        }

        // All ten sessions are live and hold the same in-flight composition,
        // each in its own buffer.
        assert_eq!(composer.session_count(), 10);
        for sid in &sessions {
            assert_eq!(composer.get_buffer_content(sid), Some("được"));
        }

        // Committing each session emits its own composed text and empties only
        // that session's buffer.
        for (i, sid) in sessions.iter().enumerate() {
            match composer.process_key(sid, ' ') {
                ComposerResult::Compose(text) => assert_eq!(text, "được "),
                other => panic!("session {i}: expected Compose, got {other:?}"),
            }
            assert!(composer.get_buffer_content(sid).is_none());
            // The remaining sessions are untouched by this commit.
            assert_eq!(composer.session_count(), 10 - (i + 1));
        }
    }

    #[test]
    fn interleaved_distinct_sequences_stay_isolated_across_many_sessions() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);

        // Twelve sessions, each typing a *different* Telex sequence, fed one
        // keystroke per session per round. Each expected composed result is the
        // committed Vietnamese text for that session's raw sequence.
        let cases: Vec<(SessionID, &str, &str)> = vec![
            ("s0".into(), "as", "á"),
            ("s1".into(), "af", "à"),
            ("s2".into(), "ar", "ả"),
            ("s3".into(), "ax", "ã"),
            ("s4".into(), "aj", "ạ"),
            ("s5".into(), "dd", "đ"),
            ("s6".into(), "ow", "ơ"),
            ("s7".into(), "uw", "ư"),
            ("s8".into(), "ee", "ê"),
            ("s9".into(), "oo", "ô"),
            ("s10".into(), "aa", "â"),
            ("s11".into(), "aw", "ă"),
        ];

        // Interleave keystrokes: round-robin one character from each session's
        // raw sequence until all are exhausted.
        let max_len = cases.iter().map(|(_, raw, _)| raw.len()).max().unwrap();
        for idx in 0..max_len {
            for (sid, raw, _) in &cases {
                if let Some(ch) = raw.chars().nth(idx) {
                    assert_eq!(composer.process_key(sid, ch), ComposerResult::Consumed);
                }
            }
        }

        assert_eq!(composer.session_count(), cases.len());

        // Every session committed independently yields exactly its own result,
        // proving no keystrokes bled between the interleaved buffers.
        for (sid, raw, expected) in &cases {
            match composer.process_key(sid, ' ') {
                ComposerResult::Compose(text) => {
                    assert_eq!(text, format!("{expected} "), "session {sid} (raw {raw})")
                }
                other => panic!("session {sid}: expected Compose, got {other:?}"),
            }
        }
    }

    #[test]
    fn composition_state_preserved_across_session_switches() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let a = "session-a".to_string();
        let b = "session-b".to_string();

        // Start composing the prefix of "người" in session A (mirrors the
        // design's session switching example), then switch away mid-composition.
        // "ngu" are plain base characters, so the in-flight buffer is "ngu".
        feed(&mut composer, &a, "ngu");
        assert_eq!(composer.get_buffer_content(&a), Some("ngu"));

        // Switch to session B and complete a whole word there ("được").
        feed(&mut composer, &b, "dduowjc");
        assert_eq!(composer.get_buffer_content(&b), Some("được"));
        match composer.process_key(&b, ' ') {
            ComposerResult::Compose(text) => assert_eq!(text, "được "),
            other => panic!("expected Compose for session B, got {other:?}"),
        }
        assert!(composer.get_buffer_content(&b).is_none());

        // Session A's incomplete composition must be exactly as we left it.
        assert_eq!(composer.get_buffer_content(&a), Some("ngu"));

        // Resume composing in A; the buffer continues from "ngu" → "người"
        // ("owif": o, w applies the horn to the u/o cluster → ươ, i, then the
        // huyền tone f).
        feed(&mut composer, &a, "owif");
        match composer.process_key(&a, ' ') {
            ComposerResult::Compose(text) => assert_eq!(text, "người "),
            other => panic!("expected Compose for session A, got {other:?}"),
        }
    }

    // --- Session lifecycle ------------------------------------------------

    #[test]
    fn clear_session_removes_buffer() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        feed(&mut composer, &sid, "vi");
        assert!(composer.get_buffer_content(&sid).is_some());
        composer.clear_session(&sid);
        assert!(composer.get_buffer_content(&sid).is_none());
    }

    #[test]
    fn flush_session_returns_raw_keystrokes() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        feed(&mut composer, &sid, "dduowjc");
        // Flushing returns the verbatim keystrokes, not the composed text.
        assert_eq!(composer.flush_session(&sid), Some("dduowjc".to_string()));
        // Buffer is empty afterwards.
        assert!(composer.get_buffer_content(&sid).is_none());
    }

    #[test]
    fn flush_session_on_unknown_session_is_none() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        assert_eq!(composer.flush_session(&"nope".to_string()), None);
    }

    // --- Method preserved across switch ----------------------------------

    #[test]
    fn buffer_preserved_when_switching_method() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        feed(&mut composer, &sid, "vi");
        composer.set_method(InputMethod::Vni);
        // Existing composition is preserved across the method switch.
        assert_eq!(composer.get_buffer_content(&sid), Some("vi"));
    }

    // --- Backspace handling (task 8.2) -----------------------------------

    #[test]
    fn backspace_reverts_tone_then_base_in_lifo_order() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        // "as" → "á": base 'a' then the sắc tone mark.
        feed(&mut composer, &sid, "as");
        assert_eq!(composer.get_buffer_content(&sid), Some("á"));

        // First backspace removes the most recent step (the tone), reverting
        // to the base vowel.
        assert_eq!(composer.handle_backspace(&sid), ComposerResult::Consumed);
        assert_eq!(composer.get_buffer_content(&sid), Some("a"));

        // Second backspace removes the base character, emptying the buffer.
        assert_eq!(composer.handle_backspace(&sid), ComposerResult::Consumed);
        assert!(composer.get_buffer_content(&sid).is_none());

        // With nothing left to revert, backspace is forwarded to the remote.
        match composer.handle_backspace(&sid) {
            ComposerResult::PassThrough(ev) => assert_eq!(ev.seq(), "\u{0008}"),
            other => panic!("expected PassThrough backspace, got {other:?}"),
        }
    }

    #[test]
    fn backspace_replays_composition_states_in_reverse_for_uowj() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();

        // Build "ượ" via Telex (u, o, w → horn vowel mark, j → nặng tone),
        // snapshotting the buffer after each keystroke. These are the exact
        // intermediate composition states (the "ượ" → "ươ" → "uo" → "u"
        // reversal chain from the design's backspace example).
        let mut forward = Vec::new();
        for ch in "uowj".chars() {
            assert_eq!(composer.process_key(&sid, ch), ComposerResult::Consumed);
            forward.push(
                composer
                    .get_buffer_content(&sid)
                    .expect("composition in flight")
                    .to_string(),
            );
        }
        // Sanity: the last snapshot is the fully composed cluster currently in
        // the buffer.
        assert_eq!(
            forward.last().map(String::as_str),
            composer.get_buffer_content(&sid)
        );

        // Backspace until the buffer empties, recording the state after each
        // reversal. Every backspace that undoes a step is Consumed.
        let mut reversed = Vec::new();
        while composer.get_buffer_content(&sid).is_some() {
            assert_eq!(composer.handle_backspace(&sid), ComposerResult::Consumed);
            if let Some(state) = composer.get_buffer_content(&sid) {
                reversed.push(state.to_string());
            }
        }

        // The states visited on the way back are the forward states in reverse,
        // excluding the fully composed state we started reversing from.
        let expected: Vec<String> = forward.iter().rev().skip(1).cloned().collect();
        assert_eq!(reversed, expected);

        // Buffer is empty and discarded; a further backspace passes through.
        assert!(composer.get_buffer_content(&sid).is_none());
        match composer.handle_backspace(&sid) {
            ComposerResult::PassThrough(ev) => assert_eq!(ev.seq(), "\u{0008}"),
            other => panic!("expected PassThrough backspace, got {other:?}"),
        }
    }

    #[test]
    fn backspace_with_no_buffer_passes_through() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        // No keystrokes typed yet → no buffer exists for the session.
        match composer.handle_backspace(&sid) {
            ComposerResult::PassThrough(ev) => assert_eq!(ev.seq(), "\u{0008}"),
            other => panic!("expected PassThrough backspace, got {other:?}"),
        }
        // No empty buffer is created as a side effect.
        assert!(composer.get_buffer_content(&sid).is_none());
    }

    #[test]
    fn backspace_on_empty_buffer_passes_through() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        // Compose then fully revert so the buffer existed but is now empty.
        feed(&mut composer, &sid, "a");
        assert_eq!(composer.handle_backspace(&sid), ComposerResult::Consumed);
        assert!(composer.get_buffer_content(&sid).is_none());

        // A backspace against the (now empty / removed) buffer passes through.
        match composer.handle_backspace(&sid) {
            ComposerResult::PassThrough(ev) => assert_eq!(ev.seq(), "\u{0008}"),
            other => panic!("expected PassThrough backspace, got {other:?}"),
        }
    }

    #[test]
    fn backspace_is_isolated_per_session() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let a = "session-a".to_string();
        let b = "session-b".to_string();
        feed(&mut composer, &a, "as"); // "á" in session A
        feed(&mut composer, &b, "vi"); // "vi" in session B

        // Backspacing in A must not touch B's composition.
        assert_eq!(composer.handle_backspace(&a), ComposerResult::Consumed);
        assert_eq!(composer.get_buffer_content(&a), Some("a"));
        assert_eq!(composer.get_buffer_content(&b), Some("vi"));
    }

    // --- Method cycling (task 10.1) --------------------------------------

    #[test]
    fn cycle_method_follows_fixed_order_and_wraps() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        // Telex → VNI → VNI Windows → Off → Telex (wrap).
        assert_eq!(composer.cycle_method(), InputMethod::Vni);
        assert_eq!(composer.active_method(), InputMethod::Vni);
        assert_eq!(composer.cycle_method(), InputMethod::VniWindows);
        assert_eq!(composer.cycle_method(), InputMethod::Off);
        assert_eq!(composer.cycle_method(), InputMethod::Telex);
        // A full cycle of four presses returns to the start.
        assert_eq!(composer.active_method(), InputMethod::Telex);
    }

    #[test]
    fn cycle_method_wraps_from_any_start() {
        // Starting from Off, the next method is Telex.
        let mut composer = VietnameseComposer::new(); // Off by default
        assert_eq!(composer.cycle_method(), InputMethod::Telex);

        // Four cycles from any starting method return to that method.
        for start in [
            InputMethod::Telex,
            InputMethod::Vni,
            InputMethod::VniWindows,
            InputMethod::Off,
        ] {
            let mut c = VietnameseComposer::with_method(start);
            for _ in 0..4 {
                c.cycle_method();
            }
            assert_eq!(c.active_method(), start, "4 cycles from {start:?}");
        }
    }

    #[test]
    fn cycle_method_preserves_buffers() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        feed(&mut composer, &sid, "vi");
        assert_eq!(composer.get_buffer_content(&sid), Some("vi"));
        // Cycling the method does not disturb the in-flight composition.
        composer.cycle_method();
        assert_eq!(composer.get_buffer_content(&sid), Some("vi"));
    }

    // --- Method toggling (task 10.1) -------------------------------------

    #[test]
    fn toggle_method_off_then_on_restores_method() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Vni);
        // Toggling off remembers VNI and switches to Off.
        assert_eq!(composer.toggle_method(), InputMethod::Off);
        assert_eq!(composer.active_method(), InputMethod::Off);
        // Toggling back on restores the remembered VNI method.
        assert_eq!(composer.toggle_method(), InputMethod::Vni);
        assert_eq!(composer.active_method(), InputMethod::Vni);
    }

    #[test]
    fn toggle_method_defaults_to_telex_when_never_composed() {
        // A fresh composer is Off and has never used a composing method.
        let mut composer = VietnameseComposer::new();
        assert_eq!(composer.active_method(), InputMethod::Off);
        // Turning on defaults to Telex.
        assert_eq!(composer.toggle_method(), InputMethod::Telex);
        assert_eq!(composer.active_method(), InputMethod::Telex);
    }

    #[test]
    fn toggle_method_remembers_method_set_via_cycle() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        // Cycle to VNI Windows, then toggle off and back on.
        composer.cycle_method(); // Vni
        composer.cycle_method(); // VniWindows
        assert_eq!(composer.active_method(), InputMethod::VniWindows);
        assert_eq!(composer.toggle_method(), InputMethod::Off);
        assert_eq!(composer.toggle_method(), InputMethod::VniWindows);
    }

    #[test]
    fn toggle_method_preserves_buffers() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        feed(&mut composer, &sid, "vi");
        // Toggling off and on must not lose the in-flight composition.
        composer.toggle_method();
        assert_eq!(composer.get_buffer_content(&sid), Some("vi"));
        composer.toggle_method();
        assert_eq!(composer.get_buffer_content(&sid), Some("vi"));
    }

    // --- Timeout / app-switch flushing (task 13.1) -----------------------

    use std::time::Duration;

    #[test]
    fn flush_if_timed_out_returns_none_for_unknown_session() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        assert_eq!(composer.flush_if_timed_out(&sid, Duration::ZERO), None);
    }

    #[test]
    fn flush_if_timed_out_flushes_stale_sequence_as_raw() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Vni);
        let sid = session();
        // Incomplete VNI sequence ("u" + horn key "7" → "ư"); raw input is "u7".
        feed(&mut composer, &sid, "u7");
        assert_eq!(composer.get_buffer_content(&sid), Some("ư"));
        // A zero timeout means any non-empty, touched buffer is already stale,
        // so the raw keystrokes are flushed verbatim.
        let flushed = composer.flush_if_timed_out(&sid, Duration::ZERO);
        assert_eq!(flushed.as_deref(), Some("u7"));
        // The buffer is dropped after the flush.
        assert!(composer.get_buffer_content(&sid).is_none());
        assert!(!composer.has_session(&sid));
    }

    #[test]
    fn flush_if_timed_out_keeps_fresh_sequence() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        feed(&mut composer, &sid, "vi");
        // A large timeout: the just-typed sequence has not gone stale.
        assert_eq!(
            composer.flush_if_timed_out(&sid, Duration::from_secs(3600)),
            None
        );
        // Composition is preserved untouched.
        assert_eq!(composer.get_buffer_content(&sid), Some("vi"));
    }

    #[test]
    fn flush_timed_out_sessions_flushes_only_stale_buffers() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let a = "session-a".to_string();
        let b = "session-b".to_string();
        feed(&mut composer, &a, "vi");
        feed(&mut composer, &b, "ng");
        // Zero timeout flushes every non-empty session.
        let mut flushed = composer.flush_timed_out_sessions(Duration::ZERO);
        flushed.sort();
        assert_eq!(
            flushed,
            vec![
                ("session-a".to_string(), "vi".to_string()),
                ("session-b".to_string(), "ng".to_string()),
            ]
        );
        assert_eq!(composer.session_count(), 0);
    }

    #[test]
    fn flush_all_sessions_flushes_everything_as_raw() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let a = "session-a".to_string();
        let b = "session-b".to_string();
        // "dd" → "đ" but raw input is "dd"; flushing on app switch emits raw.
        feed(&mut composer, &a, "dd");
        feed(&mut composer, &b, "uw");
        let mut flushed = composer.flush_all_sessions();
        flushed.sort();
        assert_eq!(
            flushed,
            vec![
                ("session-a".to_string(), "dd".to_string()),
                ("session-b".to_string(), "uw".to_string()),
            ]
        );
        // All buffers are released after an app-switch flush.
        assert_eq!(composer.session_count(), 0);
    }

    #[test]
    fn flush_all_sessions_is_empty_when_no_buffers() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        assert!(composer.flush_all_sessions().is_empty());
    }

    // --- Graceful degradation: processing-time guard (task 13.2) ----------

    #[test]
    fn processing_guard_resets_counter_within_budget() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        // A keystroke comfortably within the 10ms budget leaves the counter at 0.
        composer.guard_processing_time(&sid, 'a', Duration::from_millis(1));
        assert_eq!(composer.consecutive_slow_keystrokes, 0);
    }

    #[test]
    fn processing_guard_counts_consecutive_slow_keystrokes() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        let slow = PROCESSING_BUDGET + Duration::from_millis(5);

        composer.guard_processing_time(&sid, 'a', slow);
        assert_eq!(composer.consecutive_slow_keystrokes, 1);
        composer.guard_processing_time(&sid, 'b', slow);
        assert_eq!(composer.consecutive_slow_keystrokes, 2);
        // Third consecutive slow keystroke reaches the degradation threshold.
        composer.guard_processing_time(&sid, 'c', slow);
        assert_eq!(composer.consecutive_slow_keystrokes, SLOW_KEYSTROKE_THRESHOLD);

        // A keystroke back within budget breaks the run and resets the counter.
        composer.guard_processing_time(&sid, 'd', Duration::ZERO);
        assert_eq!(composer.consecutive_slow_keystrokes, 0);
    }

    #[test]
    fn processing_guard_exactly_at_budget_is_not_slow() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        // Exactly at the budget is within tolerance (guard uses strictly-greater).
        composer.guard_processing_time(&sid, 'a', PROCESSING_BUDGET);
        assert_eq!(composer.consecutive_slow_keystrokes, 0);
    }

    // --- Graceful degradation: memory monitoring (task 13.2) --------------

    #[test]
    fn fresh_composer_is_within_memory_budget() {
        let composer = VietnameseComposer::with_method(InputMethod::Telex);
        assert!(!composer.is_over_memory_limit());
        assert!(composer.estimated_memory_bytes() <= MEMORY_LIMIT_BYTES);
    }

    #[test]
    fn estimated_memory_grows_with_composition() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let baseline = composer.estimated_memory_bytes();
        let sid = session();
        // An in-flight composition adds buffer + history bytes on top of the
        // fixed composer footprint.
        feed(&mut composer, &sid, "dduowjc");
        assert!(
            composer.estimated_memory_bytes() > baseline,
            "memory estimate should grow once a session buffer exists"
        );
    }

    #[test]
    fn estimated_memory_returns_to_baseline_after_clear() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let baseline = composer.estimated_memory_bytes();
        let sid = session();
        feed(&mut composer, &sid, "vi");
        assert!(composer.estimated_memory_bytes() > baseline);
        composer.clear_session(&sid);
        // With the only buffer gone, usage returns to the fixed footprint.
        assert_eq!(composer.estimated_memory_bytes(), baseline);
    }

    #[test]
    fn enforce_memory_limit_is_noop_within_budget() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        feed(&mut composer, &sid, "vi");
        // Normal usage is far below 10MB, so nothing is flushed.
        assert!(composer.enforce_memory_limit().is_empty());
        assert_eq!(composer.get_buffer_content(&sid), Some("vi"));
    }

    #[test]
    fn enforce_memory_limit_flushes_completed_compositions_when_over_budget() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        // Build a real in-flight composition (raw "as" → "á").
        feed(&mut composer, &sid, "as");

        // Inflate the buffer's capacity past the 10MB budget to simulate memory
        // pressure without depending on the allocator's growth heuristics.
        composer
            .buffers
            .get_mut(&sid)
            .expect("session buffer")
            .current
            .reserve(MEMORY_LIMIT_BYTES + 1024);
        assert!(composer.is_over_memory_limit());

        // Enforcing the limit flushes the session's raw keystrokes for delivery
        // to the remote and releases its buffer.
        let flushed = composer.enforce_memory_limit();
        assert_eq!(flushed, vec![(sid.clone(), "as".to_string())]);
        assert_eq!(composer.session_count(), 0);
        assert!(!composer.is_over_memory_limit());
    }

    #[test]
    fn is_over_memory_limit_false_for_many_normal_sessions() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        // Ten concurrent sessions each composing a word stays well under 10MB.
        for i in 0..10 {
            let sid = format!("session-{i}");
            feed(&mut composer, &sid, "dduowjc");
        }
        assert_eq!(composer.session_count(), 10);
        assert!(!composer.is_over_memory_limit());
    }

    // --- Diagnostics: dump_buffer_state (requirement 17.2) ----------------

    #[test]
    fn dump_buffer_state_reports_no_buffers_when_idle() {
        let composer = VietnameseComposer::with_method(InputMethod::Telex);
        let dump = composer.dump_buffer_state();
        assert!(
            dump.contains("active_method=Telex"),
            "dump should report the active method, got: {dump}"
        );
        assert!(
            dump.contains("active_sessions=0"),
            "dump should report zero sessions, got: {dump}"
        );
        assert!(
            dump.contains("(no active composition buffers)"),
            "dump should note the absence of buffers, got: {dump}"
        );
    }

    #[test]
    fn dump_buffer_state_reports_in_flight_composition() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let sid = session();
        // "as" composes to "á" but is not committed, so it remains in flight.
        feed(&mut composer, &sid, "as");

        let dump = composer.dump_buffer_state();
        assert!(
            dump.contains("active_sessions=1"),
            "dump should report one session, got: {dump}"
        );
        assert!(
            dump.contains("session=session-1"),
            "dump should name the session, got: {dump}"
        );
        // The composed current text and the raw keystrokes that produced it.
        assert!(
            dump.contains("current=\"á\""),
            "dump should show the composed text, got: {dump}"
        );
        assert!(
            dump.contains("raw_input=\"as\""),
            "dump should show the raw keystrokes, got: {dump}"
        );
    }

    #[test]
    fn dump_buffer_state_lists_all_sessions_deterministically() {
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        // Create buffers out of sorted order to prove the dump sorts them.
        feed(&mut composer, &"session-b".to_string(), "as");
        feed(&mut composer, &"session-a".to_string(), "af");

        let dump = composer.dump_buffer_state();
        assert!(dump.contains("active_sessions=2"), "got: {dump}");
        let idx_a = dump.find("session=session-a").expect("session-a present");
        let idx_b = dump.find("session=session-b").expect("session-b present");
        assert!(
            idx_a < idx_b,
            "sessions should be listed in sorted id order, got: {dump}"
        );
    }
}
