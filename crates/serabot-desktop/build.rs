fn main() {
    // Build-time inputs read with option_env! in src/updater.rs.
    // Absent values disable updates (developer build) without any network call.
    println!("cargo:rerun-if-env-changed=SERABOT_RELEASES_REPO");
    println!("cargo:rerun-if-env-changed=SERABOT_BUILD_SHA");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    embed_windows_metadata();
}

/// Version metadata and a per-monitor-v2 DPI manifest for the Windows exe.
/// The window icon itself is generated at runtime in `main.rs`.
#[cfg(windows)]
fn embed_windows_metadata() {
    let mut resource = winresource::WindowsResource::new();
    resource
        .set("ProductName", "SeraBot Owner")
        .set("FileDescription", "SeraBot owner console")
        .set("ProductVersion", env!("CARGO_PKG_VERSION"))
        .set("FileVersion", env!("CARGO_PKG_VERSION"))
        .set("OriginalFilename", "serabot-desktop.exe")
        .set("LegalCopyright", "SeraBot")
        .set_manifest(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity version="1.0.0.0" processorArchitecture="*" name="SeraBot.Owner" type="win32"/>
  <description>SeraBot Owner</description>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">permonitorv2,permonitor</dpiAwareness>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>"#,
        );
    if let Err(error) = resource.compile() {
        // Keep the exe buildable on hosts without the Windows resource tools.
        println!("cargo:warning=Windows resource metadata was not embedded: {error}");
    }
}
