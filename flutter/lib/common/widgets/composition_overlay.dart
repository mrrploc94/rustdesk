import 'package:flutter/material.dart';
import 'package:flutter_hbb/models/vietnamese_input_model.dart';
import 'package:provider/provider.dart';

import '../../common.dart';

/// Floating overlay that shows the in-progress Vietnamese composition buffer
/// near the remote cursor.
///
/// The widget is a thin reactive view over [VietnameseInputModel]. It renders a
/// small highlighted container with the text currently being composed
/// ([VietnameseInputModel.compositionPreview]) while a composition is active
/// ([VietnameseInputModel.isComposing]) and the overlay is enabled.
///
/// Positioning: precise cursor-following requires integration with the remote
/// view (the cursor position is owned by the remote canvas). To keep this
/// widget self-contained and testable, it accepts a [position] offset and wraps
/// itself in a [Positioned] so the mount point decides where it lives. The
/// widget that embeds this overlay (the remote page) is responsible for feeding
/// the live cursor offset into [position]; see the desktop/mobile remote pages
/// for where the overlay is mounted into the canvas [Stack].
///
/// Auto-hide: when [VietnameseInputModel.isComposing] flips to false after a
/// commit/flush, the overlay fades out within [_autoHideDuration] (100ms) via
/// [AnimatedOpacity] so a committed character does not leave a lingering hint.
class CompositionOverlay extends StatelessWidget {
  const CompositionOverlay({
    Key? key,
    required this.model,
    this.position = Offset.zero,
    this.showOverlay = true,
    this.maxChars = _maxChars,
  }) : super(key: key);

  /// Reactive Vietnamese input state. The overlay listens to this model and
  /// rebuilds whenever the composition preview changes.
  final VietnameseInputModel model;

  /// Screen-space offset (relative to the enclosing [Stack]) where the overlay
  /// should be anchored. Typically the remote cursor position. A small vertical
  /// offset is added so the hint floats just below the caret rather than
  /// covering it.
  final Offset position;

  /// Master enable/disable switch (driven by config `overlay.enabled`). When
  /// false the overlay is never rendered, regardless of composition state.
  final bool showOverlay;

  /// Maximum number of characters to display before truncating with an
  /// ellipsis. Defaults to 20 to match the composition buffer capacity.
  final int maxChars;

  /// Buffer capacity / default display cap (20 characters).
  static const int _maxChars = 20;

  /// Auto-hide fade duration after a commit (must be <= 100ms per spec).
  static const Duration _autoHideDuration = Duration(milliseconds: 100);

  /// Vertical gap between the anchor (cursor) and the overlay so the hint does
  /// not sit directly on top of the caret.
  static const double _cursorVerticalGap = 20.0;

  /// Clamp [text] to at most [maxChars] characters, appending an ellipsis when
  /// the composition is longer than the visible window.
  String _truncate(String text) {
    if (text.length <= maxChars) {
      return text;
    }
    // Keep the most recent characters visible (the tail is where typing
    // happens) and mark the elision at the front.
    return '…${text.substring(text.length - maxChars)}';
  }

  @override
  Widget build(BuildContext context) {
    // Overlay disabled by configuration: render nothing at all.
    if (!showOverlay) {
      return const SizedBox.shrink();
    }

    return ChangeNotifierProvider.value(
      value: model,
      child: Consumer<VietnameseInputModel>(
        builder: (context, model, child) {
          final composing = model.isComposing;
          final text = _truncate(model.compositionPreview);

          return Positioned(
            left: position.dx,
            top: position.dy + _cursorVerticalGap,
            child: IgnorePointer(
              // The overlay is a passive hint; it must never intercept pointer
              // events meant for the remote canvas.
              child: AnimatedOpacity(
                opacity: composing && text.isNotEmpty ? 1.0 : 0.0,
                duration: _autoHideDuration,
                curve: Curves.easeOut,
                child: _CompositionBubble(text: text),
              ),
            ),
          );
        },
      ),
    );
  }
}

/// The visual bubble that renders the composition text with a distinct,
/// semi-transparent highlight background so it stands out over the remote
/// canvas content.
class _CompositionBubble extends StatelessWidget {
  const _CompositionBubble({Key? key, required this.text}) : super(key: key);

  final String text;

  @override
  Widget build(BuildContext context) {
    return Material(
      type: MaterialType.transparency,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
        decoration: BoxDecoration(
          // Distinct highlight: accent-tinted, semi-transparent so underlying
          // content stays faintly visible.
          color: MyTheme.accent.withAlpha(220),
          borderRadius: BorderRadius.circular(4),
          border: Border.all(
            color: Colors.white.withAlpha(180),
            width: 1,
          ),
          boxShadow: [
            BoxShadow(
              color: Colors.black.withAlpha(80),
              blurRadius: 4,
              offset: const Offset(0, 1),
            ),
          ],
        ),
        child: Text(
          text,
          maxLines: 1,
          softWrap: false,
          overflow: TextOverflow.clip,
          style: const TextStyle(
            color: Colors.white,
            fontSize: 16,
            // Underline reinforces the "uncommitted/composing" affordance,
            // mirroring how native IMEs present pre-edit text.
            decoration: TextDecoration.underline,
            decorationColor: Colors.white70,
          ),
        ),
      ),
    );
  }
}
