import 'package:flutter/material.dart';

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
  String? _error;
  bool _obscureToken = true;

  @override
  void dispose() {
    _urlController.dispose();
    _tokenController.dispose();
    _conversationController.dispose();
    super.dispose();
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
          ),
          const SizedBox(height: 12),
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
