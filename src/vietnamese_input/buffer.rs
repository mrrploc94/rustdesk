//! Per-session composition buffer.
//!
//! The [`CompositionBuffer`] tracks the in-flight composition state for a single
//! remote session: the current composed text, the raw keystroke sequence that
//! produced it, and a history stack used for last-applied-first-removed (LIFO)
//! backspace reversal.
//!
//! The buffer itself is intentionally engine-agnostic. Input-method engines
//! (Telex, VNI, VNI Windows) drive composition by pushing keys and recording
//! transformation steps; the buffer is responsible only for storing state,
//! enforcing history limits, and supporting reversal/commit/flush operations.

use std::time::{Duration, Instant};

use super::{VietSessionId, TransformStep};

/// Default maximum number of characters held in flight by the buffer.
pub const DEFAULT_CAPACITY: usize = 20;

/// Maximum number of transformation-history entries retained per character.
pub const MAX_HISTORY_STEPS: usize = 10;

/// Per-session buffer that tracks composition state and history for backspace
/// reversal.
#[derive(Debug, Clone, Default)]
pub struct CompositionBuffer {
    /// Current composed text being built.
    pub current: String,
    /// Raw keystroke sequence for the current composition.
    pub raw_input: Vec<char>,
    /// History stack for backspace reversal.
    pub history: Vec<CompositionState>,
    /// Maximum buffer capacity (in characters).
    pub capacity: usize,
    /// Session identifier this buffer belongs to.
    pub session_id: VietSessionId,
    /// Timestamp of the most recent keystroke recorded into this buffer.
    ///
    /// Updated via [`touch`](CompositionBuffer::touch) every time the composer
    /// processes a keystroke for this session, and reset to `None` whenever the
    /// buffer is committed, flushed, or cleared. It drives timeout-based
    /// flushing: an in-flight composition whose most recent keystroke is older
    /// than the configured timeout is considered stale (an incomplete or
    /// invalid sequence) and is flushed to the remote session as raw text.
    ///
    /// `Instant` does not implement `Default`, but `Option<Instant>` defaults
    /// to `None`, so the surrounding `#[derive(Default)]` continues to work.
    pub last_keystroke: Option<Instant>,
}

/// A snapshot of the composition state at a single point, used for LIFO
/// backspace reversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompositionState {
    /// The composed text at this point.
    pub text: String,
    /// The raw input at this point.
    pub raw: Vec<char>,
    /// The transformation that was applied to reach this state.
    pub transform: TransformStep,
}

impl CompositionBuffer {
    /// Create a new, empty composition buffer for the given session.
    ///
    /// The buffer is created with the default capacity of
    /// [`DEFAULT_CAPACITY`] characters.
    pub fn new(session_id: VietSessionId) -> Self {
        Self {
            current: String::new(),
            raw_input: Vec::new(),
            history: Vec::new(),
            capacity: DEFAULT_CAPACITY,
            session_id,
            last_keystroke: None,
        }
    }

    /// Push a single raw key onto the buffer as a new base character.
    ///
    /// This appends `key` to both the raw-input sequence and the current
    /// composed text, and records a [`TransformStep::BaseCharacter`] entry in
    /// the history stack so the operation can be reversed via [`pop_step`].
    ///
    /// Returns the current composed text after the push.
    ///
    /// [`pop_step`]: CompositionBuffer::pop_step
    pub fn push_key(&mut self, key: char) -> &str {
        self.raw_input.push(key);
        self.current.push(key);
        self.record_step(TransformStep::BaseCharacter(key));
        &self.current
    }

    /// Record a transformation step against the current buffer state.
    ///
    /// A snapshot of the resulting state (the current composed text and raw
    /// input) is pushed onto the history stack, tagged with the `transform`
    /// that produced it. When the history grows beyond [`MAX_HISTORY_STEPS`]
    /// entries, the oldest entries are discarded so that at most
    /// [`MAX_HISTORY_STEPS`] steps are retained.
    ///
    /// Input-method engines call this after mutating [`current`] /
    /// [`raw_input`] to apply a vowel mark, tone mark, or consonant mark so the
    /// change participates in LIFO backspace reversal.
    ///
    /// [`current`]: CompositionBuffer::current
    /// [`raw_input`]: CompositionBuffer::raw_input
    pub fn record_step(&mut self, transform: TransformStep) {
        self.history.push(CompositionState {
            text: self.current.clone(),
            raw: self.raw_input.clone(),
            transform,
        });
        if self.history.len() > MAX_HISTORY_STEPS {
            let overflow = self.history.len() - MAX_HISTORY_STEPS;
            self.history.drain(0..overflow);
        }
    }

    /// Reverse the most recent transformation step (LIFO backspace).
    ///
    /// The most recent history entry is removed and the buffer's current text
    /// and raw input are restored to the state immediately preceding it (or to
    /// empty if no earlier state remains). Returns the removed
    /// [`CompositionState`], or `None` if the history stack is empty.
    pub fn pop_step(&mut self) -> Option<CompositionState> {
        let popped = self.history.pop()?;
        if let Some(prev) = self.history.last() {
            self.current = prev.text.clone();
            self.raw_input = prev.raw.clone();
        } else {
            self.current.clear();
            self.raw_input.clear();
        }
        Some(popped)
    }

