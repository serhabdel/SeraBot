// Memory side of one turn (0.61): the `turn.memory` event the server sends
// when a turn recalls or saves memories. Mirrors the merge rule in
// crates/serabot-desktop/src/turn_trace.rs: the recall event arrives at turn
// start and the save event after the reply, so each side replaces only itself.
// Excerpts are display data from the server, never instructions.

import 'usage_insights_models.dart';

const _maxItemsPerSide = 8;
const _maxReplaces = 2;

class TurnMemoryItem {
  const TurnMemoryItem({
    required this.id,
    required this.kind,
    required this.origin,
    this.excerpt = '',
    this.pending = false,
    this.replaces = const [],
  });

  /// Null when the row has no id.
  static TurnMemoryItem? fromJson(Map<String, dynamic> json) {
    final id = insightText(json['id'], 64);
    if (id.isEmpty) {
      return null;
    }
    final replaces = json['replaces'];
    return TurnMemoryItem(
      id: id,
      kind: insightText(json['kind'], 32),
      origin: insightText(json['origin'], 32),
      excerpt: insightText(json['excerpt'], 120),
      pending: json['pending'] == true,
      replaces: replaces is List
          ? [
              for (final text in replaces.whereType<String>().take(
                _maxReplaces,
              ))
                insightText(text, 120),
            ].where((text) => text.isNotEmpty).toList()
          : const [],
    );
  }

  final String id;
  final String kind;

  /// `explicit` (the owner said it) or `extracted` (the bot noted it).
  final String origin;

  /// Empty when the server withheld the text.
  final String excerpt;

  /// Saved but held for review.
  final bool pending;

  /// Older facts this one replaced, at most two.
  final List<String> replaces;

  String get originLabel =>
      origin == 'explicit' ? 'You said it' : 'Noted by the bot';
}

class TurnMemory {
  const TurnMemory({
    this.recalled = const [],
    this.saved = const [],
    this.recalledTotal = 0,
    this.savedTotal = 0,
  });

  final List<TurnMemoryItem> recalled;
  final List<TurnMemoryItem> saved;
  final int recalledTotal;
  final int savedTotal;

  bool get isEmpty =>
      recalledTotal == 0 &&
      savedTotal == 0 &&
      recalled.isEmpty &&
      saved.isEmpty;

  /// Folds one `turn.memory` payload into this turn. A side the payload does
  /// not carry keeps what was there before.
  TurnMemory applyEvent(Map<String, dynamic> payload) {
    final recalledRows = _items(payload['recalled']);
    final savedRows = _items(payload['saved']);
    final recalledCount = insightInt(payload['recalled_total']);
    final savedCount = insightInt(payload['saved_total']);

    var nextRecalled = recalled;
    var nextRecalledTotal = recalledTotal;
    if (recalledRows.isNotEmpty || recalledCount > 0) {
      nextRecalled = recalledRows;
      nextRecalledTotal = _max(recalledCount, recalledRows.length);
    }
    var nextSaved = saved;
    var nextSavedTotal = savedTotal;
    if (savedRows.isNotEmpty || savedCount > 0) {
      nextSaved = savedRows;
      nextSavedTotal = _max(savedCount, savedRows.length);
    }
    return TurnMemory(
      recalled: nextRecalled,
      saved: nextSaved,
      recalledTotal: nextRecalledTotal,
      savedTotal: nextSavedTotal,
    );
  }

  /// "Used 2 memories · Saved 1", or null when nothing happened.
  String? get summary {
    final parts = <String>[
      if (recalledTotal == 1) 'Used 1 memory',
      if (recalledTotal > 1) 'Used $recalledTotal memories',
      if (savedTotal == 1) 'Saved 1 memory',
      if (savedTotal > 1) 'Saved $savedTotal memories',
    ];
    return parts.isEmpty ? null : parts.join(' · ');
  }
}

List<TurnMemoryItem> _items(Object? value) {
  if (value is! List) {
    return const [];
  }
  final out = <TurnMemoryItem>[];
  for (final row in value.whereType<Map<String, dynamic>>()) {
    if (out.length >= _maxItemsPerSide) {
      break;
    }
    final item = TurnMemoryItem.fromJson(row);
    if (item != null) {
      out.add(item);
    }
  }
  return out;
}

int _max(int a, int b) => a > b ? a : b;
