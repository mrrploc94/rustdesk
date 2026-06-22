//! Configuration schema and persistence for the Vietnamese input composer.
//!
//! [`VietnameseInputConfig`] is the single, serializable source of truth for
//! the composer's user-facing settings: whether the feature is enabled, the
//! default input method, Unicode normalization form, the toggle/cycle keyboard
//! shortcuts, overlay visibility, an optional custom-rules directory, and the
//! idle-flush timeout.
//!
//! ## Persistence
//!
//! Settings are persisted through RustDesk's existing key/value option store
//! (`crate::ui_interface::get_option` / `set_option`), the same mechanism every
//! other RustDesk preference uses. Each field maps to a stable `vietnamese-input-*`
//! option key (see the [`keys`] module). Persistence is abstracted behind the
//! [`OptionStore`] trait so the load/save logic can be unit-tested with an
//! in-memory store without touching process-wide state.
//!
//! ## Activation
//!
//! [`VietnameseInputConfig::apply`] pushes a loaded configuration into the
//! [`integration`](super::integration) layer (method, normalization, shortcuts,
//! enabled gate), so restoring persisted settings on startup is a single
//! [`load_and_apply`](VietnameseInputConfig::load_and_apply) call. This is what
//! satisfies "restore the persisted input method selection on startup"
//! (requirements 4.5, 4.6).
//!
//! _Requirements: 4.5, 4.6, 5.2, 8.1, 8.2, 8.3, 8.4, 8.5, 8.7, 9.4_

use std::path::PathBuf;

use super::integration::{self, Chord};
use super::{InputMethod, NormalizationForm};

/// A keyboard shortcut binding.
///
/// The composer's keyboard shortcuts are modeled by [`Chord`] (a set of
/// Ctrl/Shift/Alt/Meta modifiers plus a trigger key) in the
/// [`integration`](super::integration) layer. To avoid duplicating that type,
/// `KeyShortcut` is an alias for [`Chord`]; the design document's
/// "`KeyShortcut { modifiers, key }`" shape is represented by `Chord`'s
/// individual modifier flags plus its `key`.
pub type KeyShortcut = Chord;

/// Option-store keys under which each configuration field is persisted.
///
/// Kept as a dedicated module of `&str` constants so the persisted key names
/// are defined exactly once and shared between [`VietnameseInputConfig::load_from`]
/// and [`VietnameseInputConfig::save_to`].
pub mod keys {
    /// Whether Vietnamese input is enabled (`"Y"` / empty).
    pub const ENABLED: &str = "vietnamese-input-enabled";
    /// Default input method identifier (`"telex"`, `"vni"`, `"vni_windows"`, `"off"`).
    pub const METHOD: &str = "vietnamese-input-method";
    /// Unicode normalization form (`"nfc"` / `"nfd"`).
    pub const NORMALIZATION: &str = "vietnamese-input-normalization";
    /// Toggle shortcut chord (e.g. `"ctrl+shift+v"`).
    pub const TOGGLE_SHORTCUT: &str = "vietnamese-input-toggle-shortcut";
    /// Cycle shortcut chord (e.g. `"ctrl+shift+i"`).
    pub const CYCLE_SHORTCUT: &str = "vietnamese-input-cycle-shortcut";
    /// Whether the composition overlay is shown (`"Y"` / `"N"`).
    pub const SHOW_OVERLAY: &str = "vietnamese-input-show-overlay";
    /// Optional custom-rules directory (filesystem path; empty = none).
    pub const CUSTOM_RULES_DIR: &str = "vietnamese-input-custom-rules-dir";
    /// Idle-flush timeout in milliseconds (e.g. `"500"`).
    pub const TIMEOUT_MS: &str = "vietnamese-input-timeout-ms";
}

/// Default idle-flush timeout in milliseconds (requirements 2.6, 12.1).
pub const DEFAULT_TIMEOUT_MS: u64 = 500;

