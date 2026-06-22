import 'package:flutter/material.dart';
import 'package:flutter_hbb/models/vietnamese_input_model.dart';
import 'package:provider/provider.dart';

import '../../common.dart';

/// Visual feedback for Vietnamese input method switching.
///
/// This file provides two pieces of UI feedback required when the active
/// Vietnamese input method changes (Requirements 4.3, 4.9, 9.3, 10.10):
///
/// * [showInputMethodToast] — a transient toast notification fired whenever the
///   method changes (selector pick, cycle shortcut, or on/off toggle). It
///   reuses RustDesk's global [showToast] helper so it picks up the existing
///   toast theme and overlay plumbing, and it displays for
///   [_methodToastDuration] (2 seconds) per Requirement 9.3.
/// * [InputMethodIndicator] — a compact, always-on status label that mirrors
///   the currently active method name during a remote session
///   (Requirement 4.9). It is a thin reactive view over [VietnameseInputModel].
///
/// Mounting / wiring notes:
/// * Mount [InputMethodIndicator] in the remote session toolbar / status bar
///   (e.g. the desktop remote page's tab-bar action row or the mobile remote
///   action sheet), passing the page's [VietnameseInputModel]. It rebuilds
///   itself whenever the method changes.
/// * Call [showInputMethodToast] from wherever the method actually changes. The
///   simplest hook is in response to [VietnameseInputModel] notifications: when
///   [VietnameseInputModel.activeMethod] (or [VietnameseInputModel.enabled])
///   changes, call `showInputMethodToast(model.activeMethod)`. The keyboard
///   shortcut handlers (Ctrl+Shift+V toggle, Ctrl+Shift+I cycle) should also
///   call it directly so the user gets feedback even when the change originates
///   from a shortcut rather than the settings UI.

/// How long the method-change toast stays on screen.
///
/// Requirement 9.3 mandates the indicator is shown for 2 seconds after a
/// toggle-shortcut press; the same duration is used for every method change so
/// the feedback is consistent.
const Duration _methodToastDuration = Duration(seconds: 2);

/// Build the human-readable toast message for a method change.
///
/// Exposed (rather than inlined) so it can be unit-tested without an overlay:
/// * [InputMethod.off] -> `"Vietnamese input: Off"`
/// * any other method -> `"Vietnamese: <displayName>"` (e.g. `"Vietnamese: Telex"`)
String inputMethodToastMessage(InputMethod method) {
  if (method == InputMethod.off) {
    return 'Vietnamese input: Off';
  }
  return 'Vietnamese: ${method.displayName}';
}

/// Show a transient toast announcing the newly active input [method].
///
/// Fired on method change (Requirement 4.3) and on toggle-shortcut press
/// (Requirement 9.3). Displays for [_methodToastDuration] (2 seconds) using the
/// shared [showToast] helper, so it inherits the app-wide toast styling and the
/// global overlay (no [BuildContext] required at the call site).
void showInputMethodToast(InputMethod method) {
  showToast(inputMethodToastMessage(method), timeout: _methodToastDuration);
}

/// Compact status label showing the active Vietnamese input method.
///
/// Intended for the remote session toolbar / status bar so the user can always
/// see which method is live (Requirement 4.9). It is a passive, reactive view:
/// it listens to [model] and rebuilds whenever the active method changes.
///
/// Rendering:
/// * Shows a small keyboard glyph followed by `"VN: <displayName>"`.
/// * When the method is [InputMethod.off] the label dims (reduced opacity) to
///   signal Vietnamese composition is inactive, while still reporting the
///   state so the indicator never silently disappears.
class InputMethodIndicator extends StatelessWidget {
  const InputMethodIndicator({
    Key? key,
    required this.model,
    this.compact = false,
  }) : super(key: key);

  /// Reactive Vietnamese input state. The indicator rebuilds whenever the
  /// active method (or enabled flag) changes.
  final VietnameseInputModel model;

  /// When true, render only the method [InputMethod.displayName] (no `"VN:"`
  /// prefix) to fit tighter toolbars.
  final bool compact;

  @override
  Widget build(BuildContext context) {
    return ChangeNotifierProvider.value(
      value: model,
      child: Consumer<VietnameseInputModel>(
        builder: (context, model, child) {
          final method = model.activeMethod;
          final isOff = method == InputMethod.off;
          final label =
              compact ? method.displayName : 'VN: ${method.displayName}';
          final textColor = Theme.of(context).textTheme.titleMedium?.color;

          return Tooltip(
            message: inputMethodToastMessage(method),
            child: Opacity(
              // Dim when inactive so the active state reads at a glance without
              // hiding the indicator entirely.
              opacity: isOff ? 0.5 : 1.0,
              child: Container(
                padding:
                    const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
                decoration: BoxDecoration(
                  color: MyTheme.accent.withAlpha(isOff ? 40 : 90),
                  borderRadius: BorderRadius.circular(4),
                ),
                child: Row(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Icon(
                      Icons.keyboard,
                      size: 16,
                      color: textColor,
                    ),
                    const SizedBox(width: 4),
                    Text(
                      label,
                      style: TextStyle(
                        fontSize: 13,
                        fontWeight: FontWeight.w500,
                        color: textColor,
                        decoration: TextDecoration.none,
                      ),
                    ),
                  ],
                ),
              ),
            ),
          );
        },
      ),
    );
  }
}
