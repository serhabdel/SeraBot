import 'package:flutter/material.dart';

import '../api_client.dart';
import '../models.dart';
import '../session.dart';

/// Voice, speech, and OCR settings.
///
/// The status chips come from the server and say whether each capability can
/// actually run; the reason a capability is off names a setting and never a
/// secret. Editing is admin-only — a member sees the status and gets a clear
/// message if they try to save.
class VoiceTab extends StatefulWidget {
  const VoiceTab({super.key, required this.session});

  final OwnerSession session;

  @override
  State<VoiceTab> createState() => _VoiceTabState();
}

class _VoiceTabState extends State<VoiceTab> {
  VoiceSettings? _settings;
  String? _error;
  String? _notice;
  bool _loading = false;
  bool _saving = false;

  final _speechModel = TextEditingController();
  final _voiceId = TextEditingController();
  final _ocrModel = TextEditingController();
  String _replyMode = 'text';
  bool _ocrEnabled = false;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _speechModel.dispose();
    _voiceId.dispose();
    _ocrModel.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final settings = await widget.session.client.voiceSettings();
      if (!mounted) {
        return;
      }
      setState(() {
        _settings = settings;
        _speechModel.text = settings.tts.model;
        _voiceId.text = settings.tts.voiceId;
        _ocrModel.text = settings.ocr.model;
        _replyMode = settings.replyMode;
        _ocrEnabled = settings.ocr.ready;
      });
    } on ApiException catch (e) {
      if (!mounted) {
        return;
      }
      setState(() => _error = e.messageForUser());
    } finally {
      if (mounted) {
        setState(() => _loading = false);
      }
    }
  }

  Future<void> _save() async {
    setState(() {
      _saving = true;
      _error = null;
      _notice = null;
    });
    try {
      await widget.session.client.saveVoiceSettings(
        speechModel: _speechModel.text.trim(),
        voiceId: _voiceId.text.trim(),
        replyMode: _replyMode,
        ocrEnabled: _ocrEnabled,
        ocrModel: _ocrModel.text.trim(),
      );
      if (!mounted) {
        return;
      }
      setState(() => _notice = 'Saved. Restart the server to apply.');
      await _load();
    } on ApiException catch (e) {
      if (!mounted) {
        return;
      }
      setState(() => _error = e.messageForUser());
    } finally {
      if (mounted) {
        setState(() => _saving = false);
      }
    }
  }

  Widget _chip(String label, bool ready) {
    return Chip(
      avatar: Icon(
        ready ? Icons.check_circle : Icons.error_outline,
        size: 18,
        color: ready ? Colors.green : Theme.of(context).colorScheme.error,
      ),
      label: Text(label),
    );
  }

  @override
  Widget build(BuildContext context) {
    final settings = _settings;
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Text('Voice and OCR', style: Theme.of(context).textTheme.titleLarge),
        const SizedBox(height: 8),
        Text(
          'Scanned PDFs and images fall back to OCR when the built-in '
          'extractor finds no text. Voice replies send a voice note instead '
          'of text.',
          style: Theme.of(context).textTheme.bodySmall,
        ),
        const SizedBox(height: 12),
        if (_loading) const LinearProgressIndicator(),
        if (settings != null) ...[
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              _chip('Speech to text', settings.stt.ready),
              _chip('Voice replies', settings.tts.ready),
              _chip('OCR', settings.ocr.ready),
            ],
          ),
          const SizedBox(height: 12),
          if (settings.stt.detail != null)
            Text('Speech to text: ${settings.stt.detail}',
                style: Theme.of(context).textTheme.bodySmall),
          if (settings.tts.detail != null)
            Text('Voice replies: ${settings.tts.detail}',
                style: Theme.of(context).textTheme.bodySmall),
          if (settings.ocr.detail != null)
            Text('OCR: ${settings.ocr.detail}',
                style: Theme.of(context).textTheme.bodySmall),
          const SizedBox(height: 16),
          TextField(
            controller: _speechModel,
            decoration: const InputDecoration(
              labelText: 'Text-to-speech model',
              border: OutlineInputBorder(),
            ),
            autocorrect: false,
            enableSuggestions: false,
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _voiceId,
            decoration: const InputDecoration(
              labelText: 'Voice id',
              border: OutlineInputBorder(),
            ),
            autocorrect: false,
            enableSuggestions: false,
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _ocrModel,
            decoration: const InputDecoration(
              labelText: 'OCR model',
              border: OutlineInputBorder(),
            ),
            autocorrect: false,
            enableSuggestions: false,
          ),
          const SizedBox(height: 12),
          SwitchListTile(
            contentPadding: EdgeInsets.zero,
            title: const Text('Use OCR when a PDF or image has no text layer'),
            value: _ocrEnabled,
            onChanged: (value) => setState(() => _ocrEnabled = value),
          ),
          const SizedBox(height: 8),
          Text('Default reply mode',
              style: Theme.of(context).textTheme.titleSmall),
          const SizedBox(height: 8),
          SegmentedButton<String>(
            segments: const [
              ButtonSegment(value: 'text', label: Text('Text')),
              ButtonSegment(value: 'voice', label: Text('Voice')),
            ],
            selected: {_replyMode},
            onSelectionChanged: (value) =>
                setState(() => _replyMode = value.first),
          ),
          const SizedBox(height: 8),
          Text(
            'Voice replies are capped at ${settings.maxSpeechChars} '
            'characters. In Telegram, /voice and /text switch the mode for '
            'that chat.',
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ],
        if (_error != null) ...[
          const SizedBox(height: 12),
          Text(_error!,
              style: TextStyle(color: Theme.of(context).colorScheme.error)),
        ],
        if (_notice != null) ...[
          const SizedBox(height: 12),
          Text(_notice!),
        ],
        const SizedBox(height: 16),
        FilledButton(
          onPressed: _saving || settings == null ? null : _save,
          child: _saving
              ? const SizedBox(
                  height: 20,
                  width: 20,
                  child: CircularProgressIndicator(strokeWidth: 2),
                )
              : const Text('Save voice settings'),
        ),
        const SizedBox(height: 8),
        OutlinedButton(
          onPressed: _loading ? null : _load,
          child: const Text('Reload'),
        ),
      ],
    );
  }
}
