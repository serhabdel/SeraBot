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

/// One connected mailbox in `GET /v1/mail`'s `accounts` list (0.74.4);
/// [account] is empty for the default mailbox. Never carries a password.
class MailAccountStatus {
  const MailAccountStatus({
    this.account = '',
    this.smtpHost,
    this.imapHost,
    this.username,
  });

  factory MailAccountStatus.fromJson(Map<String, dynamic> json) {
    return MailAccountStatus(
      account: json['account'] as String? ?? '',
      smtpHost: json['smtp_host'] as String?,
      imapHost: json['imap_host'] as String?,
      username: json['username'] as String?,
    );
  }

  final String account;
  final String? smtpHost;
  final String? imapHost;
  final String? username;
}

/// `GET /v1/mail`: per-user SMTP/IMAP connection status. Never carries a
/// password. The top-level fields describe the default mailbox; [accounts]
/// lists every connected mailbox (default + named accounts, 0.74.4).
class MailStatus {
  const MailStatus({
    this.connected = false,
    this.smtpHost,
    this.imapHost,
    this.username,
    this.accounts = const [],
  });

  factory MailStatus.fromJson(Map<String, dynamic> json) {
    final accounts = (json['accounts'] as List? ?? const [])
        .whereType<Map<String, dynamic>>()
        .map(MailAccountStatus.fromJson)
        .toList();
    return MailStatus(
      connected: json['connected'] as bool? ?? false,
      smtpHost: json['smtp_host'] as String?,
      imapHost: json['imap_host'] as String?,
      username: json['username'] as String?,
      accounts: accounts,
    );
  }

  final bool connected;
  final String? smtpHost;
  final String? imapHost;
  final String? username;
  final List<MailAccountStatus> accounts;
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
    this.account,
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

  /// Named account label (0.74.4); null means the default mailbox.
  final String? account;
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
    final a = account?.trim();
    return {
      if (a != null && a.isNotEmpty) 'account': a,
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
  final account = req.account?.trim();
  if (account != null &&
      account.isNotEmpty &&
      (account.length > 24 ||
          !RegExp(r'^[a-z0-9_-]+$').hasMatch(account))) {
    return "Account name must be 1-24 chars: lowercase letters, digits, '-' or '_'.";
  }
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
    this.account,
    this.smtpHost,
    this.imapHost,
    this.username,
    this.smtpTest = MailProbeOutcome.ok,
    this.imapTest = MailProbeOutcome.ok,
  });

  factory MailConnectResponse.fromJson(Map<String, dynamic> json) {
    return MailConnectResponse(
      connected: json['connected'] as bool? ?? false,
      account: json['account'] as String?,
      smtpHost: json['smtp_host'] as String?,
      imapHost: json['imap_host'] as String?,
      username: json['username'] as String?,
      smtpTest: MailProbeOutcome.fromJson(json['smtp_test']),
      imapTest: MailProbeOutcome.fromJson(json['imap_test']),
    );
  }

  final bool connected;
  final String? account;
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

/// One attachment of a read mail message (`GET /v1/mail/messages/{uid}`, 0.74.5).
class MailAttachmentInfo {
  const MailAttachmentInfo({
    required this.name,
    this.contentType = 'application/octet-stream',
    this.size = 0,
    this.dataBase64,
  });

  factory MailAttachmentInfo.fromJson(Map<String, dynamic> json) {
    return MailAttachmentInfo(
      name: (json['name'] as String? ?? 'attachment').trim(),
      contentType:
          (json['content_type'] as String? ?? 'application/octet-stream').trim(),
      size: (json['size'] as num?)?.toInt() ?? 0,
      dataBase64: json['data_base64'] as String?,
    );
  }

  final String name;
  final String contentType;
  final int size;
  final String? dataBase64;
}

/// One mail message as returned by `GET /v1/mail/messages*` and
/// `GET /v1/mail/search`.
class MailMessageInfo {
  const MailMessageInfo({
    required this.uid,
    this.subject = '',
    this.from = '',
    this.date = '',
    this.flags = const [],
    this.body = '',
    this.attachments = const [],
    this.snippet = '',
    this.threadKey = '',
    this.hasAttachments = false,
    this.unsubscribeMailto,
    this.unsubscribeUrl,
  });

