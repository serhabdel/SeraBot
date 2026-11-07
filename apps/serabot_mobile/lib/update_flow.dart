import 'dart:io';

import 'package:flutter/material.dart';
import 'package:package_info_plus/package_info_plus.dart';
import 'package:path_provider/path_provider.dart';

import 'installer.dart';
import 'updater.dart';

/// Checks GitHub Releases and, when a newer verified-capable release exists,
/// shows "Update available" with the release notes. Nothing is downloaded or
/// installed without a tap on "Download and install".
///
/// [silent] suppresses "up to date" and error messages (used on app start).
Future<void> checkForUpdates(
  BuildContext context, {
  bool silent = false,
  ApkInstaller? installer,
}) async {
  final messenger = ScaffoldMessenger.maybeOf(context);
  void say(String text) {
    if (!silent) {
      messenger?.showSnackBar(SnackBar(content: Text(text)));
    }
  }

  UpdateChecker? checker;
  try {
    final info = await PackageInfo.fromPlatform();
    final current = AppVersion.parse(info.version);
    if (current == null) {
      return;
    }
    final cache = await getTemporaryDirectory();
    checker = UpdateChecker(current: current, cacheDir: Directory('${cache.path}/updates'));
    final update = await checker.check();
    if (update == null) {
      say('SeraBot ${current.toString()} is up to date.');
      checker.close();
      return;
    }
    if (!context.mounted) {
      checker.close();
      return;
    }
    await showDialog<void>(
      context: context,
      builder: (_) => _UpdateDialog(
        checker: checker!,
        update: update,
        installer: installer ?? AndroidApkInstaller(),
      ),
    );
  } on UpdateException catch (e) {
    say(e.toString());
  } on Object {
    say('Update check failed.');
  } finally {
    checker?.close();
  }
}

class _UpdateDialog extends StatefulWidget {
  const _UpdateDialog({
    required this.checker,
    required this.update,
    required this.installer,
  });

  final UpdateChecker checker;
  final AvailableUpdate update;
  final ApkInstaller installer;

  @override
  State<_UpdateDialog> createState() => _UpdateDialogState();
}

class _UpdateDialogState extends State<_UpdateDialog> {
  bool _busy = false;
  double? _progress;
  String? _message;
  File? _ready;

  Future<void> _start() async {
    setState(() {
      _busy = true;
      _message = null;
    });
    try {
      _ready ??= await widget.checker.download(
        widget.update,
        onProgress: (received, total) {
          if (mounted && total > 0) {
            setState(() => _progress = received / total);
          }
        },
      );
      final outcome = await widget.installer.install(_ready!);
      if (!mounted) {
        return;
      }
      setState(() {
        _busy = false;
        _message = switch (outcome) {
          InstallOutcome.launched => null,
          InstallOutcome.needsPermission =>
            'Allow installs from SeraBot in the settings page that opened, '
                'then tap "Download and install" again.',
          InstallOutcome.unsupported => 'Installing updates is only supported on Android.',
          InstallOutcome.failed => 'The installer could not be opened.',
        };
      });
      if (outcome == InstallOutcome.launched && mounted) {
        Navigator.of(context).pop();
      }
    } on UpdateException catch (e) {
      if (mounted) {
        setState(() {
          _busy = false;
          _message = e.toString();
        });
      }
    } on Object {
      if (mounted) {
        setState(() {
          _busy = false;
          _message = 'The update failed.';
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final update = widget.update;
    return AlertDialog(
      title: const Text('Update available'),
      content: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('SeraBot ${update.version}'),
            const SizedBox(height: 8),
            if (update.notes.isNotEmpty) Text(update.notes),
            if (_busy) ...[
              const SizedBox(height: 12),
              LinearProgressIndicator(value: _progress),
            ],
            if (_message != null) ...[
              const SizedBox(height: 12),
              Text(_message!),
            ],
          ],
        ),
      ),
      actions: [
        TextButton(
          onPressed: _busy ? null : () => Navigator.of(context).pop(),
          child: const Text('Later'),
        ),
        FilledButton(
          onPressed: _busy ? null : _start,
          child: const Text('Download and install'),
        ),
      ],
    );
  }
}