/// User-facing configuration for the Vietnamese input composer.
///
/// All fields have sensible defaults (see [`Default`]) so an absent or partial
/// persisted configuration always yields a usable composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VietnameseInputConfig {
    /// Whether Vietnamese input composition is enabled. Defaults to `false` so
    /// the keyboard pipeline is unchanged until the user opts in.
    pub enabled: bool,
    /// The input method activated on startup (requirement 4.5/4.6).
    pub default_method: InputMethod,
    /// Unicode normalization form applied to composed output (requirement 5.2).
    pub normalization: NormalizationForm,
    /// Chord that toggles Vietnamese input on/off (default Ctrl+Shift+V).
    pub toggle_shortcut: KeyShortcut,
    /// Chord that cycles through the input methods (default Ctrl+Shift+I).
    pub cycle_shortcut: KeyShortcut,
    /// Whether the composition overlay is shown (requirement 14.4).
    pub show_overlay: bool,
    /// Optional directory of custom input-method rule files (requirement 16.8).
    pub custom_rules_dir: Option<PathBuf>,
    /// Idle timeout (ms) after which an incomplete/invalid sequence is flushed.
    pub timeout_ms: u64,
}

impl Default for VietnameseInputConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_method: InputMethod::Telex,
            normalization: NormalizationForm::Nfc,
            // Requirement 9.1 / 9.2 defaults, mirroring `ShortcutConfig::default`.
            toggle_shortcut: Chord::ctrl_shift('v'),
            cycle_shortcut: Chord::ctrl_shift('i'),
            show_overlay: true,
            custom_rules_dir: None,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

/// Abstraction over RustDesk's persisted key/value option store.
///
/// Implemented by [`RustDeskOptionStore`] in production (backed by
/// `crate::ui_interface`) and by in-memory stores in tests, so the load/save
/// logic is fully unit-testable without process-wide side effects.
pub trait OptionStore {
    /// Return the value for `key`, or an empty string when unset.
    fn get(&self, key: &str) -> String;
    /// Persist `value` under `key`. An empty `value` clears the key.
    fn set(&self, key: &str, value: &str);
}

/// Production [`OptionStore`] backed by RustDesk's `ui_interface` option store
/// (the same `get_option`/`set_option` every other preference uses).
pub struct RustDeskOptionStore;

impl OptionStore for RustDeskOptionStore {
    fn get(&self, key: &str) -> String {
        crate::ui_interface::get_option(key)
    }

    fn set(&self, key: &str, value: &str) {
        crate::ui_interface::set_option(key.to_string(), value.to_string());
    }
}

impl VietnameseInputConfig {
    /// Load the configuration from RustDesk's persisted option store, falling
    /// back to defaults for any unset field.
    pub fn load() -> Self {
        Self::load_from(&RustDeskOptionStore)
    }

    /// Persist this configuration to RustDesk's option store.
    pub fn save(&self) {
        self.save_to(&RustDeskOptionStore);
    }

    /// Load the configuration, then [`apply`](Self::apply) it to the composer.
    ///
    /// This is the single startup entry point: it restores the persisted input
    /// method, normalization, shortcuts, and enabled state in one call
    /// (requirements 4.5, 4.6). Returns the loaded configuration so callers can
    /// reflect it in the UI.
    pub fn load_and_apply() -> Self {
        let config = Self::load();
        config.apply();
        config
    }

    /// Load the configuration from an arbitrary [`OptionStore`].
    ///
    /// Every field falls back to its [`Default`] when the corresponding option
    /// is absent or unparseable, so a missing or partially-written store always
    /// yields a usable configuration.
    pub fn load_from(store: &impl OptionStore) -> Self {
        let defaults = Self::default();

        let enabled = parse_bool(&store.get(keys::ENABLED), defaults.enabled);

        let default_method = integration::parse_method(&store.get(keys::METHOD))
            .unwrap_or(defaults.default_method);

        let normalization = parse_normalization(&store.get(keys::NORMALIZATION))
            .unwrap_or(defaults.normalization);

        let toggle_shortcut =
            parse_chord(&store.get(keys::TOGGLE_SHORTCUT)).unwrap_or(defaults.toggle_shortcut);

        let cycle_shortcut =
            parse_chord(&store.get(keys::CYCLE_SHORTCUT)).unwrap_or(defaults.cycle_shortcut);

        let show_overlay = parse_bool(&store.get(keys::SHOW_OVERLAY), defaults.show_overlay);

        let custom_rules_dir = {
            let raw = store.get(keys::CUSTOM_RULES_DIR);
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(PathBuf::from(trimmed))
            }
        };

