import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../api_client.dart';
import '../build_config.dart';
import '../session.dart';
import '../validation.dart';

class ConnectScreen extends StatefulWidget {
  const ConnectScreen({super.key, required this.session, required this.onConnected});

  final OwnerSession session;
  final VoidCallback onConnected;

  @override
  State<ConnectScreen> createState() => _ConnectScreenState();
}

class _ConnectScreenState extends State<ConnectScreen> {
  final _urlController = TextEditingController(text: BuildConfig.initialServerUrl());
  final _tokenController = TextEditingController();
  final _conversationController = TextEditingController(text: 'android-owner');
  final _inviteController = TextEditingController();
  String? _error;
  bool _obscureToken = true;

  /// Sign-in providers reported by the server, in server order. Empty until
  /// the first successful fetch; the token path works regardless.
  List<AuthProvider> _providers = const [];
  bool _loadingProviders = false;
  String? _signInError;
  String? _signInUrl;

  @override
  void dispose() {
    _urlController.dispose();
    _tokenController.dispose();
    _conversationController.dispose();
    _inviteController.dispose();
    super.dispose();
  }

  /// Fetch the provider list without a session. A failure is silent: the
  /// token-paste path must keep working when the server is unreachable or
  /// older than this build.
  Future<void> _loadProviders() async {
    setState(() => _loadingProviders = true);
    try {
      final providers = await ApiClient.authProviders(_urlController.text.trim());
      if (!mounted) {
        return;
      }
      setState(() {
        _providers = providers;
        _signInError = null;
      });
    } on ApiException {
      if (!mounted) {
        return;
      }
      setState(() => _providers = const []);
    } finally {
      if (mounted) {
        setState(() => _loadingProviders = false);
      }
    }
  }

  /// Ask the server for an authorize URL. The user opens it in the browser;
  /// the browser then shows the session token once, for pasting below.
  Future<void> _startSignIn(String provider) async {
    setState(() {
      _signInError = null;
      _signInUrl = null;
    });
    try {
      final url = await ApiClient.startSignIn(
        _urlController.text.trim(),
        provider: provider,
        inviteCode: _inviteController.text,
      );
      if (!mounted) {
        return;
      }
      setState(() => _signInUrl = url);
    } on ApiException catch (e) {
      if (!mounted) {
        return;
      }
      setState(() => _signInError = e.messageForUser());
    } catch (_) {
      if (!mounted) {
        return;
      }
      setState(() => _signInError = 'Sign-in could not be started.');
    }
  }

  Future<void> _copySignInUrl() async {
    final url = _signInUrl;
    if (url == null) {
      return;
    }
    await Clipboard.setData(ClipboardData(text: url));
    if (!mounted) {
      return;
    }
    ScaffoldMessenger.of(context).showSnackBar(
      const SnackBar(content: Text('Sign-in link copied. Open it in your browser.')),
    );
  }

  Future<void> _connect() async {
    setState(() => _error = null);
    final conv = _conversationController.text.trim();
    if (!validId(conv)) {
      setState(() {
        _error =
            'Conversation id must be 1–128 ASCII letters, digits, underscores, colons or hyphens.';
      });
      return;
    }
    try {
      await widget.session.connect(
        _urlController.text.trim(),
        _tokenController.text,
      );
      widget.session.setConversationId(conv);
      widget.onConnected();
    } on ApiException catch (e) {
      setState(() => _error = e.messageForUser());
    } on ValidationException catch (e) {
      setState(() => _error = e.message);
    } catch (_) {
      setState(() => _error = 'Connection failed.');
    }
  }

