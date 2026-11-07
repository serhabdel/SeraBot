import 'dart:io';

import 'package:flutter/services.dart';

/// Result of asking Android to install a verified APK.
enum InstallOutcome { launched, needsPermission, unsupported, failed }

abstract class ApkInstaller {
  /// Open the system package installer for [apk]. When the user has not yet
  /// allowed installs from SeraBot, opens that settings page and returns
  /// [InstallOutcome.needsPermission]; calling again after they allow it
  /// launches the installer.
  Future<InstallOutcome> install(File apk);
}

class AndroidApkInstaller implements ApkInstaller {
  static const _channel = MethodChannel('tech.seradyx.serabot/installer');

  @override
  Future<InstallOutcome> install(File apk) async {
    if (!Platform.isAndroid) {
      return InstallOutcome.unsupported;
    }
    try {
      final result = await _channel.invokeMethod<String>('install', {'path': apk.path});
      return switch (result) {
        'launched' => InstallOutcome.launched,
        'needs_permission' => InstallOutcome.needsPermission,
        _ => InstallOutcome.failed,
      };
    } on PlatformException {
      return InstallOutcome.failed;
    } on MissingPluginException {
      return InstallOutcome.unsupported;
    }
  }
}
