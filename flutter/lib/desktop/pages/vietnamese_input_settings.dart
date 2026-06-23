import 'package:flutter/material.dart';
import 'package:flutter_hbb/common.dart';
import 'package:flutter_hbb/common/widgets/vietnamese_input_help.dart';
import 'package:flutter_hbb/models/platform_model.dart';
import 'package:flutter_hbb/models/vietnamese_input_model.dart';
import 'package:get/get.dart';
import 'package:provider/provider.dart';

/// Local option keys used to persist Vietnamese input preferences through
/// RustDesk's existing `get_option`/`set_option` mechanism (see
/// `bind.mainGetLocalOption` / `bind.mainSetLocalOption`). These mirror the
/// Rust-side `VietnameseInputConfig` fields so the UI and core stay in sync.
const String kOptionVietnameseNormalization = 'vietnamese-input-normalization';
const String kOptionVietnameseToggleShortcut =
    'vietnamese-input-toggle-shortcut';
const String kOptionVietnameseCycleShortcut = 'vietnamese-input-cycle-shortcut';

/// Persisted values for the normalization mode. Matches the Rust
/// `NormalizationForm` enum (NFC default, NFD optional).
const String kNormalizationNfc = 'nfc';
const String kNormalizationNfd = 'nfd';

/// Default keyboard shortcuts (see Requirements 9.1 and 9.2).
const String kDefaultToggleShortcut = 'Ctrl+Shift+V';
const String kDefaultCycleShortcut = 'Ctrl+Shift+I';

// Layout constants kept local to this page so it does not depend on the
// private constants in `desktop_setting_page.dart`, while still matching the
// visual rhythm of the other settings cards.
const double _kCardFixedWidth = 540;
const double _kCardLeftMargin = 15;
const double _kContentHMargin = 15;
const double _kTitleFontSize = 20;
const double _kContentFontSize = 15;

/// Vietnamese input configuration page.
///
/// Implements Requirement 8 (Configuration User Interface): an enable/disable
/// toggle, input method selector, Unicode normalization mode selector, keyboard
/// shortcut configuration, a live test/preview area, and tooltip explanations
/// for every setting. All changes are applied immediately without requiring an
/// application restart — method/enabled changes are forwarded to the Rust core
/// through [VietnameseInputModel] (which calls the FFI bridge), and the
/// remaining options are persisted through `mainSetLocalOption`.
class VietnameseInputSettingsPage extends StatefulWidget {
  const VietnameseInputSettingsPage({Key? key, this.model}) : super(key: key);

  /// Optional shared model. When omitted the page owns a private model instance
  /// so it can be opened standalone (e.g. from a settings menu) without any
  /// external wiring.
  final VietnameseInputModel? model;

  @override
  State<VietnameseInputSettingsPage> createState() =>
      _VietnameseInputSettingsPageState();
}

