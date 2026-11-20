import 'package:flutter_test/flutter_test.dart';
import 'package:serabot_mobile/alerts_models.dart';

/// Synthetic GET /v1/alerts body with every 0.70 field present.
Map<String, dynamic> _fullBody() => {
  'enabled': true,
  'settings': {
    'telegram': true,
    'whatsapp_conversation_id': 'conv_synthetic_1',
    'morning_brief': true,
  },
  'auto_send_files': true,
  'weekly_digest': false,
  'digest': {'enabled': true, 'weekday': 'mon', 'hour_utc': 8},
  'telegram_linked': true,
  'whatsapp_choices': [
    {'conversation_id': 'conv_synthetic_1', 'label': '+15550001111'},
    {'conversation_id': '', 'label': 'skipped, no id'},
    {'conversation_id': 'conv_synthetic_2', 'label': '+15550002222'},
  ],
  'recent': [
    {
      'at': '2026-11-20T09:05:00Z',
      'kind': 'failover',
      'text': 'Model failover on the main bot.\nTwo providers tried.',
    },
    {'at': 42, 'kind': null, 'text': 7},
  ],
  'thresholds': {
    'disk_percent': 85,
    'memory_percent': 90,
    'failovers_in_15_min': 3,
    'cooldown_mins': 60,
  },
  'morning_brief': true,
  'brief_hour_utc': 7,
  'email_triage': false,
};

void main() {
  group('AlertsView.fromJson', () {
    test('reads every field of a 0.70 server', () {
      final view = AlertsView.fromJson(_fullBody());

      expect(view.enabled, isTrue);
      expect(view.telegram, isTrue);
      expect(view.telegramLinked, isTrue);
      expect(view.whatsappConversationId, 'conv_synthetic_1');
      expect(view.autoSendFiles, isTrue);
      expect(view.weeklyDigest, isFalse);
      expect(view.digestEnabled, isTrue);
      expect(view.thresholds.diskPercent, 85);
      expect(view.thresholds.cooldownMins, 60);
      expect(view.morningBrief, isTrue);
      expect(view.briefHourUtc, 7);
      expect(view.emailTriage, isFalse);
    });

    test('drops choices without an id and keeps labels short', () {
      final view = AlertsView.fromJson(_fullBody());

      expect(view.whatsappChoices.map((c) => c.conversationId), [
        'conv_synthetic_1',
        'conv_synthetic_2',
      ]);
      expect(view.whatsappChoices.first.label, '+15550001111');
    });

    test('keeps recent alerts with multi-line text and defaults bad types', () {
      final view = AlertsView.fromJson(_fullBody());

      expect(view.recent, hasLength(2));
      expect(view.recent.first.kind, 'failover');
      expect(view.recent.first.text, contains('\n'));
      expect(view.recent.last.at, '', reason: 'a number is not a timestamp');
      expect(view.recent.last.kind, '');
      expect(view.recent.last.text, '');
    });

    test('0.70 fields are null when an older server omits them', () {
      final body = _fullBody()
        ..remove('morning_brief')
        ..remove('brief_hour_utc')
        ..remove('email_triage');
      (body['settings'] as Map<String, dynamic>).remove('morning_brief');
      final view = AlertsView.fromJson(body);

      expect(view.morningBrief, isNull);
      expect(view.briefHourUtc, isNull);
      expect(view.emailTriage, isNull);
    });

    test('an empty or garbage body falls back to safe defaults', () {
      final empty = AlertsView.fromJson(const {});
      expect(empty.enabled, isFalse);
      expect(empty.telegram, isFalse);
      expect(empty.whatsappChoices, isEmpty);
      expect(empty.recent, isEmpty);
      expect(empty.whatsappConversationId, isNull);
      expect(empty.thresholds.diskPercent, 0);

      final garbage = AlertsView.fromJson({
        'enabled': 'yes',
        'settings': 'nope',
        'whatsapp_choices': 'nope',
        'recent': 12,
        'thresholds': [1, 2],
        'brief_hour_utc': 'seven',
        'morning_brief': 1,
      });
      expect(garbage.enabled, isFalse);
      expect(garbage.telegram, isFalse);
      expect(garbage.whatsappChoices, isEmpty);
      expect(garbage.recent, isEmpty);
      expect(garbage.morningBrief, isNull, reason: 'a 1 is not a flag');
      expect(garbage.briefHourUtc, isNull);
    });
  });

  group('AlertsUpdate.toJson', () {
    test('sends only the base fields when the server omitted 0.70 fields', () {
      final body = _fullBody()
        ..remove('morning_brief')
        ..remove('email_triage');
      (body['settings'] as Map<String, dynamic>).remove('morning_brief');
      final json = AlertsView.fromJson(body).toUpdate().toJson();

      expect(json, {
        'telegram': true,
        'whatsapp_conversation_id': 'conv_synthetic_1',
        'auto_send_files': true,
        'weekly_digest': false,
      });
      expect(json.containsKey('morning_brief'), isFalse);
      expect(json.containsKey('email_triage'), isFalse);
    });

    test('sends the 0.70 switches when the server reported them', () {
      final json = AlertsView.fromJson(_fullBody()).toUpdate().toJson();

      expect(json['morning_brief'], isTrue);
      expect(json['email_triage'], isFalse);
    });

    test('copyWith clears the WhatsApp chat only when asked to', () {
      final base = AlertsView.fromJson(_fullBody()).toUpdate();

      expect(
        base.copyWith(telegram: false).toJson()['whatsapp_conversation_id'],
        'conv_synthetic_1',
        reason: 'omitting the chat keeps it',
      );
      expect(
        base
            .copyWith(whatsappConversationId: null)
            .toJson()['whatsapp_conversation_id'],
        isNull,
      );
      expect(
        base
            .copyWith(whatsappConversationId: 'conv_synthetic_2')
            .toJson()['whatsapp_conversation_id'],
        'conv_synthetic_2',
      );
    });
  });

  group('DigestPreview.fromJson', () {
    test('reads the text and delivery flag', () {
      final preview = DigestPreview.fromJson({
        'text': 'Week in review\nFour tasks done.',
        'delivered': true,
      });
      expect(preview.text, contains('Four tasks done.'));
      expect(preview.delivered, isTrue);
    });

    test('defaults to an empty, undelivered preview', () {
      final preview = DigestPreview.fromJson(const {'text': 5});
      expect(preview.text, '');
      expect(preview.delivered, isFalse);
    });
  });

  group('alertText', () {
    test('keeps line breaks and drops other control characters', () {
      expect(
        alertText('one\ntwo\tthree\u0007four\u009b'),
        'one\ntwo\tthreefour',
      );
    });

    test('caps the length in code points', () {
      expect(alertText('abcdef', 3), 'abc');
      expect(alertText(null), '');
    });
  });
}
