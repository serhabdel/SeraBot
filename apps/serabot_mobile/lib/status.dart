import 'package:material_ui/material_ui.dart';

/// A three-step limit status, the same words the desktop console uses.
/// Colour is never the only signal: every level has an icon and a word.
enum StatusLevel { ok, near, atLimit }

StatusLevel statusLevelFromFlags({
  required bool nearLimit,
  required bool atLimit,
}) {
  if (atLimit) {
    return StatusLevel.atLimit;
  }
  return nearLimit ? StatusLevel.near : StatusLevel.ok;
}

String statusLevelLabel(StatusLevel level) {
  switch (level) {
    case StatusLevel.ok:
      return 'OK';
    case StatusLevel.near:
      return 'Near limit';
    case StatusLevel.atLimit:
      return 'At limit';
  }
}

IconData statusLevelIcon(StatusLevel level) {
  switch (level) {
    case StatusLevel.ok:
      return Icons.check_circle_outline;
    case StatusLevel.near:
      return Icons.warning_amber_rounded;
    case StatusLevel.atLimit:
      return Icons.error_outline;
  }
}

/// Fill percentage, clamped to 0..100. A zero limit reads as 0.
int quotaPercent(int used, int limit) {
  if (limit <= 0) {
    return 0;
  }
  final safeUsed = used < 0 ? 0 : used;
  final percent = (safeUsed * 100) ~/ limit;
  return percent > 100 ? 100 : percent;
}

/// Pill with an icon and a word. Colours are picked for both themes.
class StatusChip extends StatelessWidget {
  const StatusChip({super.key, required this.level, this.label});

  final StatusLevel level;

  /// Overrides the default word for [level].
  final String? label;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final dark = theme.brightness == Brightness.dark;
    final (Color bg, Color fg) = switch (level) {
      StatusLevel.ok => dark
          ? (const Color(0xFF16382A), const Color(0xFF8FE3B0))
          : (const Color(0xFFDFF5E8), const Color(0xFF0B5B2E)),
      StatusLevel.near => dark
          ? (const Color(0xFF4A3A0D), const Color(0xFFF5D26B))
          : (const Color(0xFFFFF0C7), const Color(0xFF6B4A00)),
      StatusLevel.atLimit => (
          theme.colorScheme.errorContainer,
          theme.colorScheme.onErrorContainer,
        ),
    };
    final text = label ?? statusLevelLabel(level);
    return Semantics(
      label: text,
      excludeSemantics: true,
      child: Container(
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 4),
        decoration: BoxDecoration(
          color: bg,
          borderRadius: BorderRadius.circular(999),
        ),
        child: Row(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(statusLevelIcon(level), size: 16, color: fg),
            const SizedBox(width: 6),
            Text(
              text,
              style: theme.textTheme.labelMedium?.copyWith(
                color: fg,
                fontWeight: FontWeight.w600,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
