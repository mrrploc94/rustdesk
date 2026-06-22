import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/models/platform_model.dart';

/// In-app help documentation and first-use tutorial for Vietnamese input.
///
/// This file implements Requirement 18 (Documentation and User Guidance):
///   * 18.1 — in-app help documentation for Vietnamese input configuration
///   * 18.3 — example typing sequences for Telex, VNI and VNI Windows
///   * 18.4 — a brief tutorial overlay shown the first time Vietnamese input is
///            enabled, explaining basic usage and the keyboard shortcuts
///   * 18.5 — a comparison table of key mappings across the three methods
///
/// The widgets use [translate] for every user-facing string (consistent with
/// the rest of the codebase) and reuse the shared [CustomAlertDialog] /
/// [dialogButton] helpers and [OverlayDialogManager] so the help and tutorial
/// render the same way as every other dialog in the app.

/// Local option key tracking whether the first-use tutorial has been shown.
///
/// Persisted through RustDesk's existing `get_option`/`set_option` mechanism
/// (`bind.mainGetLocalOption` / `bind.mainSetLocalOption`) so the tutorial is
/// only ever shown once, even across application restarts.
const String kOptionVietnameseTutorialShown = 'vietnamese-input-tutorial-shown';

/// Default keyboard shortcuts surfaced in the help/tutorial text. These mirror
/// the defaults in `vietnamese_input_settings.dart` (Requirements 9.1 / 9.2).
const String _kDefaultToggleShortcut = 'Ctrl+Shift+V';
const String _kDefaultCycleShortcut = 'Ctrl+Shift+I';

/// A single row in the cross-method comparison table.
class _MappingRow {
  const _MappingRow(this.feature, this.telex, this.vni, this.vniWindows);

  /// What the row produces, e.g. "â", "sắc (á)".
  final String feature;

  /// Telex key sequence, e.g. "aa".
  final String telex;

  /// VNI key sequence, e.g. "a6".
  final String vni;

  /// VNI Windows key sequence.
  final String vniWindows;
}

/// Comparison data across the three input methods (Requirement 18.5).
///
/// Grouped as: vowel marks (â/ă/ơ/ư/ê/ô), the đ consonant, and the five tone
/// marks (sắc/huyền/hỏi/ngã/nặng).
const List<_MappingRow> _kComparisonRows = [
  // Vowel marks.
  _MappingRow('â', 'aa', 'a6', 'a6'),
  _MappingRow('ă', 'aw', 'a8', 'a8'),
  _MappingRow('ê', 'ee', 'e6', 'e6'),
  _MappingRow('ô', 'oo', 'o6', 'o6'),
  _MappingRow('ơ', 'ow', 'o7', 'o7'),
  _MappingRow('ư', 'uw', 'u7', 'u7'),
  // Consonant mark.
  _MappingRow('đ', 'dd', 'd9', 'd9'),
  // Tone marks (shown on the base vowel "a").
  _MappingRow('sắc (á)', 'as', 'a1', 'a1'),
  _MappingRow('huyền (à)', 'af', 'a2', 'a2'),
  _MappingRow('hỏi (ả)', 'ar', 'a3', 'a3'),
  _MappingRow('ngã (ã)', 'ax', 'a4', 'a4'),
  _MappingRow('nặng (ạ)', 'aj', 'a5', 'a5'),
];

/// Worked example sequences for each method (Requirement 18.3).
class _ExampleSet {
  const _ExampleSet(this.method, this.examples);
  final String method;

  /// Pairs of (typed sequence, resulting Vietnamese text).
  final List<List<String>> examples;
}

const List<_ExampleSet> _kExampleSets = [
  _ExampleSet('Telex', [
    ['dduowjc', 'được'],
    ['as', 'á'],
    ['Vieejt', 'Việt'],
    ['xin chaof', 'xin chào'],
  ]),
  _ExampleSet('VNI', [
    ['d9uo75c', 'được'],
    ['a1', 'á'],
    ['Vie6t5', 'Việt'],
    ['xin cha2o', 'xin chào'],
  ]),
  _ExampleSet('VNI Windows', [
    ['d9uo75c', 'được'],
    ['a1', 'á'],
    ['Vie6t5', 'Việt'],
  ]),
];

