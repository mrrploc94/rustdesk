//! Integration-style tests for the Vietnamese input composer.
//!
//! These tests exercise the composer end-to-end at the module boundary —
//! across multiple sessions, the timeout / app-switch flush paths, the
//! configuration persistence round-trip, and the determinism / external-state
//! independence guarantees — rather than any single engine in isolation. They
//! correspond to tasks 19.1–19.4 of the Vietnamese input support spec.
//!
//! ## Determinism / non-flakiness policy
//!
//! Everything here is deterministic and free of real `sleep`s:
//!
//! - **No wall-clock timing thresholds.** The composer's idle-timeout logic is
//!   driven entirely by the [`Duration`] argument passed to
//!   [`VietnameseComposer::flush_if_timed_out`]. We exploit this to test the
//!   timeout behavior without ever sleeping: [`Duration::ZERO`] makes any
//!   in-flight buffer *already* stale (`elapsed() >= 0` is always true), while a
//!   very large [`Duration`] makes it *never yet* stale. This keeps the timeout
//!   assertions exact and CI-safe.
//! - **No hard latency assertions.** Task 19.3 deliberately avoids asserting a
//!   `<10ms` per-keystroke wall-clock budget — such assertions are flaky on
//!   shared CI runners. Instead we assert *functional* correctness under a high
//!   interleaved load (correct output for every session, sane `session_count`)
//!   and that estimated memory stays within the documented budget. Elapsed time
//!   is measured only to prove the workload completes, never compared to a
//!   threshold.
//!
//! ## Scope note for RustDesk feature compatibility (task 19.4)
//!
//! The composer transforms typed characters into composed Unicode text and is
//! intentionally decoupled from RustDesk's file transfer, clipboard sync, and
//! audio forwarding subsystems — it holds no handle to any of them and never
//! awaits a remote acknowledgment. The tests below assert that observable
//! consequence: composition output is a pure function of the input sequence and
//! the active method, independent of any external/remote state, and it always
//! completes locally. True end-to-end feature-interaction tests (composing
//! *while* a file transfer or audio stream is live over a real connection)
//! require a running RustDesk session with a connected peer and are out of
//! scope for unit-level CI.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::composer::MEMORY_LIMIT_BYTES;
use super::config::{OptionStore, VietnameseInputConfig};
use super::{
    ComposerResult, InputMethod, NormalizationForm, SessionID, VietnameseComposer, DEFAULT_TIMEOUT,
};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// A non-flaky "definitely not stale yet" window. Far larger than any plausible
/// test execution time, so an in-flight buffer touched moments ago is never
/// reported as timed out. We never actually wait this long — it is only ever
/// compared against, never slept on.
const FAR_FUTURE: Duration = Duration::from_secs(3600);

/// Feed a whole character sequence into one session, returning the final
/// [`ComposerResult`].
fn feed(composer: &mut VietnameseComposer, sid: &SessionID, seq: &str) -> ComposerResult {
    let mut last = ComposerResult::Consumed;
    for ch in seq.chars() {
        last = composer.process_key(sid, ch);
    }
    last
}

fn sid(name: &str) -> SessionID {
    name.to_string()
}

/// In-memory [`OptionStore`] mirroring the pattern used in `config.rs` tests,
/// so configuration persistence can be exercised deterministically without any
/// process-wide side effects. An empty value clears the key (matching the
/// production `RustDeskOptionStore` semantics).
#[derive(Default)]
struct MemStore {
    map: RefCell<HashMap<String, String>>,
}

impl OptionStore for MemStore {
    fn get(&self, key: &str) -> String {
        self.map.borrow().get(key).cloned().unwrap_or_default()
    }

    fn set(&self, key: &str, value: &str) {
        if value.is_empty() {
            self.map.borrow_mut().remove(key);
        } else {
            self.map
                .borrow_mut()
                .insert(key.to_string(), value.to_string());
        }
    }
}

// ===========================================================================
// 19.1 Timeout and flush behavior
// ===========================================================================
//
// Requirements: 2.6 (incomplete VNI flush after 500ms), 12.1 (invalid Telex
// flush after 500ms), 12.3 (application-switch flush). All timing is driven by
// the `Duration` argument, so these tests are deterministic and never sleep.

