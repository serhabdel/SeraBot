import 'package:flutter/material.dart';
import 'package:flutter_web_auth_2/flutter_web_auth_2.dart';

import '../auth.dart';
import '../build_config.dart';
import '../session.dart';
import '../widgets.dart';

class ConnectScreen extends StatefulWidget {
  const ConnectScreen({super.key, required this.session, this.launcher});

  final OwnerSession session;

  /// Opens the authorize URL; defaults to flutter_web_auth_2. Tests inject one.
  final AuthLauncher? launcher;

  @override
  State<ConnectScreen> createState() => _ConnectScreenState();
}

class _ConnectScreenState extends State<ConnectScreen> {
  final _urlController = TextEditingController(text: BuildConfig.initialServerUrl());
  final _inviteController = TextEditingController();
  bool _hasInvite = false;
  String? _error;

  @override
  void dispose() {
    _urlController.dispose();
    _inviteController.dispose();
    super.dispose();
  }

  Future<String> _defaultLauncher(String url) {
    return FlutterWebAuth2.authenticate(
      url: url,
      callbackUrlScheme: signInCallbackScheme,
    );
  }

  Future<void> _signIn() async {
    setState(() => _error = null);
    try {
      await widget.session.signIn(
        _urlController.text.trim(),
        widget.launcher ?? _defaultLauncher,
        inviteCode: _hasInvite ? _inviteController.text : null,
      );
    } on SignInException catch (e) {
      if (mounted) {
        setState(() => _error = e.message);
      }
    } catch (_) {
      if (mounted) {
        setState(() => _error = signInErrorMessage('server'));
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final busy = widget.session.busy;
    final notice = _error ?? widget.session.statusMessage;
    return Scaffold(
      appBar: AppBar(title: const Text('SeraBot')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          Text('Sign in', style: Theme.of(context).textTheme.headlineSmall),
          const SizedBox(height: 8),
          const Text(
            'Enter your SeraBot server (HTTPS, or http://127.0.0.1:8787 '
            'through a tunnel), then sign in with your Microsoft account.',
          ),
          const SizedBox(height: 24),
          TextField(
            controller: _urlController,
            decoration: const InputDecoration(
              labelText: 'Server URL',
              hintText: 'https://example.com or http://127.0.0.1:8787',
              border: OutlineInputBorder(),
            ),
            autocorrect: false,
            enableSuggestions: false,
            keyboardType: TextInputType.url,
          ),
          const SizedBox(height: 12),
          CheckboxListTile(
            contentPadding: EdgeInsets.zero,
            controlAffinity: ListTileControlAffinity.leading,
            title: const Text('I have an invite code'),
            value: _hasInvite,
            onChanged: (v) => setState(() => _hasInvite = v ?? false),
          ),
          if (_hasInvite) ...[
            SecretField(
              controller: _inviteController,
              label: 'Invite code',
            ),
            const SizedBox(height: 12),
          ],
          if (_error != null) ...[
            ErrorBanner(_error!, onRetry: busy ? null : _signIn),
            const SizedBox(height: 12),
          ] else if (notice != null) ...[
            Text(
              notice,
              style: TextStyle(color: Theme.of(context).colorScheme.error),
            ),
            const SizedBox(height: 12),
          ],
          const SizedBox(height: 12),
          FilledButton.icon(
            onPressed: busy ? null : _signIn,
            icon: busy
                ? const SizedBox(
                    height: 20,
                    width: 20,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.login),
            label: const Text('Sign in with Microsoft'),
          ),
        ],
      ),
    );
  }
}
