import 'validation.dart';

class BuildConfig {
  /// Optional origin from `--dart-define=SERABOT_DEFAULT_SERVER_URL`.
  /// GitHub Actions sets that define from the `SERABOT_OWNER_API_URL` secret.
  static const defaultServerUrl = String.fromEnvironment(
    'SERABOT_DEFAULT_SERVER_URL',
    defaultValue: '',
  );

  /// GitHub Actions secret *name* for the owner token. Never a token value.
  static const ownerTokenSecretName = String.fromEnvironment(
    'SERABOT_OWNER_TOKEN_SECRET_NAME',
    defaultValue: 'SERABOT_OWNER_TOKEN',
  );

  static String initialServerUrl() => initialServerUrlFrom(defaultServerUrl);

  /// Valid origins are prefilled. Empty and invalid hints stay on the generic field.
  static String initialServerUrlFrom(String hinted) {
    final trimmed = hinted.trim();
    if (trimmed.isEmpty) {
      return 'https://';
    }
    try {
      return validateEndpoint(trimmed);
    } on ValidationException {
      return 'https://';
    }
  }
}