  factory MailMessageInfo.fromJson(Map<String, dynamic> json) {
    final rawFlags = json['flags'] as List? ?? const [];
    final rawAttachments = json['attachments'] as List? ?? const [];
    final rawUnsubscribe = json['unsubscribe'];
    final unsubscribe = rawUnsubscribe is Map<String, dynamic>
        ? rawUnsubscribe
        : const <String, dynamic>{};
    return MailMessageInfo(
      uid: (json['uid'] as num?)?.toInt() ?? 0,
      subject: json['subject'] as String? ?? '',
      from: json['from'] as String? ?? '',
      date: json['date'] as String? ?? '',
      flags: rawFlags.whereType<String>().toList(),
      body: json['body'] as String? ?? '',
      attachments: rawAttachments
          .whereType<Map<String, dynamic>>()
          .map(MailAttachmentInfo.fromJson)
          .toList(),
      snippet: json['snippet'] as String? ?? '',
      threadKey: json['thread_key'] as String? ?? '',
      hasAttachments: json['has_attachments'] as bool? ?? false,
      unsubscribeMailto: unsubscribe['mailto'] as String?,
      unsubscribeUrl: unsubscribe['url'] as String?,
    );
  }

  final int uid;
  final String subject;
  final String from;
  final String date;
  final List<String> flags;
  final String body;
  final List<MailAttachmentInfo> attachments;

  /// First words of the body, for list rows (0.74.14).
  final String snippet;

  /// Messages of one conversation share a key (0.74.14).
  final String threadKey;

  /// A list fetch may not carry the attachments themselves, only this hint.
  final bool hasAttachments;

  /// `List-Unsubscribe` targets, when the sender is a mailing list (0.74.14).
  final String? unsubscribeMailto;
  final String? unsubscribeUrl;

  bool get hasFiles => hasAttachments || attachments.isNotEmpty;

  bool get isSeen => flags.any((f) {
    final lower = f.toLowerCase();
    return lower == r'\seen' || lower == 'seen';
  });

  bool get isFlagged => flags.any((f) {
    final lower = f.toLowerCase();
    return lower == r'\flagged' || lower == 'flagged';
  });

  MailMessageInfo copyWith({
    List<String>? flags,
    String? body,
    List<MailAttachmentInfo>? attachments,
  }) {
    return MailMessageInfo(
      uid: uid,
      subject: subject,
      from: from,
      date: date,
      flags: flags ?? this.flags,
      body: body ?? this.body,
      attachments: attachments ?? this.attachments,
      snippet: snippet,
      threadKey: threadKey,
      hasAttachments: hasAttachments,
      unsubscribeMailto: unsubscribeMailto,
      unsubscribeUrl: unsubscribeUrl,
    );
  }

  /// Returns a copy with `\Seen` and/or `\Flagged` updated locally.
  MailMessageInfo withUpdatedFlags({bool? seen, bool? flagged}) {
    final next = List<String>.from(flags);
    if (seen != null) {
      next.removeWhere((f) {
        final l = f.toLowerCase();
        return l == r'\seen' || l == 'seen';
      });
      if (seen) {
        next.add(r'\Seen');
      }
    }
    if (flagged != null) {
      next.removeWhere((f) {
        final l = f.toLowerCase();
        return l == r'\flagged' || l == 'flagged';
      });
      if (flagged) {
        next.add(r'\Flagged');
      }
    }
    return copyWith(flags: next);
  }
}

/// Reply of `POST /v1/mail/messages/{uid}/attachments/{index}/save-to-library`.
class MailSavedAttachment {
  const MailSavedAttachment({
    this.filename,
    this.hash,
    this.onedrive,
  });

  factory MailSavedAttachment.fromJson(Map<String, dynamic> json) {
    final item = json['item'];
    String? filename = json['filename'] as String? ?? json['name'] as String?;
    String? hash = json['hash'] as String? ?? json['id'] as String?;
    if (item is Map<String, dynamic>) {
      filename ??= item['filename'] as String? ?? item['name'] as String?;
      hash ??= item['id'] as String? ?? item['hash'] as String?;
    }
    return MailSavedAttachment(
      filename: filename,
      hash: hash,
      onedrive: json['onedrive'] as String?,
    );
  }

  final String? filename;
  final String? hash;
  final String? onedrive;
}

