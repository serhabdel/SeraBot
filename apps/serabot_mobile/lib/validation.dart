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
