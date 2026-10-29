use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use age::secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Result, SeraError};

#[derive(Clone, Default)]
pub struct SecretStore {
    path: PathBuf,
    identity_path: PathBuf,
    values: BTreeMap<String, String>,
}

impl std::fmt::Debug for SecretStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretStore")
            .field("path", &self.path)
            .field("identity_path", &self.identity_path)
            .field("secret_names", &self.values.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SecretFile {
    #[serde(default)]
    values: BTreeMap<String, String>,
}

impl SecretStore {
    pub fn open(config: &Config) -> Result<Self> {
        Self::open_paths(config.secrets_path(), config.identity_path())
    }

    pub fn open_paths(path: impl Into<PathBuf>, identity_path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let identity_path = identity_path.into();
        let values = if path.exists() {
            decrypt_file(&path, &identity_path)?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            path,
            identity_path,
            values,
        })
    }

    pub fn from_map(values: BTreeMap<String, String>) -> Self {
        Self {
            path: PathBuf::new(),
            identity_path: PathBuf::new(),
            values,
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn require(&self, name: &str) -> Result<&str> {
        self.get(name)
            .ok_or_else(|| SeraError::Secrets(format!("missing secret: {name}")))
    }

    pub fn names(&self) -> Vec<&str> {
        self.values.keys().map(String::as_str).collect()
    }

    pub fn set(&mut self, name: &str, value: &str) -> Result<()> {
        if name.is_empty() || name.len() > 128 {
            return Err(SeraError::Secrets("invalid secret name".into()));
        }
        if value.is_empty() || value.len() > 16 * 1024 {
            return Err(SeraError::Secrets("invalid secret value".into()));
        }
        self.values.insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.values.remove(name).is_some()
    }

    pub fn save(&self) -> Result<()> {
        if self.path.as_os_str().is_empty() || self.identity_path.as_os_str().is_empty() {
            return Err(SeraError::Secrets(
                "cannot persist an in-memory secret store".into(),
            ));
        }
        ensure_identity(&self.identity_path)?;
        write_encrypted(&self.path, &self.identity_path, &self.values)
    }
}

/// Process-wide handle for the age store. Refresh paths share one store so
/// rotated credentials are written back without dropping sibling secrets.
#[derive(Clone)]
pub struct SharedSecrets {
    inner: Arc<Mutex<SecretStore>>,
}

impl std::fmt::Debug for SharedSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let store = self.lock();
        let names = store
            .names()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        formatter
            .debug_struct("SharedSecrets")
            .field("path", &store.path)
            .field("identity_path", &store.identity_path)
            .field("secret_names", &names)
            .finish()
    }
}

impl SharedSecrets {
    pub fn new(store: SecretStore) -> Self {
        Self {
            inner: Arc::new(Mutex::new(store)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SecretStore> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn get(&self, name: &str) -> Option<String> {
        self.lock().get(name).map(str::to_owned)
    }

    /// Presence check for status surfaces. Values never leave this module.
    pub fn has(&self, name: &str) -> bool {
        self.lock().get(name).is_some()
    }

    pub fn require(&self, name: &str) -> Result<String> {
        self.lock().require(name).map(str::to_owned)
    }

    /// Reload the ciphertext, apply `updates`, and write it back.
    /// A failed save leaves the previous ciphertext and this handle unchanged.
    pub fn persist(&self, updates: &[(&str, &str)]) -> Result<()> {
        if updates.is_empty() {
            return Err(SeraError::Secrets(
                "rotated credential was not persisted".into(),
            ));
        }
        let mut guard = self.lock();
        if guard.path.as_os_str().is_empty() || guard.identity_path.as_os_str().is_empty() {
            return Err(SeraError::Secrets(
                "cannot persist an in-memory secret store".into(),
            ));
        }
        let mut updated = SecretStore::open_paths(&guard.path, &guard.identity_path)?;
        for (name, value) in updates {
            updated.set(name, value)?;
        }
        updated.save()?;
        *guard = updated;
        Ok(())
    }
}

pub fn ensure_identity(path: &Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let identity = age::x25519::Identity::generate();
    let secret = identity.to_string();
    std::fs::write(path, secret.expose_secret().as_bytes())?;
    restrict_permissions(path)?;
    Ok(())
}

fn load_identity(path: &Path) -> Result<age::x25519::Identity> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| SeraError::Secrets(format!("{}: {error}", path.display())))?;
    age::x25519::Identity::from_str(raw.trim())
        .map_err(|error| SeraError::Secrets(format!("invalid age identity: {error}")))
}

fn decrypt_file(path: &Path, identity_path: &Path) -> Result<BTreeMap<String, String>> {
    let identity = load_identity(identity_path)?;
    let input = std::fs::File::open(path)?;
    let reader = age::armor::ArmoredReader::new(input);
    let decryptor = age::Decryptor::new(reader)
        .map_err(|error| SeraError::Secrets(format!("cannot open secret store: {error}")))?;
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|error| SeraError::Secrets(format!("cannot decrypt secret store: {error}")))?;
    let mut plaintext = String::new();
    reader.read_to_string(&mut plaintext)?;
    let file: SecretFile = serde_json::from_str(&plaintext)
        .map_err(|error| SeraError::Secrets(format!("invalid secret store content: {error}")))?;
    Ok(file.values)
}

