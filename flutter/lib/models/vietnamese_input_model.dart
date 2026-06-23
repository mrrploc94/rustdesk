import 'package:flutter/material.dart';
import 'package:flutter_hbb/models/platform_model.dart';

/// Vietnamese input methods supported by the composer.
///
/// The string [identifier] of each method MUST stay in sync with the Rust-side
/// `parse_method` implementation (see `src/vietnamese_input/`). The Rust core
/// matches on these exact identifiers when `vietnamese_input_set_method` is
/// invoked through the generated FFI bridge.
enum InputMethod {
  telex,
  vni,
  vniWindows,
  off,
}

extension InputMethodIdentifier on InputMethod {
  /// Stable identifier exchanged with the Rust core. Matches `parse_method`.
  String get identifier {
    switch (this) {
      case InputMethod.telex:
        return 'telex';
      case InputMethod.vni:
        return 'vni';
      case InputMethod.vniWindows:
        return 'vni_windows';
      case InputMethod.off:
        return 'off';
    }
  }

  /// Human readable label used by the UI (toasts, selectors, status bar).
  String get displayName {
    switch (this) {
      case InputMethod.telex:
        return 'Telex';
      case InputMethod.vni:
        return 'VNI';
      case InputMethod.vniWindows:
        return 'VNI Windows';
      case InputMethod.off:
        return 'Off';
    }
  }

  /// Parse an [InputMethod] from its Rust-side [identifier]. Falls back to
  /// [InputMethod.off] for unknown/empty values so the UI never crashes on a
  /// malformed bridge response.
  static InputMethod fromIdentifier(String? value) {
    switch (value) {
      case 'telex':
        return InputMethod.telex;
      case 'vni':
        return InputMethod.vni;
      case 'vni_windows':
        return InputMethod.vniWindows;
      case 'off':
      default:
        return InputMethod.off;
    }
  }
}

/// Flutter-side state for the Vietnamese input composer.
///
/// This model is a thin reactive mirror of the authoritative state that lives
/// in the Rust core. UI widgets (composition overlay, settings page, status
/// indicators) listen to this model, while every state mutation is forwarded to
/// the Rust core through the generated `flutter_rust_bridge` bindings exposed on
/// [bind].
///
/// The generated binding names mirror the Rust FFI functions added in the core
/// (`vietnamese_input_set_enabled`, `vietnamese_input_is_enabled`,
/// `vietnamese_input_set_method`, `vietnamese_input_get_composition`). They are
/// generated at build time, so they are referenced here through [bind] in the
/// same style as every other FFI call in the codebase
/// (e.g. `bind.mainGetLocalOption(...)`).
class VietnameseInputModel extends ChangeNotifier {
  InputMethod _activeMethod = InputMethod.off;
  String _compositionPreview = '';
  bool _isComposing = false;
  bool _enabled = false;

  /// Currently active input method.
  InputMethod get activeMethod => _activeMethod;

  /// Text currently being composed (shown in the composition overlay).
  String get compositionPreview => _compositionPreview;

  /// Whether a composition is in progress (buffer is non-empty).
  bool get isComposing => _isComposing;

  /// Whether Vietnamese input is enabled at all.
  bool get enabled => _enabled;

  /// Enable or disable Vietnamese input.
  ///
  /// Updates local state immediately for a responsive UI, notifies listeners,
  /// then forwards the change to the Rust core.
  Future<void> setEnabled(bool enabled) async {
    if (_enabled != enabled) {
      _enabled = enabled;
      notifyListeners();
    }
    await bind.vietnameseInputSetEnabled(enabled: enabled);
  }

  /// Switch the active input method.
  ///
  /// Updates local state immediately, notifies listeners, then forwards the
  /// selection to the Rust core so subsequent keystrokes are routed to the
  /// matching engine.
  Future<void> setMethod(InputMethod method) async {
    if (_activeMethod != method) {
      _activeMethod = method;
      notifyListeners();
    }
    await bind.vietnameseInputSetMethod(method: method.identifier);
  }

  /// Update the composition preview shown in the overlay.
  ///
  /// Driven by composition-update events streamed from the Rust core. An empty
  /// [text] means no composition is in progress.
  void updatePreview(String text) {
    if (_compositionPreview == text && _isComposing == text.isNotEmpty) {
      return;
    }
    _compositionPreview = text;
    _isComposing = text.isNotEmpty;
    notifyListeners();
  }

  /// Clear the composition preview (called after a commit/flush or on session
  /// close) and hide the overlay.
  void clearPreview() {
    if (_compositionPreview.isEmpty && !_isComposing) {
      return;
    }
    _compositionPreview = '';
    _isComposing = false;
    notifyListeners();
  }

  /// Synchronise local state with the authoritative Rust core state.
  ///
  /// Useful on startup or when (re)attaching to a session so the UI reflects the
  /// persisted enabled flag and any in-flight composition.
  Future<void> syncFromBridge(String sessionId) async {
    final enabled = await bind.vietnameseInputIsEnabled();
    if (_enabled != enabled) {
      _enabled = enabled;
    }
    // The composer is process-global; no session id is forwarded. See the
    // Rust `vietnamese_input_get_composition` doc comment for why the bridge
    // function takes no `session_id` parameter (frb type-unification bug).
    final composition = await bind.vietnameseInputGetComposition();
    _compositionPreview = composition;
    _isComposing = composition.isNotEmpty;
    notifyListeners();
  }
}