        let timeout_ms = {
            let raw = store.get(keys::TIMEOUT_MS);
            raw.trim().parse::<u64>().unwrap_or(defaults.timeout_ms)
        };

        Self {
            enabled,
            default_method,
            normalization,
            toggle_shortcut,
            cycle_shortcut,
            show_overlay,
            custom_rules_dir,
            timeout_ms,
        }
    }

    /// Persist this configuration to an arbitrary [`OptionStore`].
    ///
    /// The inverse of [`load_from`](Self::load_from): writing then reading back
    /// through the same store reproduces an equal configuration.
    pub fn save_to(&self, store: &impl OptionStore) {
        store.set(keys::ENABLED, bool_to_str(self.enabled));
        store.set(keys::METHOD, method_to_str(self.default_method));
        store.set(keys::NORMALIZATION, normalization_to_str(self.normalization));
        store.set(keys::TOGGLE_SHORTCUT, &chord_to_string(&self.toggle_shortcut));
        store.set(keys::CYCLE_SHORTCUT, &chord_to_string(&self.cycle_shortcut));
        // Overlay uses explicit "Y"/"N" so the persisted value distinguishes a
        // user-chosen "off" from an unset key (which falls back to the default).
        store.set(keys::SHOW_OVERLAY, if self.show_overlay { "Y" } else { "N" });
        store.set(
            keys::CUSTOM_RULES_DIR,
            &self
                .custom_rules_dir
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        store.set(keys::TIMEOUT_MS, &self.timeout_ms.to_string());
    }

    /// Push this configuration into the [`integration`](super::integration)
    /// layer, activating it on the global composer.
    ///
    /// Method, normalization, and shortcuts are applied first, then the enabled
    /// gate is set last so the feature only goes live once the composer is fully
    /// configured. This is what makes [`load_and_apply`](Self::load_and_apply)
    /// restore the persisted method on startup.
    ///
    /// _Requirements: 4.5, 4.6, 5.2, 8.7, 9.4_
    pub fn apply(&self) {
        integration::set_method(self.default_method);
        integration::set_normalization(self.normalization);
        integration::set_toggle_shortcut(self.toggle_shortcut);
        integration::set_cycle_shortcut(self.cycle_shortcut);
        integration::set_enabled(self.enabled);
    }
}

// ---------------------------------------------------------------------------
// Field <-> string conversions
// ---------------------------------------------------------------------------

/// Parse a persisted boolean. RustDesk's convention is `"Y"` for true; this
/// also accepts a few common spellings and falls back to `default` for unset or
/// unrecognized values.
fn parse_bool(raw: &str, default: bool) -> bool {
    match raw.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" | "true" | "1" | "on" => true,
        "n" | "no" | "false" | "0" | "off" => false,
        "" => default,
        _ => default,
    }
}

/// Render a boolean for persistence using RustDesk's `"Y"` / empty convention.
fn bool_to_str(value: bool) -> &'static str {
    if value {
        "Y"
    } else {
        ""
    }
}

/// Canonical lowercase identifier for an [`InputMethod`], matching the
/// identifiers accepted by [`integration::parse_method`].
fn method_to_str(method: InputMethod) -> &'static str {
    match method {
        InputMethod::Telex => "telex",
        InputMethod::Vni => "vni",
        InputMethod::VniWindows => "vni_windows",
        InputMethod::Off => "off",
    }
}

