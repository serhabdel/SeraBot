//! The member's Library (0.74.8): one cross-chat list of every file a bot
//! made in any of the member's conversations plus the member's own uploads
//! (`GET /v1/library`, `POST /v1/library/upload` in `api/library.rs`), with a
//! best-effort mirror of each file to the member's own connected OneDrive
//! (`[library] onedrive_mirror`, default on; folder default
//! `SeraBot Library` under the OneDrive base folder).
//!
//! Uploads are recorded in `conversation_artifacts` under the virtual
//! conversation [`LIBRARY_SCOPE`], so they stay UID-scoped like every other
//! user-owned row and are served by the existing per-user artifact
//! download/preview endpoints. `social_extract` downloads land in the same
//! table (real conversation) and are mirrored through the same
//! [`LibraryMirror`].

use std::sync::Arc;

use crate::config::Config;
use crate::onedrive::OneDriveRegistry;

/// Virtual conversation id under which Library uploads are recorded in
/// `conversation_artifacts` (scoped by user like every row there).
pub const LIBRARY_SCOPE: &str = "library";

/// Tool name recorded for Library uploads.
pub const LIBRARY_UPLOAD_TOOL: &str = "library_upload";

/// Best-effort OneDrive mirror for library files. Never fails the caller:
/// [`LibraryMirror::mirror`] reports a short status string instead.
#[derive(Clone)]
pub struct LibraryMirror {
    config: Config,
    onedrive: Option<Arc<OneDriveRegistry>>,
}

impl LibraryMirror {
    pub fn new(config: Config, onedrive: Option<Arc<OneDriveRegistry>>) -> Self {
        Self { config, onedrive }
    }

    /// Whether mirroring is switched on in `[library]`.
    pub fn enabled(&self) -> bool {
        self.config.library.enabled && self.config.library.onedrive_mirror
    }

    /// The OneDrive folder (under the base folder) the mirror uploads to.
    pub fn folder(&self) -> &str {
        &self.config.library.onedrive_folder
    }

    /// Whether the acting user has connected their own drive. Never falls
    /// back to another member's (or the admin's) credential.
    pub fn connected(&self, user_id: &str) -> bool {
        self.onedrive
            .as_ref()
            .and_then(|registry| registry.connected(user_id).ok())
            .unwrap_or(false)
    }

    /// Mirror one file to the acting user's OneDrive. Best-effort: the return
    /// value is "uploaded", "off", a not-connected/not-set-up note, or
    /// "failed: …". A failure never propagates — the library copy is already
    /// stored.
    pub async fn mirror(&self, user_id: &str, filename: &str, bytes: &[u8], mime: &str) -> String {
        if !self.enabled() {
            return "off".into();
        }
        let Some(registry) = &self.onedrive else {
            return "OneDrive is not set up on this server".into();
        };
        if !self.connected(user_id) {
            return "OneDrive is not connected for this account".into();
        }
        let Ok(client) = registry.client_for(user_id) else {
            return "failed: could not build the OneDrive client".into();
        };
        let folder = self.folder().to_owned();
        match client
            .lock()
            .await
            .upload(filename, bytes.to_vec(), mime, Some(folder.as_str()))
            .await
        {
            Ok(_) => "uploaded".into(),
            Err(error) => format!("failed: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_is_off_without_onedrive_and_reports_it() {
        let mirror = LibraryMirror::new(Config::default(), None);
        assert!(mirror.enabled());
        assert!(!mirror.connected("user-1"));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let status = rt.block_on(mirror.mirror("user-1", "a.png", b"png", "image/png"));
        assert_eq!(status, "OneDrive is not set up on this server");
    }

    #[test]
    fn mirror_can_be_turned_off_in_config() {
        let mut config = Config::default();
        config.library.onedrive_mirror = false;
        let mirror = LibraryMirror::new(config, None);
        assert!(!mirror.enabled());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let status = rt.block_on(mirror.mirror("user-1", "a.png", b"png", "image/png"));
        assert_eq!(status, "off");
        assert_eq!(mirror.folder(), "SeraBot Library");
    }
}
