import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/models.dart';
import 'package:serabot_mobile/validation.dart';

void main() {
  group('validateEndpoint', () {
    test('accepts https origin', () {
      expect(validateEndpoint('https://sera.example.com'), 'https://sera.example.com');
    });

    test('accepts loopback http', () {
      expect(
        validateEndpoint('http://127.0.0.1:8787'),
        'http://127.0.0.1:8787',
      );
    });

    test('rejects credentials', () {
      expect(
        () => validateEndpoint('https://user@host.example.com'),
        throwsA(isA<ValidationException>()),
      );
    });

    test('rejects path', () {
      expect(
        () => validateEndpoint('https://host.example.com/v1'),
        throwsA(isA<ValidationException>()),
      );
    });

    test('rejects plain http non-loopback', () {
      expect(
        () => validateEndpoint('http://example.com'),
        throwsA(isA<ValidationException>()),
      );
    });
  });

  group('validToken', () {
    test('length bounds', () {
      expect(validToken('x' * 31), isFalse);
      expect(validToken('x' * 32), isTrue);
      expect(validToken('x' * 512), isTrue);
      expect(validToken('x' * 513), isFalse);
    });
  });

  group('validId', () {
    test('allows android-owner style ids', () {
      expect(validId('android-owner'), isTrue);
      expect(validId('windows-owner'), isTrue);
    });
  });

  group('fleet inputs', () {
    test('bot ids stay on the safe subset', () {
      expect(validBotId('notes'), isTrue);
      expect(validBotId('com'), isTrue);
      expect(validBotId('main'), isFalse);
      expect(validBotId('con'), isFalse);
      expect(validBotId('com1'), isFalse);
      expect(validBotId('lpt9'), isFalse);
      expect(validBotId('Notes'), isFalse);
      expect(validBotId('../notes'), isFalse);
      expect(validBotId(''), isFalse);
    });

    test('tool allowlist excludes delegate and accepts the defaults', () {
      final seen = <String>{};
      for (final tool in specialistTools) {
        expect(seen.add(tool), isTrue, reason: tool);
        expect(tool, isNot('delegate'));
      }
      for (final tool in defaultSpecialistTools) {
        expect(specialistTools, contains(tool));
      }
      expect(() => validateToolList(['skill_list', 'rag_search']), returnsNormally);
      expect(() => validateToolList([]), returnsNormally);
      expect(
        () => validateToolList(['delegate']),
        throwsA(isA<ValidationException>()),
      );
      expect(
        () => validateToolList(['../secret']),
        throwsA(isA<ValidationException>()),
      );
      expect(
        () => validateToolList(List.filled(33, 'skill_list')),
        throwsA(isA<ValidationException>()),
      );
    });

    test('skill lists and conversation parents', () {
      expect(parseSkillList('ask, notes'), ['ask', 'notes']);
      expect(parseSkillList(''), isEmpty);
      expect(() => parseSkillList('Ask'), throwsA(isA<ValidationException>()));
      expect(() => parseSkillList('../ask'), throwsA(isA<ValidationException>()));
      expect(() => parseSkillList('ask ask'), throwsA(isA<ValidationException>()));
      expect(
        () => validateConversationParent(ConversationKind.section, null),
        returnsNormally,
      );
      expect(
        () => validateConversationParent(ConversationKind.section, 'sec:finance'),
        throwsA(isA<ValidationException>()),
      );
      expect(
        () => validateConversationParent(ConversationKind.group, null),
        throwsA(isA<ValidationException>()),
      );
      expect(
        () => validateConversationParent(ConversationKind.group, 'sec:finance'),
        returnsNormally,
      );
      expect(
        () => validateConversationParent(ConversationKind.group, 'grp:ops'),
        throwsA(isA<ValidationException>()),
      );
      expect(
        () => validateConversationParent(ConversationKind.direct, null),
        returnsNormally,
      );
      expect(
        () => validateConversationParent(ConversationKind.direct, 'grp:ops'),
        returnsNormally,
      );
      expect(
        () => validateConversationParent(ConversationKind.room, 'dir:analyst'),
        throwsA(isA<ValidationException>()),
      );
      expect(conversationNodeId(ConversationKind.group, 'ops'), 'grp:ops');
      expect(conversationNodeId(ConversationKind.section, 'Finance'), isNull);
      expect(validBotDescription('line\nok\t'), isTrue);
      expect(validBotDescription('a\u0000b'), isFalse);
      expect(validBotDescription('é' * 2001), isFalse);
    });
  });
}
