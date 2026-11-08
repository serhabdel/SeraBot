import 'dart:typed_data';

import 'package:flutter/material.dart';

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