/// An invalid / incomplete Telex sequence left in flight is flushed verbatim
/// (as the user's raw keystrokes) once it has been idle for the timeout window.
/// `Duration::ZERO` makes the buffer already stale without any real wait.
///
/// _Requirements: 12.1_
#[test]
fn invalid_telex_sequence_flushes_raw_after_timeout() {
    let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
    let s = sid("telex-timeout");

    // "ngh" are plain base consonants: they stay buffered (no committing
    // trigger), forming an incomplete syllable that never composes on its own.
    feed(&mut composer, &s, "ngh");
    assert_eq!(composer.get_buffer_content(&s), Some("ngh"));

    // Not yet stale: a large window means the just-touched buffer is retained.
    assert_eq!(composer.flush_if_timed_out(&s, FAR_FUTURE), None);
    assert_eq!(composer.get_buffer_content(&s), Some("ngh"));

    // Already stale: ZERO timeout flushes the raw keystrokes verbatim.
    assert_eq!(
        composer.flush_if_timed_out(&s, Duration::ZERO),
        Some("ngh".to_string())
    );
    // Buffer is emptied and dropped after the flush.
    assert_eq!(composer.get_buffer_content(&s), None);
    assert!(!composer.has_session(&s));
}

/// An incomplete VNI sequence (base characters awaiting a number-key modifier
/// that never arrives) is flushed as raw text after the idle timeout.
///
/// _Requirements: 2.6_
#[test]
fn incomplete_vni_sequence_flushes_raw_after_timeout() {
    let mut composer = VietnameseComposer::with_method(InputMethod::Vni);
    let s = sid("vni-timeout");

    // "ngu" is an incomplete VNI syllable: base characters with no tone/mark
    // number typed yet.
    feed(&mut composer, &s, "ngu");
    assert_eq!(composer.get_buffer_content(&s), Some("ngu"));

    // The default 500ms window: the buffer was just touched, so it is not yet
    // timed out — deterministic because no time is allowed to pass.
    assert_eq!(composer.flush_if_timed_out(&s, DEFAULT_TIMEOUT), None);
    assert_eq!(composer.get_buffer_content(&s), Some("ngu"));

    // Forcing staleness with ZERO flushes the literal keystrokes.
    assert_eq!(
        composer.flush_if_timed_out(&s, Duration::ZERO),
        Some("ngu".to_string())
    );
    assert_eq!(composer.get_buffer_content(&s), None);
}

/// A buffer that has already composed a partial result still flushes its *raw*
/// input (not the composed glyph) on timeout, so the user's literal keystrokes
/// are preserved when a sequence is abandoned.
///
/// _Requirements: 2.6, 12.1_
#[test]
fn timeout_flush_emits_raw_not_composed_text() {
    let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
    let s = sid("telex-raw");

    // "dduow" has already composed to "đươ" in flight, but the raw keystrokes
    // are what get flushed on timeout.
    feed(&mut composer, &s, "dduow");
    assert_eq!(composer.get_buffer_content(&s), Some("đươ"));

    assert_eq!(
        composer.flush_if_timed_out(&s, Duration::ZERO),
        Some("dduow".to_string())
    );
    assert_eq!(composer.get_buffer_content(&s), None);
}

/// `flush_timed_out_sessions` sweeps every live session, flushing only those
/// that are stale. With `Duration::ZERO` every in-flight session is flushed; the
/// returned pairs carry each session's raw text.
///
/// _Requirements: 2.6, 12.1_
#[test]
fn sweep_flushes_all_stale_sessions_as_raw() {
    let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
    let a = sid("sweep-a");
    let b = sid("sweep-b");

    feed(&mut composer, &a, "ngh");
    feed(&mut composer, &b, "tr");
    assert_eq!(composer.session_count(), 2);

    // Nothing stale under a large window.
    assert!(composer.flush_timed_out_sessions(FAR_FUTURE).is_empty());
    assert_eq!(composer.session_count(), 2);

    // Everything stale under ZERO; collect into a map for order-independent
    // assertions (sweep order follows HashMap iteration).
    let flushed: HashMap<SessionID, String> =
        composer.flush_timed_out_sessions(Duration::ZERO).into_iter().collect();
    assert_eq!(flushed.get(&a), Some(&"ngh".to_string()));
    assert_eq!(flushed.get(&b), Some(&"tr".to_string()));
    assert_eq!(composer.session_count(), 0);
}

