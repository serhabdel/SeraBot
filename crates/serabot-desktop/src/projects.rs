//! Blocking API helpers behind the desktop Projects workspace (0.59).
//! Each call opens a short-lived `ApiClient` from the session credentials and
//! runs off the UI thread.

use serde_json::Value;

use crate::api::ApiClient;
use crate::worker::Credentials;

fn client(credentials: &Credentials) -> Result<ApiClient, String> {
    ApiClient::new(credentials.endpoint.clone(), credentials.token.clone())
        .map_err(|error| error.to_string())
}

pub fn list(credentials: &Credentials) -> Result<Value, String> {
    client(credentials)?
        .projects_list()
        .map_err(|error| error.to_string())
}

pub fn create(credentials: &Credentials, name: &str) -> Result<Value, String> {
    client(credentials)?
        .projects_create(name)
        .map_err(|error| error.to_string())
}

pub fn list_files(credentials: &Credentials, slug: &str) -> Result<Value, String> {
    client(credentials)?
        .projects_files(slug)
        .map_err(|error| error.to_string())
}

pub fn upload(
    credentials: &Credentials,
    slug: &str,
    filename: &str,
    subfolder: Option<&str>,
    bytes: &[u8],
) -> Result<Value, String> {
    client(credentials)?
        .projects_upload(slug, filename, subfolder, bytes)
        .map_err(|error| error.to_string())
}

pub fn download(credentials: &Credentials, slug: &str, rel_path: &str) -> Result<Vec<u8>, String> {
    client(credentials)?
        .projects_download(slug, rel_path)
        .map_err(|error| error.to_string())
}

pub fn delete_file(credentials: &Credentials, slug: &str, rel_path: &str) -> Result<Value, String> {
    client(credentials)?
        .projects_delete_file(slug, rel_path)
        .map_err(|error| error.to_string())
}
