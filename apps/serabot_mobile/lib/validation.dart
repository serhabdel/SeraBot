import 'models.dart';

// Input validation mirroring `crates/serabot-desktop/src/api.rs`.

class ValidationException implements Exception {
  ValidationException(this.message);
  final String message;

  @override
  String toString() => message;
}

const _endpointHelp =
    'Use an HTTPS server origin, or exact HTTP localhost / 127.0.0.1 / [::1]. '
    'No credentials, path, query or fragment.';

/// Returns normalized origin (scheme + authority, no path).
String validateEndpoint(String raw) {
  const invalid = _endpointHelp;
  for (final c in raw.runes) {
    if (c <= 0x20 || c == 0x7f || c == 0x5c) {
      throw ValidationException(invalid);
    }
    if (c == 0x09 || c == 0x0a || c == 0x0d || c == 0x20) {
      throw ValidationException(invalid);
    }
  }

  final schemeSplit = raw.split('://');
  if (schemeSplit.length != 2) {
    throw ValidationException(invalid);
  }
  final scheme = schemeSplit[0];
  final rest = schemeSplit[1];
  if (scheme != 'http' && scheme != 'https') {
    throw ValidationException(invalid);
  }

  final slash = rest.indexOf('/');
  final authority = slash == -1 ? rest : rest.substring(0, slash);
  if (authority.contains('@')) {
    throw ValidationException(invalid);
  }

  late final Uri uri;
  try {
    uri = Uri.parse(raw);
  } catch (_) {
    throw ValidationException(invalid);
  }

  if (uri.host.isEmpty ||
      uri.userInfo.isNotEmpty ||
      uri.query.isNotEmpty ||
      uri.fragment.isNotEmpty ||
      (uri.path.isNotEmpty && uri.path != '/')) {
    throw ValidationException(invalid);
  }

  if (scheme == 'http') {
    String host;
    if (authority.startsWith('[')) {
      final end = authority.indexOf(']');
      if (end == -1) {
        throw ValidationException(invalid);
      }
      host = authority.substring(0, end + 1);
    } else {
      host = authority.split(':').first;
    }
    if (host != 'localhost' && host != '127.0.0.1' && host != '[::1]') {
      throw ValidationException(invalid);
    }
  }

  return '${uri.scheme}://${uri.authority}';
}

bool validId(String id) {
  if (id.isEmpty || id.length > 128) {
    return false;
  }
  return id.runes.every((r) {
    if (r > 0x7f) {
      return false;
    }
    final b = r;
    return (b >= 0x30 && b <= 0x39) ||
        (b >= 0x41 && b <= 0x5a) ||
        (b >= 0x61 && b <= 0x7a) ||
        b == 0x5f ||
        b == 0x3a ||
        b == 0x2d;
  });
}

bool validToken(String token) {
  if (token.length < 32 || token.length > 512) {
    return false;
  }
  return token.codeUnits.every((b) => b >= 0x21 && b <= 0x7e);
}

bool validText(String text) {
  return text.trim().isNotEmpty && text.runes.length <= 8000;
}

/// Specialist tools the owner may select. Mirrors the server allowlist.
/// `delegate` is orchestrator-only and is intentionally absent.
const specialistTools = <String>[
  'skill_list',
  'skill_search',
  'skill_load',
  'rag_search',
  'xlsx_analyze',
  'timesheet_analyze',
  'document_index',
  'fetch_url',
  'watch_now',
  'audio_transcribe',
  'audio_understand',
  'firecrawl_scrape',
  'firecrawl_map',
  'firecrawl_search',
  'firecrawl_crawl',
  'elevenlabs_list_voices',
  'elevenlabs_text_to_speech',
  'elevenlabs_sound_effect',
  'elevenlabs_transcribe',
  'elevenlabs_generate_image',
  'elevenlabs_generate_video',
  'deck_build',
  'telegram_send_document',
  'onedrive_upload',
  'onedrive_list',
  'browser_session_start',
  'browser_session_stop',
  'browser_navigate',
  'browser_snapshot',
  'google_drive_list',
  'google_sheets_read',
  'google_calendar_list_events',
  'gmail_list_messages',
  'outlook_list_messages',
  'outlook_calendar_list_events',
  'notion_query_database',
  'agentmail_inboxes_list',
  'agentmail_send',
];

const defaultSpecialistTools = <String>[
  'skill_list',
  'skill_search',
  'skill_load',
];

const _maxBotDescriptionChars = 2000;
const _maxScaffoldTools = 32;
const _maxScaffoldSkills = 16;

