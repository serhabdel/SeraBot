/// Native SMTP/IMAP mail connector (`/v1/mail`, 0.74.1–0.74.2). Mirrors the
/// desktop `api.rs` mail types: status never returns the password, connect
/// runs a live SMTP + IMAP login test per side, disconnect removes the
/// stored credentials.
library;

/// Security mode for a mail connection. Wire values are lowercase.
enum MailSecurity { startTls, ssl, none }

extension MailSecurityX on MailSecurity {
  String get apiValue => switch (this) {
    MailSecurity.startTls => 'starttls',
    MailSecurity.ssl => 'ssl',
    MailSecurity.none => 'none',
  };

  String get label => switch (this) {
    MailSecurity.startTls => 'STARTTLS',
    MailSecurity.ssl => 'SSL / TLS',
    MailSecurity.none => 'None (local only)',
  };

  static MailSecurity parse(String? raw) {
    switch ((raw ?? '').trim().toLowerCase()) {
      case 'ssl':
        return MailSecurity.ssl;
      case 'none':
        return MailSecurity.none;
      default:
        return MailSecurity.startTls;
    }
  }
}

/// `GET /v1/mail`: per-user SMTP/IMAP connection status. Never carries a
/// password.
class MailStatus {
  const MailStatus({
    this.connected = false,
    this.smtpHost,
    this.imapHost,
    this.username,
  });

  factory MailStatus.fromJson(Map<String, dynamic> json) {
    return MailStatus(
      connected: json['connected'] as bool? ?? false,
      smtpHost: json['smtp_host'] as String?,
      imapHost: json['imap_host'] as String?,
      username: json['username'] as String?,
    );
  }

  final bool connected;
  final String? smtpHost;
  final String? imapHost;
  final String? username;
}

/// Outcome of one live probe inside `POST /v1/mail/connect`. The server
/// returns `"ok"` or `{"error": "..."}` (a bare error string is also read
/// as an error, mirroring the desktop parser).
class MailProbeOutcome {
  const MailProbeOutcome._(this.error);

  factory MailProbeOutcome.fromJson(Object? value) {
    if (value is String) {
      return value == 'ok'
          ? const MailProbeOutcome._(null)
          : MailProbeOutcome._(value);
    }
    if (value is Map<String, dynamic>) {
      final err = value['error'];
      if (err is String && err.isNotEmpty) {
        return MailProbeOutcome._(err);
      }
    }
    return const MailProbeOutcome._('Test failed');
  }

  static const ok = MailProbeOutcome._(null);

  /// Null when the probe succeeded, else the server's failure sentence.
  final String? error;

  bool get succeeded => error == null;
}

/// Body of `POST /v1/mail/connect`. The password lives in memory only and
/// is never logged or returned by the server.
class MailConnectRequest {
  const MailConnectRequest({
    required this.smtpHost,
    required this.smtpPort,
    required this.smtpSecurity,
    required this.username,
    required this.password,
    required this.imapHost,
    required this.imapPort,
    required this.imapSecurity,
    this.from,
  });

  /// One-click Hostinger preset (desktop card parity): SMTP 587 STARTTLS,
  /// IMAP 993 SSL. The username/password still come from the member.
  factory MailConnectRequest.hostinger({
    required String username,
    required String password,
    String? from,
  }) {
    return MailConnectRequest(
      smtpHost: 'smtp.hostinger.com',
      smtpPort: 587,
      smtpSecurity: MailSecurity.startTls,
      username: username,
      password: password,
      imapHost: 'imap.hostinger.com',
      imapPort: 993,
      imapSecurity: MailSecurity.ssl,
      from: from,
    );
  }

  final String smtpHost;
  final int smtpPort;
  final MailSecurity smtpSecurity;
  final String username;
  final String password;
  final String imapHost;
  final int imapPort;
  final MailSecurity imapSecurity;
  final String? from;

