// Alerts and delivery settings (`GET /v1/alerts`, `PUT /v1/alerts`,
// `POST /v1/alerts/digest`; 0.68 to 0.70). Only the caller's own settings and
// sent alerts appear here. Every field is optional on the wire: a missing or
// wrong-typed value falls back to its default, so the screen never breaks on
// an older or newer server.

import 'usage_insights_models.dart'
    show insightInt, insightOptText, insightRows, insightText;

const _maxAlertText = 2000;

/// Alert text keeps line breaks (digests and briefs are multi-line) but drops
/// other control characters. At most [max] code points.
String alertText(Object? v, [int max = _maxAlertText]) {
  if (v is! String) {
    return '';
  }
  final kept = v.runes
      .where(
        (r) =>
            r == 0x0a || r == 0x09 || (r > 0x1f && !(r >= 0x7f && r <= 0x9f)),
      )
      .take(max);
  return String.fromCharCodes(kept).trim();
}

Map<String, dynamic> _map(Object? v) =>
    v is Map<String, dynamic> ? v : const <String, dynamic>{};

bool _flag(Object? v, [bool fallback = false]) => v is bool ? v : fallback;

bool? _optFlag(Object? v) => v is bool ? v : null;

class AlertChoice {
  const AlertChoice({required this.conversationId, required this.label});

  final String conversationId;
  final String label;

  factory AlertChoice.fromJson(Map<String, dynamic> json) => AlertChoice(
    conversationId: insightText(json['conversation_id'], 200),
    label: insightText(json['label'], 60),
  );
}

class AlertRecent {
  const AlertRecent({required this.at, required this.kind, required this.text});

  final String at;
  final String kind;
  final String text;

  factory AlertRecent.fromJson(Map<String, dynamic> json) => AlertRecent(
    at: insightText(json['at'], 40),
    kind: insightText(json['kind'], 60),
    text: alertText(json['text']),
  );
}

class AlertThresholds {
  const AlertThresholds({
    this.diskPercent = 0,
    this.memoryPercent = 0,
    this.failoversIn15Min = 0,
    this.cooldownMins = 0,
  });

  final int diskPercent;
  final int memoryPercent;
  final int failoversIn15Min;
  final int cooldownMins;

  factory AlertThresholds.fromJson(Map<String, dynamic> json) =>
      AlertThresholds(
        diskPercent: insightInt(json['disk_percent']),
        memoryPercent: insightInt(json['memory_percent']),
        failoversIn15Min: insightInt(json['failovers_in_15_min']),
        cooldownMins: insightInt(json['cooldown_mins']),
      );
}

/// What the PUT body carries. The 0.70 fields are only sent when the server
/// reported them, because an older server rejects unknown fields.
class AlertsUpdate {
  const AlertsUpdate({
    required this.telegram,
    this.whatsappConversationId,
    required this.autoSendFiles,
    required this.weeklyDigest,
    this.morningBrief,
    this.emailTriage,
  });

  final bool telegram;
  final String? whatsappConversationId;
  final bool autoSendFiles;
  final bool weeklyDigest;
  final bool? morningBrief;
  final bool? emailTriage;

  static const Object _keep = Object();

  /// [whatsappConversationId] takes a String, null (clear) or nothing (keep).
  AlertsUpdate copyWith({
    bool? telegram,
    Object? whatsappConversationId = _keep,
    bool? autoSendFiles,
    bool? weeklyDigest,
    bool? morningBrief,
    bool? emailTriage,
  }) => AlertsUpdate(
    telegram: telegram ?? this.telegram,
    whatsappConversationId: identical(whatsappConversationId, _keep)
        ? this.whatsappConversationId
        : whatsappConversationId as String?,
    autoSendFiles: autoSendFiles ?? this.autoSendFiles,
    weeklyDigest: weeklyDigest ?? this.weeklyDigest,
    morningBrief: morningBrief ?? this.morningBrief,
    emailTriage: emailTriage ?? this.emailTriage,
  );

  Map<String, dynamic> toJson() => {
    'telegram': telegram,
    'whatsapp_conversation_id': whatsappConversationId,
    'auto_send_files': autoSendFiles,
    'weekly_digest': weeklyDigest,
    if (morningBrief != null) 'morning_brief': morningBrief,
    if (emailTriage != null) 'email_triage': emailTriage,
  };
}

class AlertsView {
  const AlertsView({
    this.enabled = false,
    this.telegram = false,
    this.telegramLinked = false,
    this.whatsappConversationId,
    this.whatsappChoices = const [],
    this.autoSendFiles = false,
    this.weeklyDigest = false,
    this.digestEnabled = false,
    this.recent = const [],
    this.thresholds = const AlertThresholds(),
    this.morningBrief,
    this.briefHourUtc,
    this.emailTriage,
  });

  /// Whether the server sends alerts at all.
  final bool enabled;
  final bool telegram;
  final bool telegramLinked;
  final String? whatsappConversationId;
  final List<AlertChoice> whatsappChoices;
  final bool autoSendFiles;
  final bool weeklyDigest;
  final bool digestEnabled;
  final List<AlertRecent> recent;
  final AlertThresholds thresholds;

  /// 0.70 fields. Null when the server did not send them.
  final bool? morningBrief;
  final int? briefHourUtc;
  final bool? emailTriage;

  /// The values the screen starts from, ready to change and save.
  AlertsUpdate toUpdate() => AlertsUpdate(
    telegram: telegram,
    whatsappConversationId: whatsappConversationId,
    autoSendFiles: autoSendFiles,
    weeklyDigest: weeklyDigest,
    morningBrief: morningBrief,
    emailTriage: emailTriage,
  );

  factory AlertsView.fromJson(Map<String, dynamic> json) {
    final settings = _map(json['settings']);
    final digest = _map(json['digest']);
    final hour = json['brief_hour_utc'] ?? settings['brief_hour_utc'];
    return AlertsView(
      enabled: _flag(json['enabled']),
      telegram: _flag(settings['telegram']),
      telegramLinked: _flag(json['telegram_linked']),
      whatsappConversationId: insightOptText(
        settings['whatsapp_conversation_id'],
        200,
      ),
      whatsappChoices: insightRows(json['whatsapp_choices'])
          .map(AlertChoice.fromJson)
          .where((c) => c.conversationId.isNotEmpty)
          .toList(),
      autoSendFiles: _flag(json['auto_send_files']),
      weeklyDigest: _flag(json['weekly_digest']),
      digestEnabled: _flag(digest['enabled']),
      recent: insightRows(json['recent']).map(AlertRecent.fromJson).toList(),
      thresholds: AlertThresholds.fromJson(_map(json['thresholds'])),
      morningBrief: _optFlag(
        json['morning_brief'] ?? settings['morning_brief'],
      ),
      briefHourUtc: hour is num ? insightInt(hour) : null,
      emailTriage: _optFlag(json['email_triage'] ?? settings['email_triage']),
    );
  }
}

/// The digest sent now (`POST /v1/alerts/digest`): its text and whether an
/// alert channel received it.
class DigestPreview {
  const DigestPreview({required this.text, required this.delivered});

  final String text;
  final bool delivered;

  factory DigestPreview.fromJson(Map<String, dynamic> json) => DigestPreview(
    text: alertText(json['text']),
    delivered: _flag(json['delivered']),
  );
}
