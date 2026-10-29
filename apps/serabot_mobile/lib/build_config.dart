class BuildConfig {
  static const defaultServerUrl = String.fromEnvironment(
    'SERABOT_DEFAULT_SERVER_URL',
    defaultValue: '',
  );
}