/// Parse a persisted normalization identifier (`"nfc"` / `"nfd"`).
fn parse_normalization(raw: &str) -> Option<NormalizationForm> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "nfc" => Some(NormalizationForm::Nfc),
        "nfd" => Some(NormalizationForm::Nfd),
        _ => None,
    }
}

/// Canonical lowercase identifier for a [`NormalizationForm`].
fn normalization_to_str(form: NormalizationForm) -> &'static str {
    match form {
        NormalizationForm::Nfc => "nfc",
        NormalizationForm::Nfd => "nfd",
    }
}

/// Serialize a [`Chord`] into a `"+"`-joined modifier/key string, e.g.
/// `"ctrl+shift+v"`. Modifiers are emitted in a fixed order so the output is
/// stable and round-trips through [`parse_chord`].
fn chord_to_string(chord: &Chord) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if chord.ctrl {
        parts.push("ctrl");
    }
    if chord.shift {
        parts.push("shift");
    }
    if chord.alt {
        parts.push("alt");
    }
    if chord.meta {
        parts.push("meta");
    }
    let key = chord.key.to_string();
    parts.push(&key);
    parts.join("+")
}

/// Parse a `"+"`-joined chord string (e.g. `"Ctrl+Shift+V"`) into a [`Chord`].
///
/// Matching is case-insensitive and tolerant of surrounding whitespace. The
/// final non-modifier token is the trigger key; common modifier aliases
/// (`control`, `cmd`, `win`, `super`, `option`) are accepted. Returns `None`
/// when the string has no trigger key or contains an unrecognized token.
fn parse_chord(raw: &str) -> Option<Chord> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut meta = false;
    let mut key: Option<char> = None;

    for token in trimmed.split('+') {
        let token = token.trim().to_ascii_lowercase();
        if token.is_empty() {
            continue;
        }
        match token.as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" => alt = true,
            "meta" | "cmd" | "command" | "win" | "super" => meta = true,
            other => {
                let mut chars = other.chars();
                match (chars.next(), chars.next()) {
                    // Exactly one character: this is the trigger key. A second
                    // trigger key makes the chord ambiguous, so reject it.
                    (Some(c), None) => {
                        if key.is_some() {
                            return None;
                        }
                        key = Some(c);
                    }
                    _ => return None,
                }
            }
        }
    }

    key.map(|k| Chord::new(ctrl, shift, alt, meta, k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// In-memory [`OptionStore`] for deterministic, side-effect-free tests.
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
                self.map.borrow_mut().insert(key.to_string(), value.to_string());
            }
        }
    }

    #[test]
    fn default_config_has_expected_values() {
        let config = VietnameseInputConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.default_method, InputMethod::Telex);
        assert_eq!(config.normalization, NormalizationForm::Nfc);
        assert_eq!(config.toggle_shortcut, Chord::ctrl_shift('v'));
        assert_eq!(config.cycle_shortcut, Chord::ctrl_shift('i'));
        assert!(config.show_overlay);
        assert_eq!(config.custom_rules_dir, None);
        assert_eq!(config.timeout_ms, DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn load_from_empty_store_yields_defaults() {
        let store = MemStore::default();
        let config = VietnameseInputConfig::load_from(&store);
        assert_eq!(config, VietnameseInputConfig::default());
    }

    #[test]
    fn save_then_load_round_trips_default() {
        let store = MemStore::default();
        let original = VietnameseInputConfig::default();
        original.save_to(&store);
        let loaded = VietnameseInputConfig::load_from(&store);
        assert_eq!(loaded, original);
    }

    #[test]
    fn save_then_load_round_trips_non_default() {
        let store = MemStore::default();
        let original = VietnameseInputConfig {
            enabled: true,
            default_method: InputMethod::VniWindows,
            normalization: NormalizationForm::Nfd,
            toggle_shortcut: Chord::new(true, false, true, false, 'k'),
            cycle_shortcut: Chord::new(true, true, false, true, 'm'),
            show_overlay: false,
            custom_rules_dir: Some(PathBuf::from("/tmp/vn-rules")),
            timeout_ms: 750,
        };
        original.save_to(&store);
        let loaded = VietnameseInputConfig::load_from(&store);
        assert_eq!(loaded, original);
    }

    #[test]
    fn load_reads_each_persisted_field() {
        let store = MemStore::default();
        store.set(keys::ENABLED, "Y");
        store.set(keys::METHOD, "vni");
        store.set(keys::NORMALIZATION, "nfd");
        store.set(keys::TOGGLE_SHORTCUT, "ctrl+shift+x");
        store.set(keys::CYCLE_SHORTCUT, "alt+y");
        store.set(keys::SHOW_OVERLAY, "N");
        store.set(keys::CUSTOM_RULES_DIR, "/opt/rules");
        store.set(keys::TIMEOUT_MS, "1200");

        let config = VietnameseInputConfig::load_from(&store);
        assert!(config.enabled);
        assert_eq!(config.default_method, InputMethod::Vni);
        assert_eq!(config.normalization, NormalizationForm::Nfd);
        assert_eq!(config.toggle_shortcut, Chord::ctrl_shift('x'));
        assert_eq!(config.cycle_shortcut, Chord::new(false, false, true, false, 'y'));
        assert!(!config.show_overlay);
        assert_eq!(config.custom_rules_dir, Some(PathBuf::from("/opt/rules")));
        assert_eq!(config.timeout_ms, 1200);
    }

    #[test]
    fn unparseable_fields_fall_back_to_defaults() {
        let store = MemStore::default();
        store.set(keys::METHOD, "not-a-method");
        store.set(keys::NORMALIZATION, "nfx");
        store.set(keys::TIMEOUT_MS, "abc");
        store.set(keys::TOGGLE_SHORTCUT, "ctrl+shift"); // no trigger key

        let config = VietnameseInputConfig::load_from(&store);
        let defaults = VietnameseInputConfig::default();
        assert_eq!(config.default_method, defaults.default_method);
        assert_eq!(config.normalization, defaults.normalization);
        assert_eq!(config.timeout_ms, defaults.timeout_ms);
        assert_eq!(config.toggle_shortcut, defaults.toggle_shortcut);
    }

    #[test]
    fn method_string_round_trips_all_variants() {
        for method in [
            InputMethod::Telex,
            InputMethod::Vni,
            InputMethod::VniWindows,
            InputMethod::Off,
        ] {
            let s = method_to_str(method);
            assert_eq!(integration::parse_method(s), Some(method));
        }
    }

    #[test]
    fn chord_string_round_trips() {
        let chords = [
            Chord::ctrl_shift('v'),
            Chord::ctrl_shift('i'),
            Chord::new(true, false, false, false, 'a'),
            Chord::new(false, true, true, false, 'z'),
            Chord::new(true, true, true, true, 'q'),
            Chord::new(false, false, false, true, 'm'),
        ];
        for chord in chords {
            let s = chord_to_string(&chord);
            assert_eq!(parse_chord(&s), Some(chord), "round-trip failed for {s}");
        }
    }

    #[test]
    fn parse_chord_is_case_insensitive_and_trimmed() {
        assert_eq!(parse_chord("  Ctrl + Shift + V "), Some(Chord::ctrl_shift('v')));
        assert_eq!(parse_chord("CONTROL+SHIFT+I"), Some(Chord::ctrl_shift('i')));
    }

    #[test]
    fn parse_chord_rejects_missing_key() {
        assert_eq!(parse_chord("ctrl+shift"), None);
        assert_eq!(parse_chord(""), None);
        assert_eq!(parse_chord("   "), None);
    }

    #[test]
    fn parse_bool_accepts_common_spellings() {
        assert!(parse_bool("Y", false));
        assert!(parse_bool("true", false));
        assert!(parse_bool("1", false));
        assert!(!parse_bool("N", true));
        assert!(!parse_bool("false", true));
        // Unset / unrecognized falls back to the provided default.
        assert!(parse_bool("", true));
        assert!(!parse_bool("", false));
        assert!(parse_bool("garbage", true));
    }
}