/// Lowercase slug shared by bot ids and conversation slugs (`^[a-z][a-z0-9-]{0,31}$`).
bool validSlug(String slug) {
  if (slug.isEmpty || slug.length > 32) {
    return false;
  }
  final first = slug.codeUnitAt(0);
  if (first < 0x61 || first > 0x7a) {
    return false;
  }
  for (var i = 1; i < slug.length; i++) {
    final b = slug.codeUnitAt(i);
    final ok = (b >= 0x61 && b <= 0x7a) || (b >= 0x30 && b <= 0x39) || b == 0x2d;
    if (!ok) {
      return false;
    }
  }
  return true;
}

bool reservedBotId(String id) {
  if (id == 'main' || id == 'con' || id == 'prn' || id == 'aux' || id == 'nul') {
    return true;
  }
  if (id.length == 4 && (id.startsWith('com') || id.startsWith('lpt'))) {
    final last = id.codeUnitAt(3);
    return last >= 0x30 && last <= 0x39;
  }
  return false;
}

bool validBotId(String id) => validSlug(id) && !reservedBotId(id);

bool validSkillId(String id) {
  if (id.isEmpty || id.length > 64) {
    return false;
  }
  final first = id.codeUnitAt(0);
  if (first < 0x61 || first > 0x7a) {
    return false;
  }
  for (var i = 1; i < id.length; i++) {
    final b = id.codeUnitAt(i);
    final ok = (b >= 0x61 && b <= 0x7a) || (b >= 0x30 && b <= 0x39) || b == 0x2d;
    if (!ok) {
      return false;
    }
  }
  return true;
}

bool validBotDescription(String text) {
  if (text.runes.length > _maxBotDescriptionChars) {
    return false;
  }
  for (final r in text.runes) {
    if (r == 0x09 || r == 0x0a) {
      continue;
    }
    if (r <= 0x1f || (r >= 0x7f && r <= 0x9f)) {
      return false;
    }
  }
  return true;
}

void validateToolList(List<String> tools) {
  if (tools.length > _maxScaffoldTools) {
    throw ValidationException('Choose at most 32 specialist tools.');
  }
  final seen = <String>{};
  for (final tool in tools) {
    if (tool == 'delegate') {
      throw ValidationException('Specialists cannot be granted delegate.');
    }
    if (!specialistTools.contains(tool)) {
      throw ValidationException('Choose tools from the specialist list.');
    }
    if (!seen.add(tool)) {
      throw ValidationException('Each tool can be selected once.');
    }
  }
}

/// Comma- or whitespace-separated skill ids. Empty input means no skills.
List<String> parseSkillList(String raw) {
  final out = <String>[];
  for (final part in raw.split(RegExp(r'[\s,]'))) {
    if (part.isEmpty) {
      continue;
    }
    if (out.length >= _maxScaffoldSkills) {
      throw ValidationException('Enter at most 16 skill ids.');
    }
    if (!validSkillId(part)) {
      throw ValidationException('Skill ids are lowercase slugs such as ask.');
    }
    if (out.contains(part)) {
      throw ValidationException('Each skill id can be listed once.');
    }
    out.add(part);
  }
  return out;
}

String? conversationNodeId(ConversationKind kind, String slug) {
  if (!validSlug(slug)) {
    return null;
  }
  return '${kind.idPrefix}:$slug';
}

void validateConversationParent(ConversationKind kind, String? parent) {
  final value = (parent == null || parent.isEmpty) ? null : parent;
  switch (kind) {
    case ConversationKind.section:
      if (value != null) {
        throw ValidationException('Sections cannot have a parent.');
      }
    case ConversationKind.group:
      if (value == null) {
        throw ValidationException(
          'Groups require a section parent such as sec:finance.',
        );
      }
      if (!_scopedParent(value, 'sec')) {
        throw ValidationException(
          'A group parent must be a section id such as sec:finance.',
        );
      }
    case ConversationKind.direct:
    case ConversationKind.room:
      if (value != null &&
          !_scopedParent(value, 'sec') &&
          !_scopedParent(value, 'grp')) {
        throw ValidationException(
          'Direct and room nodes sit under a section (sec:) or group (grp:).',
        );
      }
  }
}

bool _scopedParent(String parent, String prefix) {
  final colon = parent.indexOf(':');
  if (colon < 0) {
    return false;
  }
  final got = parent.substring(0, colon);
  final slug = parent.substring(colon + 1);
  return got == prefix && validSlug(slug) && parent.length <= 128;
}