  Map<String, dynamic> toJson() {
    final f = from?.trim();
    return {
      'smtp_host': smtpHost,
      'smtp_port': smtpPort,
      'smtp_security': smtpSecurity.apiValue,
      'username': username,
      'password': password,
      'imap_host': imapHost,
      'imap_port': imapPort,
      'imap_security': imapSecurity.apiValue,
      if (f != null && f.isNotEmpty) 'from': f,
    };
  }
}

bool _isLoopbackMailHost(String host) =>
    host.trim() == '127.0.0.1' ||
    host.trim() == '::1' ||
    host.trim() == 'localhost';

/// Local validation mirroring the desktop `validate_mail_connect`: returns
/// the user-facing problem, or null when the request may be sent. Plain
/// (non-TLS) connections are refused unless the host is loopback, so
/// credentials never cross a network unencrypted.
String? validateMailConnect(MailConnectRequest req) {
  for (final entry in [
    ('SMTP host', req.smtpHost.trim()),
    ('IMAP host', req.imapHost.trim()),
  ]) {
    final host = entry.$2;
    if (host.isEmpty ||
        host.length > 253 ||
        host.runes.any((r) => r <= 0x1f || r == 0x7f) ||
        RegExp(r'\s').hasMatch(host)) {
      return 'Enter a valid ${entry.$1.toLowerCase()} name.';
    }
  }
  if (req.smtpPort < 1 ||
      req.smtpPort > 65535 ||
      req.imapPort < 1 ||
      req.imapPort > 65535) {
    return 'Ports must be between 1 and 65535.';
  }
  if (req.smtpSecurity == MailSecurity.none &&
      !_isLoopbackMailHost(req.smtpHost)) {
    return 'Plain SMTP is only allowed for localhost; choose STARTTLS or SSL.';
  }
  if (req.imapSecurity == MailSecurity.none &&
      !_isLoopbackMailHost(req.imapHost)) {
    return 'Plain IMAP is only allowed for localhost; choose SSL or STARTTLS.';
  }
  final user = req.username.trim();
  if (user.isEmpty ||
      user.length > 256 ||
      user.runes.any((r) => r <= 0x1f || r == 0x7f)) {
    return 'Enter your email address or mailbox username.';
  }
  if (req.password.isEmpty || req.password.length > 1024) {
    return 'Password must be 1 to 1024 characters.';
  }
  final f = req.from?.trim();
  if (f != null && (f.isEmpty || f.length > 256)) {
    return 'From address must be at most 256 characters.';
  }
  return null;
}

/// Reply of `POST /v1/mail/connect`: the stored status plus the live
/// per-side test results.
class MailConnectResponse {
  const MailConnectResponse({
    this.connected = false,
    this.smtpHost,
    this.imapHost,
    this.username,
    this.smtpTest = MailProbeOutcome.ok,
    this.imapTest = MailProbeOutcome.ok,
  });

  factory MailConnectResponse.fromJson(Map<String, dynamic> json) {
    return MailConnectResponse(
      connected: json['connected'] as bool? ?? false,
      smtpHost: json['smtp_host'] as String?,
      imapHost: json['imap_host'] as String?,
      username: json['username'] as String?,
      smtpTest: MailProbeOutcome.fromJson(json['smtp_test']),
      imapTest: MailProbeOutcome.fromJson(json['imap_test']),
    );
  }

  final bool connected;
  final String? smtpHost;
  final String? imapHost;
  final String? username;
  final MailProbeOutcome smtpTest;
  final MailProbeOutcome imapTest;
}

/// Reply of `DELETE /v1/mail`.
class MailDisconnectResponse {
  const MailDisconnectResponse({this.connected = false});

  factory MailDisconnectResponse.fromJson(Map<String, dynamic> json) {
    return MailDisconnectResponse(
      connected: json['connected'] as bool? ?? false,
    );
  }

  final bool connected;
}
