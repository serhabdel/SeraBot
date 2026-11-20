import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/turn_memory.dart';

void main() {
  test('recall and save events fill their own side only', () {
    final recall = TurnMemory().applyEvent({
      'task_id': 'task_one',
      'turn_id': 'turn_one',
      'conversation_id': 'dir:synthetic',
      'recalled': [
        {
          'id': 'm1',
          'kind': 'preference',
          'origin': 'explicit',
          'excerpt': 'Likes green tea',
        },
        {'id': 'm2', 'kind': 'fact', 'origin': 'extracted'},
      ],
      'recalled_total': 3,
    });

    expect(recall.recalled, hasLength(2));
    expect(
      recall.recalledTotal,
      3,
      reason: 'the total can exceed the rows sent',
    );
    expect(recall.recalled.first.excerpt, 'Likes green tea');
    expect(
      recall.recalled.last.excerpt,
      isEmpty,
      reason: 'withheld text stays empty',
    );
    expect(recall.saved, isEmpty);

    final saved = recall.applyEvent({
      'task_id': 'task_one',
      'recalled': <Object>[],
      'saved': [
        {
          'id': 'm9',
          'kind': 'fact',
          'origin': 'extracted',
          'excerpt': 'Works on Tuesdays',
          'pending': true,
          'replaces': ['Works on Mondays', 'Works on Sundays', 'a third one'],
        },
      ],
      'saved_total': 1,
    });

    expect(
      saved.recalled,
      hasLength(2),
      reason: 'recall survives the save event',
    );
    expect(saved.recalledTotal, 3);
    expect(saved.saved.single.pending, isTrue);
    expect(
      saved.saved.single.replaces,
      hasLength(2),
      reason: 'at most two replaced facts',
    );
    expect(saved.savedTotal, 1);
    expect(saved.saved.single.originLabel, 'Noted by the bot');
  });

  test('summary counts what happened in the turn', () {
    expect(const TurnMemory().summary, isNull);
    expect(const TurnMemory().isEmpty, isTrue);
    final memory = TurnMemory().applyEvent({
      'recalled': [
        {'id': 'a'},
        {'id': 'b'},
      ],
      'recalled_total': 2,
      'saved_total': 1,
    });
    expect(memory.isEmpty, isFalse);
    expect(memory.summary, 'Used 2 memories · Saved 1 memory');
  });

  test('rows without an id are dropped and each side is bounded', () {
    final rows = [
      for (var i = 0; i < 12; i++)
        {'id': 'mem_$i', 'excerpt': 'Synthetic fact $i'},
      {'excerpt': 'no id here'},
    ];
    final memory = TurnMemory().applyEvent({'recalled': rows});
    expect(memory.recalled, hasLength(8));
    expect(memory.recalled.every((m) => m.id.startsWith('mem_')), isTrue);
    expect(
      memory.recalledTotal,
      8,
      reason: 'no total sent: the rows kept are the count',
    );
  });

  test('malformed payload fields fall back to empty values', () {
    final memory = TurnMemory().applyEvent({
      'recalled': 'nonsense',
      'saved': null,
      'recalled_total': 'many',
    });
    expect(memory.isEmpty, isTrue);
  });

  test('item parsing strips control characters and caps replaced text', () {
    final item = TurnMemoryItem.fromJson({
      'id': 'mem_x',
      'kind': 'fact',
      'origin': 'explicit',
      'excerpt': 'Line\u0000one',
      'pending': 'yes',
      'replaces': ['a', 2, 'b', 'c'],
    })!;
    expect(item.excerpt, 'Lineone');
    expect(item.pending, isFalse, reason: 'pending must be a real boolean');
    expect(item.replaces, ['a', 'b']);
    expect(item.originLabel, 'You said it');
    expect(TurnMemoryItem.fromJson({'kind': 'fact'}), isNull);
  });
}