  @override
  Widget build(BuildContext context) {
    final busy = widget.session.busy;
    return Scaffold(
      appBar: AppBar(title: const Text('SeraBot')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          Text(
            'Owner console',
            style: Theme.of(context).textTheme.headlineSmall,
          ),
          const SizedBox(height: 8),
          Text(
            'HTTPS origin, or exact loopback HTTP for an SSH tunnel '
            '(http://127.0.0.1:8787). Token stays in memory. '
            'This build does not embed the ${BuildConfig.ownerTokenSecretName} secret.',
          ),
          const SizedBox(height: 24),
          TextField(
            controller: _urlController,
            decoration: const InputDecoration(
              labelText: 'Server URL',
              hintText: 'https://… or http://127.0.0.1:8787',
              border: OutlineInputBorder(),
            ),
            autocorrect: false,
            enableSuggestions: false,
            keyboardType: TextInputType.url,
            onSubmitted: (_) => _loadProviders(),
          ),
          const SizedBox(height: 20),
          Row(
            children: [
              Text(
                'Sign in',
                style: Theme.of(context).textTheme.titleMedium,
              ),
              const SizedBox(width: 8),
              if (_loadingProviders)
                const SizedBox(
                  height: 14,
                  width: 14,
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
            ],
          ),
          const SizedBox(height: 4),
          Align(
            alignment: Alignment.centerLeft,
            child: TextButton(
              onPressed: _loadingProviders ? null : _loadProviders,
              child: Text(
                _providers.isEmpty
                    ? 'Check sign-in options'
                    : 'Refresh sign-in options',
              ),
            ),
          ),
          const SizedBox(height: 4),
          for (final provider in _providers) ...[
            Padding(
              padding: const EdgeInsets.only(bottom: 8),
              child: provider.enabled
                  ? OutlinedButton.icon(
                      onPressed: () => _startSignIn(provider.id),
                      icon: const Icon(Icons.login),
                      label: Text('Sign in with ${provider.label}'),
                    )
                  : OutlinedButton.icon(
                      onPressed: null,
                      icon: const Icon(Icons.lock_outline),
                      label: Text('${provider.label} - coming soon'),
                    ),
            ),
          ],
          if (_providers.any((provider) => provider.enabled)) ...[
            const SizedBox(height: 4),
            TextField(
              controller: _inviteController,
              decoration: const InputDecoration(
                labelText: 'Invite code (new accounts only)',
                border: OutlineInputBorder(),
              ),
              autocorrect: false,
              enableSuggestions: false,
            ),
          ],
          if (_signInUrl != null) ...[
            const SizedBox(height: 12),
            SelectableText(
              _signInUrl!,
              style: Theme.of(context).textTheme.bodySmall,
            ),
            const SizedBox(height: 8),
            OutlinedButton.icon(
              onPressed: _copySignInUrl,
              icon: const Icon(Icons.copy),
              label: const Text('Copy sign-in link'),
            ),
            const SizedBox(height: 4),
            Text(
              'Open the link in your browser, finish signing in, then paste '
              'the session token the browser shows into the field below.',
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ],
          if (_signInError != null) ...[
            const SizedBox(height: 8),
            Text(
              _signInError!,
              style: TextStyle(color: Theme.of(context).colorScheme.error),
            ),
          ],
          const SizedBox(height: 20),
          Text(
            'Or paste an API token',
            style: Theme.of(context).textTheme.titleMedium,
          ),
          const SizedBox(height: 8),
          TextField(
            controller: _tokenController,
            decoration: InputDecoration(
              labelText: 'API token',
              border: const OutlineInputBorder(),
              suffixIcon: IconButton(
                icon: Icon(
                  _obscureToken ? Icons.visibility : Icons.visibility_off,
                ),
                onPressed: () => setState(() => _obscureToken = !_obscureToken),
              ),
            ),
            obscureText: _obscureToken,
            autocorrect: false,
            enableSuggestions: false,
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _conversationController,
            decoration: const InputDecoration(
              labelText: 'Conversation id',
              border: OutlineInputBorder(),
            ),
            autocorrect: false,
            enableSuggestions: false,
          ),
          if (_error != null) ...[
            const SizedBox(height: 12),
            Text(_error!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
          ],
          const SizedBox(height: 24),
          FilledButton(
            onPressed: busy ? null : _connect,
            child: busy
                ? const SizedBox(
                    height: 20,
                    width: 20,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Text('Connect'),
          ),
        ],
      ),
    );
  }
}