fn write_encrypted(
    path: &Path,
    identity_path: &Path,
    values: &BTreeMap<String, String>,
) -> Result<()> {
    let identity = load_identity(identity_path)?;
    let recipient = identity.to_public();
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&recipient as &dyn age::Recipient))
            .map_err(|error| SeraError::Secrets(error.to_string()))?;
    let plaintext = serde_json::to_vec(&SecretFile {
        values: values.clone(),
    })?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("age.tmp");
    {
        let file = std::fs::File::create(&temporary)?;
        let armored = age::armor::ArmoredWriter::wrap_output(file, age::armor::Format::AsciiArmor)
            .map_err(|error| SeraError::Secrets(error.to_string()))?;
        let mut writer = encryptor
            .wrap_output(armored)
            .map_err(|error| SeraError::Secrets(error.to_string()))?;
        writer.write_all(&plaintext)?;
        writer
            .finish()
            .and_then(|armored| armored.finish())
            .map_err(|error| SeraError::Secrets(error.to_string()))?;
    }
    std::fs::rename(&temporary, path)?;
    restrict_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypts_and_round_trips_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let identity = dir.path().join("identity.txt");
        let store_path = dir.path().join("secrets.age");
        ensure_identity(&identity).unwrap();

        let mut store = SecretStore::open_paths(&store_path, &identity).unwrap();
        store.set("llm_api_key", "test-value").unwrap();
        store.save().unwrap();

        let reopened = SecretStore::open_paths(&store_path, &identity).unwrap();
        assert_eq!(reopened.get("llm_api_key"), Some("test-value"));
        let raw = std::fs::read(&store_path).unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("test-value"));
    }

    #[test]
    fn rejects_persisting_memory_store() {
        let store = SecretStore::from_map(BTreeMap::new());
        assert!(store.save().is_err());
        let shared = SharedSecrets::new(store);
        let error = shared.persist(&[("refresh", "refresh-new")]).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("in-memory"));
        assert!(!message.contains("refresh-new"));
    }

    #[test]
    fn failed_persist_keeps_the_previous_ciphertext() {
        let dir = tempfile::tempdir().unwrap();
        let identity = dir.path().join("identity.txt");
        let store_path = dir.path().join("secrets.age");
        ensure_identity(&identity).unwrap();
        let mut store = SecretStore::open_paths(&store_path, &identity).unwrap();
        store.set("refresh", "refresh-old").unwrap();
        store.set("other", "other-value").unwrap();
        store.save().unwrap();
        let backup = std::fs::read(&identity).unwrap();

        let shared = SharedSecrets::new(SecretStore::open_paths(&store_path, &identity).unwrap());
        std::fs::write(&identity, "not-an-identity").unwrap();
        let error = shared.persist(&[("refresh", "refresh-new")]).unwrap_err();
        let message = error.to_string();
        assert!(!message.contains("refresh-new"));
        assert!(!message.contains("refresh-old"));
        assert!(!message.contains("other-value"));
        assert_eq!(shared.get("refresh").as_deref(), Some("refresh-old"));

        std::fs::write(&identity, backup).unwrap();
        let reopened = SecretStore::open_paths(&store_path, &identity).unwrap();
        assert_eq!(reopened.get("refresh"), Some("refresh-old"));
        assert_eq!(reopened.get("other"), Some("other-value"));
        let raw = std::fs::read(&store_path).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("refresh-old"));
        assert!(!text.contains("refresh-new"));
    }
}
