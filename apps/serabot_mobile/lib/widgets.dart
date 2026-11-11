import 'dart:typed_data';

import 'package:material_ui/material_ui.dart';

import 'app_state.dart';

/// Round avatar: the uploaded picture when there is one, else the bot's emoji
/// or the first letter on a violet tile.
class SeraAvatar extends StatelessWidget {
  const SeraAvatar({
    super.key,
    required this.label,
    this.emoji,
    this.app,
    this.kind = 'bots',
    this.id,
    this.version,
    this.radius = 20,
  });

  final String label;
  final String? emoji;
  final AppState? app;
  final String kind;
  final String? id;
  final String? version;
  final double radius;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final initial = (emoji != null && emoji!.trim().isNotEmpty)
        ? emoji!
        : (label.isEmpty ? '?' : String.fromCharCode(label.runes.first).toUpperCase());
    final fallback = CircleAvatar(
      radius: radius,
      backgroundColor: scheme.primaryContainer,
      foregroundColor: scheme.onPrimaryContainer,
      child: Text(initial, style: TextStyle(fontSize: radius * 0.8)),
    );
    final a = app;
    final i = id;
    if (a == null || i == null || version == null || version!.isEmpty) {
      return fallback;
    }
    return FutureBuilder<Uint8List?>(
      future: a.avatar(kind, i, version),
      builder: (context, snap) {
        final bytes = snap.data;
        if (bytes == null) {
          return fallback;
        }
        return CircleAvatar(radius: radius, backgroundImage: MemoryImage(bytes));
      },
    );
  }
}

/// Shows an error sentence from the server with a retry action.
class ErrorBanner extends StatelessWidget {
  const ErrorBanner(this.message, {super.key, this.onRetry});
  final String message;
  final VoidCallback? onRetry;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      color: scheme.errorContainer,
      borderRadius: BorderRadius.circular(12),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Row(
          children: [
            Icon(Icons.error_outline, color: scheme.onErrorContainer),
            const SizedBox(width: 8),
            Expanded(
              child: Text(message,
                  style: TextStyle(color: scheme.onErrorContainer)),
            ),
            if (onRetry != null)
              TextButton(onPressed: onRetry, child: const Text('Retry')),
          ],
        ),
      ),
    );
  }
}

/// "12:04" today, "Mon" this week, else "3 Mar". Input is an ISO timestamp.
String shortTime(String iso, {DateTime? now}) {
  final t = DateTime.tryParse(iso)?.toLocal();
  if (t == null) {
    return '';
  }
  final n = now ?? DateTime.now();
  final sameDay = t.year == n.year && t.month == n.month && t.day == n.day;
  String two(int v) => v.toString().padLeft(2, '0');
  if (sameDay) {
    return '${two(t.hour)}:${two(t.minute)}';
  }
  if (n.difference(t).inDays < 6) {
    return const ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'][t.weekday - 1];
  }
  const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  return '${t.day} ${months[t.month - 1]}';
}

/// An animation duration that is zero when the system asks for reduced motion
/// (Android "Remove animations").
Duration motionDuration(BuildContext context, Duration duration) =>
    MediaQuery.disableAnimationsOf(context) ? Duration.zero : duration;

/// Masked text field with a Show/Hide toggle. The toggle only reveals what
/// was typed in this field.
class SecretField extends StatefulWidget {
  const SecretField({
    super.key,
    required this.controller,
    required this.label,
    this.hint,
  });

  final TextEditingController controller;
  final String label;
  final String? hint;

  @override
  State<SecretField> createState() => _SecretFieldState();
}

class _SecretFieldState extends State<SecretField> {
  bool _shown = false;

  @override
  Widget build(BuildContext context) {
    return TextField(
      controller: widget.controller,
      obscureText: !_shown,
      autocorrect: false,
      enableSuggestions: false,
      decoration: InputDecoration(
        labelText: widget.label,
        hintText: widget.hint,
        border: const OutlineInputBorder(),
        suffixIcon: IconButton(
          key: const Key('secret-toggle'),
          tooltip: _shown ? 'Hide what I typed' : 'Show what I typed',
          icon: Icon(_shown ? Icons.visibility_off : Icons.visibility),
          onPressed: () => setState(() => _shown = !_shown),
        ),
      ),
    );
  }
}

/// Asks before something that cannot be undone. Returns true on confirm.
Future<bool> confirmAction(
  BuildContext context, {
  required String title,
  required String message,
  required String confirmLabel,
}) async {
  final result = await showDialog<bool>(
    context: context,
    builder: (dialog) => AlertDialog(
      title: Text(title),
      content: Text(message),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(dialog).pop(false),
          child: const Text('Cancel'),
        ),
        FilledButton(
          key: const Key('confirm-action'),
          onPressed: () => Navigator.of(dialog).pop(true),
          child: Text(confirmLabel),
        ),
      ],
    ),
  );
  return result ?? false;
}