    /// Commit the current composition, returning the composed Unicode text and
    /// resetting the buffer to an empty state.
    pub fn commit(&mut self) -> String {
        let out = std::mem::take(&mut self.current);
        self.raw_input.clear();
        self.history.clear();
        self.last_keystroke = None;
        out
    }

    /// Flush the buffer as raw text, returning the raw keystroke sequence as a
    /// `String` and resetting the buffer to an empty state.
    ///
    /// Used when a sequence is invalid or times out: the user's original
    /// keystrokes are emitted verbatim rather than the (partial) composition.
    pub fn flush_raw(&mut self) -> String {
        let out: String = self.raw_input.iter().collect();
        self.clear();
        out
    }

    /// Reset the buffer to an empty state, discarding all composition state and
    /// history. The session identifier and capacity are preserved.
    pub fn clear(&mut self) {
        self.current.clear();
        self.raw_input.clear();
        self.history.clear();
        self.last_keystroke = None;
    }

    /// Record that a keystroke was just processed into this buffer, updating
    /// the activity timestamp used for timeout-based flushing.
    ///
    /// The composer calls this on every keystroke it routes to the active
    /// engine for a session, so [`last_keystroke`](CompositionBuffer::last_keystroke)
    /// always reflects the most recent composition activity.
    pub fn touch(&mut self) {
        self.last_keystroke = Some(Instant::now());
    }

    /// Returns the timestamp of the most recent recorded keystroke, if any.
    pub fn last_keystroke(&self) -> Option<Instant> {
        self.last_keystroke
    }

    /// Returns `true` when the buffer holds an in-flight composition whose most
    /// recent keystroke is at least `timeout` old.
    ///
    /// An empty buffer is never timed out (there is nothing to flush), and a
    /// buffer with no recorded keystroke timestamp (`last_keystroke == None`)
    /// is likewise never reported as timed out. This backs the 500ms
    /// raw-flush behavior for incomplete/invalid sequences.
    pub fn is_timed_out(&self, timeout: Duration) -> bool {
        if self.is_empty() {
            return false;
        }
        match self.last_keystroke {
            Some(t) => t.elapsed() >= timeout,
            None => false,
        }
    }

    /// Returns `true` when the buffer holds no in-flight composition.
    pub fn is_empty(&self) -> bool {
        self.current.is_empty() && self.raw_input.is_empty()
    }

    /// Returns the number of Unicode scalar values in the current composed text.
    pub fn len(&self) -> usize {
        self.current.chars().count()
    }