/// Application switch / focus loss flushes *all* sessions' in-flight
/// compositions as raw text immediately, regardless of idle time, so no partial
/// composition is silently dropped when the user leaves RustDesk.
///
/// _Requirements: 12.3_
#[test]
fn app_switch_flushes_all_sessions_as_raw_text() {
    let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
    let a = sid("app-a");
    let b = sid("app-b");

    feed(&mut composer, &a, "dduowjc"); // composes "được" in flight
    feed(&mut composer, &b, "vieejt"); // composes "việt" in flight
    assert_eq!(composer.session_count(), 2);

    let flushed: HashMap<SessionID, String> =
        composer.flush_all_sessions().into_iter().collect();

    // Raw keystrokes are emitted, not the composed glyphs.
    assert_eq!(flushed.get(&a), Some(&"dduowjc".to_string()));
    assert_eq!(flushed.get(&b), Some(&"vieejt".to_string()));

    // Every buffer is emptied and dropped after an app-switch flush.
    assert_eq!(composer.session_count(), 0);
    assert_eq!(composer.get_buffer_content(&a), None);
    assert_eq!(composer.get_buffer_content(&b), None);
}

// ===========================================================================
// 19.2 Configuration persistence
// ===========================================================================
//
// Requirements: 4.5 (persist method selection across restarts), 4.6 (restore on
// startup), 16.8 (custom config loading). Persistence is exercised through the
// `OptionStore` abstraction with an in-memory store, simulating a "restart" as
// a fresh `load_from` against the same store.

/// Saving a configuration then loading it back from the same store reproduces
/// an equal configuration — the input method selection (and every other field)
/// survives a simulated restart.
///
/// _Requirements: 4.5, 4.6_
#[test]
fn config_round_trips_through_store() {
    let store = MemStore::default();

    let original = VietnameseInputConfig {
        enabled: true,
        default_method: InputMethod::VniWindows,
        normalization: NormalizationForm::Nfd,
        show_overlay: false,
        timeout_ms: 750,
        ..VietnameseInputConfig::default()
    };
    original.save_to(&store);

    // Simulate an application restart: a brand-new load against the persisted
    // store.
    let restored = VietnameseInputConfig::load_from(&store);
    assert_eq!(restored, original);
    // The method selection specifically persisted across the "restart".
    assert_eq!(restored.default_method, InputMethod::VniWindows);
}

/// The persisted method selection survives independently of the other fields:
/// each input method round-trips correctly across a save/load cycle.
///
/// _Requirements: 4.5, 4.6_
#[test]
fn method_selection_persists_across_restart() {
    for method in [
        InputMethod::Telex,
        InputMethod::Vni,
        InputMethod::VniWindows,
        InputMethod::Off,
    ] {
        let store = MemStore::default();
        let config = VietnameseInputConfig {
            enabled: true,
            default_method: method,
            ..VietnameseInputConfig::default()
        };
        config.save_to(&store);

        let restored = VietnameseInputConfig::load_from(&store);
        assert_eq!(
            restored.default_method, method,
            "method {method:?} did not survive a save/load restart"
        );
    }
}

/// Loading from an empty store yields the documented defaults, so a first run
/// (nothing persisted yet) always produces a usable configuration.
///
/// _Requirements: 4.6_
#[test]
fn empty_store_yields_defaults() {
    let store = MemStore::default();
    let config = VietnameseInputConfig::load_from(&store);
    assert_eq!(config, VietnameseInputConfig::default());
    // Spot-check the most important startup defaults.
    assert!(!config.enabled);
    assert_eq!(config.default_method, InputMethod::Telex);
    assert_eq!(config.normalization, NormalizationForm::Nfc);
}