class _VietnameseInputSettingsPageState
    extends State<VietnameseInputSettingsPage> {
  late final VietnameseInputModel _model;
  late final bool _ownsModel;

  late String _normalization;
  late final TextEditingController _toggleShortcutController;
  late final TextEditingController _cycleShortcutController;
  final TextEditingController _previewController = TextEditingController();

  /// Whether the diagnostic "Test Mode" is active. When on, every change to
  /// the composition preview is recorded into [_steps] so the user can see the
  /// composition evolve transformation by transformation (Requirement 17.4).
  bool _testModeEnabled = false;

  /// Ordered log of composition steps captured while Test Mode is enabled.
  /// Each entry holds the composed preview at that point plus a human readable
  /// description of the transformation that produced it.
  final List<_CompositionStep> _steps = [];

  /// Last observed composition preview, used to diff against the next update
  /// and infer which transformation was applied.
  String _lastPreview = '';

  @override
  void initState() {
    super.initState();
    _ownsModel = widget.model == null;
    _model = widget.model ?? VietnameseInputModel();
    // Observe composition-preview changes so Test Mode can record the step
    // history as the user types in the local test field.
    _model.addListener(_onModelChanged);

    // Load persisted preferences, falling back to spec defaults when unset.
    final storedNorm =
        bind.mainGetLocalOption(key: kOptionVietnameseNormalization);
    _normalization =
        storedNorm == kNormalizationNfd ? kNormalizationNfd : kNormalizationNfc;

    final storedToggle =
        bind.mainGetLocalOption(key: kOptionVietnameseToggleShortcut);
    final storedCycle =
        bind.mainGetLocalOption(key: kOptionVietnameseCycleShortcut);
    _toggleShortcutController = TextEditingController(
        text: storedToggle.isEmpty ? kDefaultToggleShortcut : storedToggle);
    _cycleShortcutController = TextEditingController(
        text: storedCycle.isEmpty ? kDefaultCycleShortcut : storedCycle);
  }

  @override
  void dispose() {
    _model.removeListener(_onModelChanged);
    _toggleShortcutController.dispose();
    _cycleShortcutController.dispose();
    _previewController.dispose();
    if (_ownsModel) {
      _model.dispose();
    }
    super.dispose();
  }

  /// Record composition-preview transitions into [_steps] while Test Mode is
  /// active. The composition steps themselves are produced by the Rust composer
  /// (which streams preview updates through [VietnameseInputModel]); here we
  /// observe how that preview evolves over time and label each transition with
  /// the transformation it represents.
  void _onModelChanged() {
    final current = _model.compositionPreview;
    if (!_testModeEnabled) {
      _lastPreview = current;
      return;
    }
    if (current == _lastPreview) return;

    if (current.isEmpty) {
      // Composition committed or flushed: clear the step log for the next word.
      if (_steps.isNotEmpty) {
        setState(_steps.clear);
      }
      _lastPreview = current;
      return;
    }

    final description = _describeTransition(_lastPreview, current);
    setState(() {
      _steps.add(_CompositionStep(preview: current, description: description));
      // Keep the log bounded so a long session cannot grow it without limit.
      if (_steps.length > 50) {
        _steps.removeAt(0);
      }
    });
    _lastPreview = current;
  }

  void _clearSteps() {
    if (_steps.isEmpty) return;
    setState(_steps.clear);
  }

  // --- Transformation classification --------------------------------------

  /// Describe the transformation that turned [prev] into [curr] for display in
  /// the Test Mode step log. Works on the composed (NFC) preview by diffing the
  /// first differing character and identifying any tone/vowel mark that was
  /// added or removed.
  String _describeTransition(String prev, String curr) {
    if (curr.length > prev.length) {
      final added = curr.substring(prev.length);
      final mark = _vowelMarkName(added);
      if (mark != null) return translate('$mark applied');
      final tone = _toneName(added);
      if (tone != null) return '${translate(tone)} ${translate('tone applied')}';
      return translate('character added');
    }
    if (curr.length < prev.length) {
      return translate('reverted (backspace)');
    }
    // Same length: locate the first differing character and classify it.
    final len = curr.length;
    for (var i = 0; i < len; i++) {
      final c = curr[i];
      final p = i < prev.length ? prev[i] : '';
      if (c == p) continue;
      final tone = _toneName(c);
      final prevTone = _toneName(p);
      final mark = _vowelMarkName(c);
      final prevMark = _vowelMarkName(p);
      if (tone != null && tone != prevTone) {
        return '${translate(tone)} ${translate('tone applied')}';
      }
      if (mark != null && mark != prevMark) {
        return translate('$mark applied');
      }
      if (tone == null && prevTone != null) {
        return translate('tone removed');
      }
      if (mark == null && prevMark != null) {
        return translate('mark removed');
      }
      return translate('transformed');
    }
    return translate('updated');
  }

  /// Return a label for the tone carried by [ch], or null if it carries none.
  static String? _toneName(String ch) {
    if (ch.isEmpty) return null;
    final c = ch.toLowerCase();
    const sac = 'áấắéếíóốớúứý';
    const huyen = 'àầằèềìòồờùừỳ';
    const hoi = 'ảẩẳẻểỉỏổởủửỷ';
    const nga = 'ãẫẵẽễĩõỗỡũữỹ';
    const nang = 'ạậặẹệịọộợụựỵ';
    if (sac.contains(c)) return 'sắc (acute)';
    if (huyen.contains(c)) return 'huyền (grave)';
    if (hoi.contains(c)) return 'hỏi (hook)';
    if (nga.contains(c)) return 'ngã (tilde)';
    if (nang.contains(c)) return 'nặng (dot)';
    return null;
  }

  /// Return a label for the vowel/consonant mark carried by [ch], or null.
  static String? _vowelMarkName(String ch) {
    if (ch.isEmpty) return null;
    final c = ch.toLowerCase();
    const circumflex = 'âấầẩẫậêếềểễệôốồổỗộ';
    const breve = 'ăắằẳẵặ';
    const horn = 'ơớờởỡợưứừửữự';
    if (circumflex.contains(c)) return 'circumflex (â/ê/ô)';
    if (breve.contains(c)) return 'breve (ă)';
    if (horn.contains(c)) return 'horn (ơ/ư)';
    if (c == 'đ') return 'consonant mark (đ)';
    return null;
  }

  Future<void> _setNormalization(String value) async {
    if (_normalization == value) return;
    setState(() => _normalization = value);
    // Apply immediately: persist so the Rust core picks it up on the next
    // composition commit (Requirement 8.7).
    await bind.mainSetLocalOption(
        key: kOptionVietnameseNormalization, value: value);
  }

  Future<void> _saveToggleShortcut(String value) async {
    await bind.mainSetLocalOption(
        key: kOptionVietnameseToggleShortcut, value: value.trim());
  }

  Future<void> _saveCycleShortcut(String value) async {
    await bind.mainSetLocalOption(
        key: kOptionVietnameseCycleShortcut, value: value.trim());
  }

  @override
  Widget build(BuildContext context) {
    return ChangeNotifierProvider<VietnameseInputModel>.value(
      value: _model,
      child: Consumer<VietnameseInputModel>(
        builder: (context, model, _) {
          final enabled = model.enabled;
          return ListView(
            padding: const EdgeInsets.symmetric(vertical: 10),
            children: [
              _enableCard(context, model),
              _methodCard(context, model, enabled),
              _normalizationCard(context, enabled),
              _shortcutCard(context, enabled),
              _previewCard(context, model, enabled),
            ],
          );
        },
      ),
    );
  }

  // --- Cards ---------------------------------------------------------------

  Widget _enableCard(BuildContext context, VietnameseInputModel model) {
    return _card(
      title: 'Vietnamese input',
      trailing: IconButton(
        icon: const Icon(Icons.help_outline),
        tooltip: translate('Vietnamese input help'),
        onPressed: () => showVietnameseInputHelp(gFFI.dialogManager),
      ),
      children: [
        _tooltip(
          'Turn Vietnamese input composition on or off. When off, every '
          'keystroke is sent to the remote session unchanged.',
          _switchRow(
            context,
            label: 'Enable Vietnamese input',
            value: model.enabled,
            onChanged: (v) async {
              await model.setEnabled(v);
              // On the first time Vietnamese input is enabled, show a brief
              // tutorial overlay explaining basic usage and shortcuts
              // (Requirement 18.4). It records a flag so it only shows once.
              if (v) {
                await maybeShowVietnameseInputTutorial(gFFI.dialogManager);
              }
            },
          ),
        ),
      ],
    );
  }

  Widget _methodCard(
      BuildContext context, VietnameseInputModel model, bool enabled) {
    Widget methodRadio(InputMethod method, String tip) {
      return _tooltip(
        tip,
        _radioRow<InputMethod>(
          context,
          label: method.displayName,
          value: method,
          groupValue: model.activeMethod,
          enabled: enabled,
          onChanged: (m) => model.setMethod(m),
        ),
      );
    }

    return _card(
      title: 'Input method',
      children: [
        methodRadio(InputMethod.telex,
            'Telex: type diacritics with letters (e.g. "ow" → ơ, "s" → sắc).'),
        methodRadio(InputMethod.vni,
            'VNI: type diacritics with number keys (e.g. "o7" → ơ, "a1" → á).'),
        methodRadio(InputMethod.vniWindows,
            'VNI Windows: VNI variant using Windows-style composition order.'),
        methodRadio(InputMethod.off,
            'Off: pass all keystrokes straight through without composition.'),
      ],
    );
  }

  Widget _normalizationCard(BuildContext context, bool enabled) {
    return _card(
      title: 'Unicode normalization',
      children: [
        _tooltip(
          'NFC (Composed): each character is a single code point. Recommended '
          'for the widest remote-system compatibility.',
          _radioRow<String>(
            context,
            label: 'NFC (Composed)',
            value: kNormalizationNfc,
            groupValue: _normalization,
            enabled: enabled,
            onChanged: _setNormalization,
          ),
        ),
        _tooltip(
          'NFD (Decomposed): base letters plus separate combining diacritics, '
          'in canonical order.',
          _radioRow<String>(
            context,
            label: 'NFD (Decomposed)',
            value: kNormalizationNfd,
            groupValue: _normalization,
            enabled: enabled,
            onChanged: _setNormalization,
          ),
        ),
      ],
    );
  }

  Widget _shortcutCard(BuildContext context, bool enabled) {
    return _card(
      title: 'Keyboard shortcuts',
      children: [
        _tooltip(
          'Shortcut that toggles Vietnamese input on and off. '
          'Example: Ctrl+Shift+V.',
          _shortcutField(
            context,
            label: 'Toggle input',
            controller: _toggleShortcutController,
            enabled: enabled,
            onSubmitted: _saveToggleShortcut,
          ),
        ),
        _tooltip(
          'Shortcut that cycles through input methods '
          '(Telex → VNI → VNI Windows → Off). Example: Ctrl+Shift+I.',
          _shortcutField(
            context,
            label: 'Cycle methods',
            controller: _cycleShortcutController,
            enabled: enabled,
            onSubmitted: _saveCycleShortcut,
          ),
        ),
      ],
    );
  }

  Widget _previewCard(
      BuildContext context, VietnameseInputModel model, bool enabled) {
    return _card(
      title: 'Test area',
      children: [
        // Test Mode toggle (Requirement 17.4): opt-in diagnostic view that
        // records the real-time composition steps and transformations.
        _tooltip(
          'Test Mode records each composition step and the transformation '
          'applied at that step, so you can see how a syllable is built up.',
          _switchRow(
            context,
            label: 'Test Mode',
            value: _testModeEnabled,
            onChanged: enabled
                ? (v) {
                    setState(() {
                      _testModeEnabled = v;
                      // Reset the log and the diff baseline whenever Test Mode
                      // is switched so stale steps are not shown.
                      _steps.clear();
                      _lastPreview = model.compositionPreview;
                    });
                  }
                : (_) {},
          ),
        ),
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: _kContentHMargin),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                translate(
                    'Type here to test your input method. This field is local '
                    'only and is not sent to any remote session.'),
                style: TextStyle(
                  fontSize: _kContentFontSize - 2,
                  color: disabledTextColor(context, true),
                ),
              ).marginOnly(bottom: 8),
              Tooltip(
                message: translate(
                    'Local preview only — nothing typed here reaches the '
                    'remote machine.'),
                child: TextField(
                  controller: _previewController,
                  enabled: enabled,
                  maxLines: 2,
                  decoration: InputDecoration(
                    border: const OutlineInputBorder(),
                    isDense: true,
                    hintText: translate('được, Việt Nam, xin chào…'),
                  ),
                ),
              ),
              if (model.isComposing)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Text(
                    '${translate('Composing')}: ${model.compositionPreview}',
                    style: TextStyle(
                      fontSize: _kContentFontSize,
                      color: MyTheme.accent,
                    ),
                  ),
                ),
              if (_testModeEnabled) _stepLog(context),
            ],
          ),
        ),
      ],
    );
  }

  /// Build the real-time composition step log shown while Test Mode is active.
  /// Lists each transformation step in order with the composed preview and the
  /// transformation type applied at that step.
  Widget _stepLog(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(top: 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  translate('Composition steps'),
                  style: const TextStyle(
                    fontSize: _kContentFontSize,
                    fontWeight: FontWeight.w600,
                  ),
                ),
              ),
              TextButton.icon(
                onPressed: _steps.isEmpty ? null : _clearSteps,
                icon: const Icon(Icons.clear_all, size: 18),
                label: Text(translate('Clear')),
              ),
            ],
          ),
          Container(
            width: double.infinity,
            constraints: const BoxConstraints(minHeight: 60, maxHeight: 220),
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
            decoration: BoxDecoration(
              color: Theme.of(context).colorScheme.surfaceVariant.withOpacity(
                  Theme.of(context).brightness == Brightness.dark ? 0.3 : 0.5),
              borderRadius: BorderRadius.circular(6),
              border: Border.all(color: Theme.of(context).dividerColor),
            ),
            child: _steps.isEmpty
                ? Align(
                    alignment: Alignment.centerLeft,
                    child: Text(
                      translate(
                          'Start typing Vietnamese above to see each step.'),
                      style: TextStyle(
                        fontSize: _kContentFontSize - 2,
                        color: disabledTextColor(context, true),
                      ),
                    ),
                  )
                : ListView.builder(
                    shrinkWrap: true,
                    itemCount: _steps.length,
                    itemBuilder: (context, index) =>
                        _stepRow(context, index, _steps[index]),
                  ),
          ),
        ],
      ),
    );
  }

  Widget _stepRow(BuildContext context, int index, _CompositionStep step) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 3),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.center,
        children: [
          SizedBox(
            width: 24,
            child: Text(
              '${index + 1}.',
              style: TextStyle(
                fontSize: _kContentFontSize - 2,
                color: disabledTextColor(context, true),
              ),
            ),
          ),
          Text(
            step.preview,
            style: const TextStyle(
              fontSize: _kContentFontSize,
              fontWeight: FontWeight.w600,
            ),
          ),
          const SizedBox(width: 10),
          Expanded(
            child: Text(
              step.description,
              style: TextStyle(
                fontSize: _kContentFontSize - 2,
                fontStyle: FontStyle.italic,
                color: MyTheme.accent,
              ),
            ),
          ),
        ],
      ),
    );
  }

  // --- Reusable building blocks (match desktop settings styling) -----------

  Widget _card(
      {required String title,
      required List<Widget> children,
      Widget? trailing}) {
    return Row(
      children: [
        Flexible(
          child: SizedBox(
            width: _kCardFixedWidth,
            child: Card(
              child: Column(
                children: [
                  Row(
                    children: [
                      Expanded(
                        child: Text(
                          translate(title),
                          textAlign: TextAlign.start,
                          style: const TextStyle(fontSize: _kTitleFontSize),
                        ),
                      ),
                      if (trailing != null) trailing,
                    ],
                  ).marginOnly(left: _kContentHMargin, top: 10, bottom: 10),
                  ...children
                      .map((e) => e.marginOnly(top: 4, right: _kContentHMargin)),
                ],
              ).marginOnly(bottom: 10),
            ).marginOnly(left: _kCardLeftMargin, top: 15),
          ),
        ),
      ],
    );
  }

  Widget _switchRow(
    BuildContext context, {
    required String label,
    required bool value,
    required ValueChanged<bool> onChanged,
  }) {
    return Row(
      children: [
        Switch(value: value, onChanged: onChanged),
        Expanded(
          child: Text(
            translate(label),
            style: const TextStyle(fontSize: _kContentFontSize),
          ),
        ),
      ],
    ).marginOnly(left: 10);
  }

  Widget _radioRow<T>(
    BuildContext context, {
    required String label,
    required T value,
    required T groupValue,
    required bool enabled,
    required ValueChanged<T> onChanged,
  }) {
    final onChange = enabled ? (T? v) => v == null ? null : onChanged(v) : null;
    return GestureDetector(
      onTap: enabled ? () => onChanged(value) : null,
      child: Row(
        children: [
          Radio<T>(value: value, groupValue: groupValue, onChanged: onChange),
          Expanded(
            child: Text(
              translate(label),
              style: TextStyle(
                fontSize: _kContentFontSize,
                color: disabledTextColor(context, enabled),
              ),
            ).marginOnly(left: 5),
          ),
        ],
      ).marginOnly(left: 10),
    );
  }

  Widget _shortcutField(
    BuildContext context, {
    required String label,
    required TextEditingController controller,
    required bool enabled,
    required ValueChanged<String> onSubmitted,
  }) {
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: _kContentHMargin),
      child: Row(
        children: [
          SizedBox(
            width: 150,
            child: Text(
              '${translate(label)}:',
              style: TextStyle(
                fontSize: _kContentFontSize,
                color: disabledTextColor(context, enabled),
              ),
            ),
          ),
          Expanded(
            child: TextField(
              controller: controller,
              enabled: enabled,
              decoration: const InputDecoration(
                border: OutlineInputBorder(),
                isDense: true,
              ),
              // Apply immediately: persist when editing ends or on submit.
              onSubmitted: onSubmitted,
              onEditingComplete: () => onSubmitted(controller.text),
            ),
          ),
        ],
      ),
    );
  }

  /// Wrap [child] with a tooltip carrying the per-setting explanation
  /// (Requirement 8 / 18.2).
  Widget _tooltip(String message, Widget child) {
    return Tooltip(
      waitDuration: const Duration(milliseconds: 300),
      message: translate(message),
      child: child,
    );
  }
}

/// A single recorded composition step for the Test Mode step log.
class _CompositionStep {
  const _CompositionStep({required this.preview, required this.description});

  /// The composed preview text at this step (e.g. "ươ").
  final String preview;

  /// Human readable description of the transformation applied to reach this
  /// step (e.g. "horn (ơ/ư) applied", "nặng (dot) tone applied").
  final String description;
}