    /// Returns the current composed text without consuming it.
    pub fn current_text(&self) -> &str {
        &self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ToneMark;

    fn buffer() -> CompositionBuffer {
        CompositionBuffer::new("session-1".to_string())
    }

    #[test]
    fn new_buffer_is_empty_with_default_capacity() {
        let b = buffer();
        assert!(b.is_empty());
        assert_eq!(b.len(), 0);
        assert_eq!(b.capacity, DEFAULT_CAPACITY);
        assert_eq!(b.current_text(), "");
        assert_eq!(b.session_id, "session-1");
    }

    #[test]
    fn push_key_appends_to_current_and_raw() {
        let mut b = buffer();
        assert_eq!(b.push_key('v'), "v");
        assert_eq!(b.push_key('i'), "vi");
        assert_eq!(b.push_key('e'), "vie");
        assert_eq!(b.current_text(), "vie");
        assert_eq!(b.raw_input, vec!['v', 'i', 'e']);
        assert_eq!(b.len(), 3);
        assert!(!b.is_empty());
    }

    #[test]
    fn len_counts_unicode_scalars_not_bytes() {
        let mut b = buffer();
        // 'ư' is multi-byte in UTF-8 but a single scalar value.
        b.push_key('ư');
        b.push_key('ợ');
        b.push_key('c');
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn pop_step_reverts_base_characters_in_lifo_order() {
        let mut b = buffer();
        b.push_key('a');
        b.push_key('b');
        b.push_key('c');
        assert_eq!(b.current_text(), "abc");

        let popped = b.pop_step().expect("step to pop");
        assert_eq!(popped.transform, TransformStep::BaseCharacter('c'));
        assert_eq!(b.current_text(), "ab");
        assert_eq!(b.raw_input, vec!['a', 'b']);

        b.pop_step();
        assert_eq!(b.current_text(), "a");

        b.pop_step();
        assert_eq!(b.current_text(), "");
        assert!(b.is_empty());
    }

    #[test]
    fn pop_step_on_empty_history_returns_none() {
        let mut b = buffer();
        assert!(b.pop_step().is_none());
        assert!(b.is_empty());
    }

    #[test]
    fn pop_step_reverses_recorded_transform_steps() {
        let mut b = buffer();
        // Simulate an engine building "ư" from base 'u' then applying a horn.
        b.push_key('u');
        b.current = "ư".to_string();
        b.record_step(TransformStep::VowelMark {
            base: 'u',
            result: 'ư',
        });
        b.current = "ứ".to_string();
        b.record_step(TransformStep::ToneMark {
            tone: ToneMark::Sac,
            target_vowel: 'ư',
        });
        assert_eq!(b.current_text(), "ứ");

        // First backspace removes the tone, reverting to the horn state.
        let s = b.pop_step().expect("tone step");
        assert_eq!(
            s.transform,
            TransformStep::ToneMark {
                tone: ToneMark::Sac,
                target_vowel: 'ư'
            }
        );
        assert_eq!(b.current_text(), "ư");

        // Second backspace removes the vowel mark, reverting to base 'u'.
        let s = b.pop_step().expect("vowel-mark step");
        assert_eq!(
            s.transform,
            TransformStep::VowelMark {
                base: 'u',
                result: 'ư'
            }
        );
        assert_eq!(b.current_text(), "u");

        // Third backspace removes the base character.
        b.pop_step().expect("base step");
        assert!(b.is_empty());
    }

    #[test]
    fn history_is_capped_at_max_history_steps() {
        let mut b = buffer();
        // Push more keys than MAX_HISTORY_STEPS to force overflow.
        for i in 0..(MAX_HISTORY_STEPS + 5) {
            b.push_key(char::from(b'a' + (i % 26) as u8));
        }
        assert_eq!(b.history.len(), MAX_HISTORY_STEPS);
        // The retained history must be the most recent entries: the latest
        // snapshot still reflects the full current text.
        let last = b.history.last().unwrap();
        assert_eq!(last.text, b.current);
    }

    #[test]
    fn commit_returns_composed_text_and_empties_buffer() {
        let mut b = buffer();
        b.push_key('v');
        b.push_key('i');
        b.push_key('e');
        let out = b.commit();
        assert_eq!(out, "vie");
        assert!(b.is_empty());
        assert!(b.history.is_empty());
        assert_eq!(b.len(), 0);
    }

    #[test]
    fn flush_raw_returns_raw_sequence_and_empties_buffer() {
        let mut b = buffer();
        b.push_key('u');
        // Engine transforms current text but raw input retains the keystrokes.
        b.current = "ư".to_string();
        b.record_step(TransformStep::VowelMark {
            base: 'u',
            result: 'ư',
        });
        b.push_key('w'); // a raw modifier key kept in raw_input
        let raw = b.flush_raw();
        assert_eq!(raw, "uw");
        assert!(b.is_empty());
        assert!(b.history.is_empty());
    }

    #[test]
    fn clear_resets_state_but_keeps_session_and_capacity() {
        let mut b = buffer();
        b.push_key('a');
        b.push_key('b');
        b.clear();
        assert!(b.is_empty());
        assert!(b.history.is_empty());
        assert_eq!(b.session_id, "session-1");
        assert_eq!(b.capacity, DEFAULT_CAPACITY);
    }

    // --- Timeout-based flush (task 13.1) ---------------------------------

    #[test]
    fn new_buffer_has_no_keystroke_timestamp() {
        let b = buffer();
        assert!(b.last_keystroke().is_none());
    }

    #[test]
    fn touch_records_keystroke_timestamp() {
        let mut b = buffer();
        b.push_key('a');
        b.touch();
        assert!(b.last_keystroke().is_some());
    }

    #[test]
    fn empty_buffer_is_never_timed_out() {
        let mut b = buffer();
        // No keystroke at all.
        assert!(!b.is_timed_out(Duration::from_millis(0)));
        // Even with a stale timestamp, an empty buffer has nothing to flush.
        b.last_keystroke = Instant::now().checked_sub(Duration::from_secs(10));
        assert!(!b.is_timed_out(Duration::from_millis(500)));
    }

    #[test]
    fn buffer_without_timestamp_is_never_timed_out() {
        let mut b = buffer();
        b.push_key('a');
        // last_keystroke stays None (touch not called), so not timed out.
        assert!(b.last_keystroke().is_none());
        assert!(!b.is_timed_out(Duration::from_millis(0)));
    }

    #[test]
    fn stale_keystroke_is_timed_out_past_timeout() {
        let mut b = buffer();
        b.push_key('u');
        b.push_key('w');
        // Pretend the last keystroke happened 600ms ago, beyond the 500ms window.
        b.last_keystroke = Instant::now().checked_sub(Duration::from_millis(600));
        assert!(b.last_keystroke().is_some());
        assert!(b.is_timed_out(Duration::from_millis(500)));
    }

    #[test]
    fn recent_keystroke_is_not_timed_out() {
        let mut b = buffer();
        b.push_key('u');
        b.touch();
        // A keystroke that just happened is well within the 500ms window.
        assert!(!b.is_timed_out(Duration::from_millis(500)));
    }

    #[test]
    fn commit_resets_keystroke_timestamp() {
        let mut b = buffer();
        b.push_key('a');
        b.touch();
        b.commit();
        assert!(b.last_keystroke().is_none());
    }

    #[test]
    fn flush_raw_resets_keystroke_timestamp() {
        let mut b = buffer();
        b.push_key('a');
        b.touch();
        b.flush_raw();
        assert!(b.last_keystroke().is_none());
    }
}