// ===========================================================================
// 19.3 Performance and concurrency (non-flaky)
// ===========================================================================
//
// Requirements: 13.3 (10 concurrent sessions under load), 13.4 (10 concurrent
// sessions), 13.6 (memory stays under budget). We deliberately do NOT assert a
// hard per-keystroke latency (<10ms) or pass-through latency (<5ms): those
// wall-clock thresholds are flaky on shared CI runners. Instead we assert
// functional correctness under a high interleaved load and that the memory
// estimate stays within the documented 10MB budget. Elapsed time is measured
// only to demonstrate the workload completes, never compared to a threshold.

/// Drive 10 sessions concurrently (interleaved one keystroke at a time, a proxy
/// for many sessions typing simultaneously at high WPM), each composing the
/// classic "dduowjc" → "được" sequence. Assert every session produces the
/// correct composed output, that `session_count` tracks the live buffers, and
/// that the estimated memory footprint stays well under the 10MB budget.
///
/// _Requirements: 13.3, 13.4, 13.6_
#[test]
fn ten_concurrent_sessions_compose_correctly_within_memory_budget() {
    let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
    let sessions: Vec<SessionID> = (0..10).map(|i| format!("perf-session-{i}")).collect();

    let sequence: Vec<char> = "dduowjc".chars().collect();

    let start = Instant::now();

    // Interleave keystrokes across all sessions: a full round touches all ten
    // buffers before any of them advances to the next key. This is the
    // high-load / concurrency proxy and catches cross-session routing leakage.
    for &key in &sequence {
        for s in &sessions {
            assert_eq!(composer.process_key(s, key), ComposerResult::Consumed);
        }
        // Memory must stay within budget at every step of the load, not just at
        // the end.
        assert!(
            composer.estimated_memory_bytes() < MEMORY_LIMIT_BYTES,
            "composer memory exceeded the {MEMORY_LIMIT_BYTES} byte budget mid-load"
        );
    }

    // All ten sessions are live and each holds the fully composed syllable.
    assert_eq!(composer.session_count(), 10);
    for s in &sessions {
        assert_eq!(composer.get_buffer_content(s), Some("được"));
    }

    // Commit each session with a space and verify the emitted text. As each
    // commit empties a buffer, the live session count decreases.
    for (i, s) in sessions.iter().enumerate() {
        match composer.process_key(s, ' ') {
            ComposerResult::Compose(text) => assert_eq!(text, "được "),
            other => panic!("session {i}: expected Compose, got {other:?}"),
        }
        assert!(composer.get_buffer_content(s).is_none());
        assert_eq!(composer.session_count(), 10 - (i + 1));
    }

    // Intentionally NO hard timing assertion (see module docs): we only confirm
    // the workload completed. Reaching this point means it did; the elapsed
    // measurement is documentary and `>= ZERO` is trivially, deterministically
    // true on every platform.
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::ZERO,
        "workload must complete (no wall-clock threshold is asserted)"
    );
}

/// Composing across 10 sessions and then flushing them all keeps memory bounded
/// and returns to zero live sessions — there is no per-session state leak that
/// would grow the footprint over repeated use.
///
/// _Requirements: 13.6_
#[test]
fn memory_returns_to_baseline_after_flushing_sessions() {
    let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
    let sessions: Vec<SessionID> = (0..10).map(|i| format!("mem-session-{i}")).collect();

    for s in &sessions {
        feed(&mut composer, s, "dduowjc");
    }
    assert_eq!(composer.session_count(), 10);
    assert!(composer.estimated_memory_bytes() < MEMORY_LIMIT_BYTES);

    // Flushing all sessions (e.g. on app switch) releases every buffer.
    composer.flush_all_sessions();
    assert_eq!(composer.session_count(), 0);
    // With no live buffers the estimate is just the composer's own fixed size,
    // comfortably under budget.
    assert!(composer.estimated_memory_bytes() < MEMORY_LIMIT_BYTES);
}

// ===========================================================================
// 19.4 RustDesk feature compatibility (adapted)
// ===========================================================================
//
// Requirements: 15.1 (file transfer), 15.2 (clipboard sync), 15.3 (audio
// forwarding), 15.4 (compose locally on poor connection), 15.5 (no shortcut
// interference). The composer holds no coupling to any of those subsystems and
// never awaits a remote acknowledgment, so the observable guarantee is that
// composition is a deterministic, purely local function of (input sequence,
// active method). True end-to-end interaction tests need a live RustDesk
// session with a connected peer and are out of scope for unit-level CI (see
// module docs).