/// The scrollable help content widget.
///
/// Shows three sections: how to enable Vietnamese input, example typing
/// sequences per method, and a comparison table of key mappings. Designed to be
/// dropped into a dialog ([showVietnameseInputHelp]) or embedded directly in a
/// page if needed.
class VietnameseInputHelp extends StatelessWidget {
  const VietnameseInputHelp({Key? key}) : super(key: key);

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        _enableSection(context),
        const SizedBox(height: 16),
        _examplesSection(context),
        const SizedBox(height: 16),
        _comparisonSection(context),
      ],
    );
  }

  // --- Sections ------------------------------------------------------------

  Widget _enableSection(BuildContext context) {
    return _section(
      context,
      title: 'How to enable Vietnamese input',
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          _bullet(context,
              'Open Settings and go to the Vietnamese input page.'),
          _bullet(context, 'Turn on "Enable Vietnamese input".'),
          _bullet(context,
              'Pick an input method: Telex, VNI or VNI Windows.'),
          _bullet(context,
              'Type in a remote session — characters are composed locally and '
              'the composed text is sent once you press space or punctuation.'),
          const SizedBox(height: 8),
          _shortcutLine(context, _kDefaultToggleShortcut,
              'Toggle Vietnamese input on/off'),
          _shortcutLine(context, _kDefaultCycleShortcut,
              'Cycle methods: Telex → VNI → VNI Windows → Off'),
        ],
      ),
    );
  }

  Widget _examplesSection(BuildContext context) {
    return _section(
      context,
      title: 'Example typing sequences',
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: _kExampleSets.map((set) {
          return Padding(
            padding: const EdgeInsets.only(bottom: 10),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  translate(set.method),
                  style: const TextStyle(
                      fontSize: 15, fontWeight: FontWeight.bold),
                ),
                const SizedBox(height: 4),
                ...set.examples.map((e) => Padding(
                      padding: const EdgeInsets.only(top: 2, left: 8),
                      child: RichText(
                        text: TextSpan(
                          style: DefaultTextStyle.of(context).style,
                          children: [
                            TextSpan(
                              text: 'Type ',
                              style:
                                  TextStyle(color: disabledTextColor(context, true)),
                            ),
                            TextSpan(
                              text: '"${e[0]}"',
                              style: const TextStyle(
                                  fontFamily: 'monospace',
                                  fontWeight: FontWeight.w600),
                            ),
                            TextSpan(
                              text: '  →  ',
                              style:
                                  TextStyle(color: disabledTextColor(context, true)),
                            ),
                            TextSpan(
                              text: '"${e[1]}"',
                              style: TextStyle(
                                  fontWeight: FontWeight.w600,
                                  color: MyTheme.accent),
                            ),
                          ],
                        ),
                      ),
                    )),
              ],
            ),
          );
        }).toList(),
      ),
    );
  }

  Widget _comparisonSection(BuildContext context) {
    return _section(
      context,
      title: 'Key mapping comparison',
      child: SingleChildScrollView(
        scrollDirection: Axis.horizontal,
        child: DataTable(
          headingRowHeight: 36,
          dataRowMinHeight: 30,
          dataRowMaxHeight: 38,
          columnSpacing: 24,
          columns: [
            DataColumn(label: Text(translate('Result'))),
            const DataColumn(label: Text('Telex')),
            const DataColumn(label: Text('VNI')),
            const DataColumn(label: Text('VNI Windows')),
          ],
          rows: _kComparisonRows
              .map((row) => DataRow(cells: [
                    DataCell(Text(row.feature,
                        style: const TextStyle(fontWeight: FontWeight.w600))),
                    DataCell(Text(row.telex,
                        style: const TextStyle(fontFamily: 'monospace'))),
                    DataCell(Text(row.vni,
                        style: const TextStyle(fontFamily: 'monospace'))),
                    DataCell(Text(row.vniWindows,
                        style: const TextStyle(fontFamily: 'monospace'))),
                  ]))
              .toList(),
        ),
      ),
    );
  }

  // --- Building blocks -----------------------------------------------------

  Widget _section(BuildContext context,
      {required String title, required Widget child}) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          translate(title),
          style: const TextStyle(fontSize: 17, fontWeight: FontWeight.bold),
        ),
        const SizedBox(height: 8),
        child,
      ],
    );
  }

  Widget _bullet(BuildContext context, String text) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Text('•  '),
          Expanded(child: Text(translate(text))),
        ],
      ),
    );
  }

  Widget _shortcutLine(BuildContext context, String shortcut, String desc) {
    return Padding(
      padding: const EdgeInsets.only(top: 4),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Container(
            padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
            decoration: BoxDecoration(
              color: Theme.of(context).brightness == Brightness.light
                  ? Colors.black12
                  : Colors.white24,
              borderRadius: BorderRadius.circular(4),
            ),
            child: Text(shortcut,
                style: const TextStyle(
                    fontFamily: 'monospace', fontWeight: FontWeight.w600)),
          ),
          const SizedBox(width: 8),
          Expanded(child: Text(translate(desc))),
        ],
      ),
    );
  }
}

/// Open the full Vietnamese input help documentation in a dialog
/// (Requirements 18.1, 18.3, 18.5).
///
/// Wire this up from the settings page with, for example, a help
/// `IconButton(icon: const Icon(Icons.help_outline), onPressed: () =>
/// showVietnameseInputHelp(gFFI.dialogManager))`.
Future<void> showVietnameseInputHelp(OverlayDialogManager dialogManager) async {
  await dialogManager.show<void>(
    (setState, close, context) => CustomAlertDialog(
      title: Row(
        children: [
          const Icon(Icons.help_outline),
          const SizedBox(width: 10),
          Expanded(child: Text(translate('Vietnamese input help'))),
        ],
      ),
      content: const SizedBox(
        width: 560,
        child: VietnameseInputHelp(),
      ),
      actions: [
        dialogButton('Close', onPressed: close, isOutline: true),
      ],
      onCancel: close,
    ),
    clickMaskDismiss: true,
    backDismiss: true,
  );
}

/// The compact tutorial content shown the first time Vietnamese input is
/// enabled (Requirement 18.4). Explains basic usage and the keyboard shortcuts
/// without the full mapping tables.
class VietnameseInputTutorial extends StatelessWidget {
  const VietnameseInputTutorial({Key? key}) : super(key: key);

  @override
  Widget build(BuildContext context) {
    Widget step(String text) => Padding(
          padding: const EdgeInsets.only(bottom: 6),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Text('•  '),
              Expanded(child: Text(translate(text))),
            ],
          ),
        );

    Widget shortcut(String keys, String desc) => Padding(
          padding: const EdgeInsets.only(top: 4),
          child: Row(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Container(
                padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
                decoration: BoxDecoration(
                  color: Theme.of(context).brightness == Brightness.light
                      ? Colors.black12
                      : Colors.white24,
                  borderRadius: BorderRadius.circular(4),
                ),
                child: Text(keys,
                    style: const TextStyle(
                        fontFamily: 'monospace',
                        fontWeight: FontWeight.w600)),
              ),
              const SizedBox(width: 8),
              Expanded(child: Text(translate(desc))),
            ],
          ),
        );

    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        Text(
          translate(
              'Vietnamese input is on. Type as you normally would and your '
              'text is composed before being sent to the remote machine.'),
        ),
        const SizedBox(height: 12),
        step('Telex example: type "dduowjc" to get "được".'),
        step('VNI example: type "d9uo75c" to get "được".'),
        step('Press space or punctuation to commit the composed word.'),
        step('Backspace steps back through the composition.'),
        const SizedBox(height: 12),
        Text(
          translate('Keyboard shortcuts'),
          style: const TextStyle(fontWeight: FontWeight.bold),
        ),
        const SizedBox(height: 4),
        shortcut(_kDefaultToggleShortcut, 'Toggle Vietnamese input on/off'),
        shortcut(_kDefaultCycleShortcut,
            'Cycle methods: Telex → VNI → VNI Windows → Off'),
      ],
    );
  }
}

/// Show the first-use tutorial overlay (Requirement 18.4).
///
/// Set [markShown] to persist the "tutorial shown" flag when the user
/// dismisses it, so it is not shown again. The "Learn more" action opens the
/// full help documentation.
Future<void> showVietnameseInputTutorial(
  OverlayDialogManager dialogManager, {
  bool markShown = true,
}) async {
  await dialogManager.show<void>(
    (setState, close, context) {
      Future<void> finish() async {
        if (markShown) {
          await bind.mainSetLocalOption(
              key: kOptionVietnameseTutorialShown, value: 'Y');
        }
        close();
      }

      return CustomAlertDialog(
        title: Row(
          children: [
            const Icon(Icons.keyboard_alt_outlined),
            const SizedBox(width: 10),
            Expanded(child: Text(translate('Getting started with Vietnamese input'))),
          ],
        ),
        content: const SizedBox(
          width: 460,
          child: VietnameseInputTutorial(),
        ),
        actions: [
          dialogButton('Learn more', isOutline: true, onPressed: () async {
            await finish();
            await showVietnameseInputHelp(dialogManager);
          }),
          dialogButton('Got it', onPressed: finish),
        ],
        onCancel: finish,
      );
    },
    clickMaskDismiss: false,
    backDismiss: true,
  );
}

/// Show the first-use tutorial only if it has not been shown before
/// (Requirement 18.4).
///
/// Call this right after Vietnamese input is first enabled. It checks the
/// persisted [kOptionVietnameseTutorialShown] flag via `mainGetLocalOption`
/// and, if unset, displays the tutorial (which then records the flag). Returns
/// `true` when the tutorial was shown this call, `false` otherwise.
Future<bool> maybeShowVietnameseInputTutorial(
    OverlayDialogManager dialogManager) async {
  final shown = bind.mainGetLocalOption(key: kOptionVietnameseTutorialShown);
  if (shown == 'Y') {
    return false;
  }
  await showVietnameseInputTutorial(dialogManager, markShown: true);
  return true;
}