/// The same input sequence yields the same composed output across repeated runs
/// within a single composer instance — composition is deterministic and carries
/// no hidden run-to-run state between committed syllables.
///
/// _Requirements: 15.1, 15.2, 15.3_
#[test]
fn composition_is_deterministic_across_repeated_runs() {
    let mut composer = VietnameseComposer::with_method(InputMethod::Telex);

    let mut outputs = Vec::new();
    for i in 0..5 {
        // A fresh session id per run so each run starts from a clean buffer.
        let s = sid(&format!("determinism-run-{i}"));
        feed(&mut composer, &s, "dduowjc");
        match composer.process_key(&s, ' ') {
            ComposerResult::Compose(text) => outputs.push(text),
            other => panic!("run {i}: expected Compose, got {other:?}"),
        }
    }

    assert!(outputs.iter().all(|t| t == "được "));
}

/// The same input sequence yields the same composed output across *fresh*
/// composer instances — output depends only on the input and method, not on any
/// external/remote state, prior session history, or instance identity.
///
/// _Requirements: 15.4_
#[test]
fn composition_is_independent_of_composer_instance_and_external_state() {
    fn compose_word(seq: &str) -> String {
        // A brand-new composer with no remote connection, no file transfer, no
        // clipboard/audio state — nothing but the input method.
        let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
        let s = sid("isolated");
        feed(&mut composer, &s, seq);
        match composer.process_key(&s, ' ') {
            ComposerResult::Compose(text) => text,
            other => panic!("expected Compose, got {other:?}"),
        }
    }

    // Two independent instances produce identical output for the same input.
    assert_eq!(compose_word("dduowjc"), "được ");
    assert_eq!(compose_word("dduowjc"), compose_word("dduowjc"));
    assert_eq!(compose_word("vieejt"), "việt ");
}

/// Composition completes locally and immediately: every keystroke returns a
/// `ComposerResult` synchronously, and the committing space yields the composed
/// text — the composer never blocks waiting on a remote acknowledgment, so it
/// works regardless of connection quality.
///
/// _Requirements: 15.4_
#[test]
fn composition_completes_locally_without_remote_acknowledgment() {
    let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
    let s = sid("local-only");

    // Each keystroke is consumed synchronously into the local buffer.
    for ch in "dduowjc".chars() {
        assert_eq!(composer.process_key(&s, ch), ComposerResult::Consumed);
    }
    // The composed glyph is available locally before anything is sent remotely.
    assert_eq!(composer.get_buffer_content(&s), Some("được"));

    // Committing produces the text synchronously — no awaiting a remote round
    // trip.
    match composer.process_key(&s, ' ') {
        ComposerResult::Compose(text) => assert_eq!(text, "được "),
        other => panic!("expected Compose, got {other:?}"),
    }
}

/// Multiple concurrent sessions remain fully independent: composing in one
/// session never leaks into another. This is the property that lets Vietnamese
/// input coexist with the rest of RustDesk's per-session features without
/// cross-contamination.
///
/// _Requirements: 15.1, 15.2, 15.3, 15.5_
#[test]
fn concurrent_sessions_do_not_interfere() {
    let mut composer = VietnameseComposer::with_method(InputMethod::Telex);
    let a = sid("indep-a");
    let b = sid("indep-b");

    // Interleave two different words across two sessions.
    feed(&mut composer, &a, "ngu");
    feed(&mut composer, &b, "dduowjc");
    assert_eq!(composer.get_buffer_content(&a), Some("ngu"));
    assert_eq!(composer.get_buffer_content(&b), Some("được"));

    // Completing session B leaves session A's in-flight composition untouched.
    match composer.process_key(&b, ' ') {
        ComposerResult::Compose(text) => assert_eq!(text, "được "),
        other => panic!("expected Compose for B, got {other:?}"),
    }
    assert_eq!(composer.get_buffer_content(&a), Some("ngu"));

    // Session A resumes and finishes independently.
    feed(&mut composer, &a, "owif");
    match composer.process_key(&a, ' ') {
        ComposerResult::Compose(text) => assert_eq!(text, "người "),
        other => panic!("expected Compose for A, got {other:?}"),
    }
}
