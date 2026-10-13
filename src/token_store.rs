//! Tokens on disk. The file is a credential, so it is only ever readable by its owner,
//! replaced atomically, and guarded by a lock file so that several server processes
//! (e.g. one per MCP client) can share it without racing each other.

use std::{
    fmt,
    fs::{self, File, TryLockError},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix timestamp in seconds.
    pub expires_at: i64,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// Held while tokens are read, refreshed and written. Released on drop.
pub struct TokenLock {
    _file: File,
}

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Waits (at most `max_wait`) for exclusive access to the token file. Waiting happens
/// on tokio's blocking pool, so a slow peer process never stalls the async runtime,
/// and a stuck peer (e.g. suspended mid-refresh) cannot hang this process forever.
pub async fn lock_tokens(path: &Path, max_wait: Duration) -> Result<TokenLock> {
    let lock_path = path.with_extension("lock");
    tokio::task::spawn_blocking(move || {
        if let Some(dir) = lock_path.parent().filter(|d| !d.as_os_str().is_empty()) {
            create_private_dir(dir).map_err(|e| storage_error(dir, e))?;
        }
        let file = open_private(&lock_path).map_err(|e| storage_error(&lock_path, e))?;
        let deadline = Instant::now() + max_wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(TokenLock { _file: file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(LOCK_POLL_INTERVAL);
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(Error::Transient(
                        "the token file is busy (another moneylover-mcp process holds it); try again".into(),
                    ));
                }
                Err(TryLockError::Error(e)) => return Err(storage_error(&lock_path, e)),
            }
        }
    })
    .await
    .map_err(|e| Error::Storage(format!("token lock task failed: {e}")))?
}

/// Marks the account as signed out for every server process sharing this token
/// path (after `logout`, or when Money Lover reports the device as removed).
/// Only `login` clears it, so no process silently logs back in.
pub fn mark_revoked(path: &Path) {
    let marker = revocation_marker(path);
    if let Err(e) = open_private(&marker) {
        tracing::warn!("could not write {}: {e}", display_path(&marker));
    }
}

pub fn clear_revoked(path: &Path) {
    let _ = fs::remove_file(revocation_marker(path));
}

pub fn is_revoked(path: &Path) -> bool {
    revocation_marker(path).exists()
}

fn revocation_marker(path: &Path) -> PathBuf {
    path.with_extension("revoked")
}

/// Reads the token file, refusing (or repairing) a file other users could read.
pub fn load_tokens(path: &Path) -> Result<Option<Tokens>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(storage_error(path, e)),
    };
    ensure_private(path)?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| storage_error(path, e))
}

pub fn save_tokens(path: &Path, tokens: &Tokens) -> Result<()> {
    let json = serde_json::to_vec_pretty(tokens).map_err(|e| storage_error(path, e))?;
    let dir = parent_dir(path);
    create_private_dir(&dir).map_err(|e| storage_error(&dir, e))?;
    // A unique temp file (created 0600 by `tempfile`) so concurrent writers never clobber each other.
    let mut tmp = tempfile::NamedTempFile::new_in(&dir).map_err(|e| storage_error(&dir, e))?;
    tmp.write_all(&json).map_err(|e| storage_error(path, e))?;
    tmp.as_file()
        .sync_all()
        .map_err(|e| storage_error(path, e))?;
    tmp.persist(path)
        .map_err(|e| storage_error(path, e.error))?;
    Ok(())
}

pub fn delete_tokens(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Shows `~/...` instead of the absolute path, which would reveal the user name to the LLM provider.
pub fn display_path(path: &Path) -> String {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Only a directory we create ourselves gets its permissions tightened.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn open_private(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// A token file readable by group or others is tightened to 0600. If that fails
/// (typically because another user owns the file) it is not trusted at all.
#[cfg(unix)]
fn ensure_private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)
        .map_err(|e| storage_error(path, e))?
        .permissions()
        .mode();
    if mode & 0o077 == 0 {
        return Ok(());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| {
        Error::Storage(format!(
            "{} is readable by other users and cannot be fixed ({e}); refusing to use it",
            display_path(path)
        ))
    })?;
    tracing::warn!(
        "{} was readable by other users; permissions tightened to 0600",
        display_path(path)
    );
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private(_path: &Path) -> Result<()> {
    Ok(())
}

fn storage_error(path: &Path, e: impl fmt::Display) -> Error {
    Error::Storage(format!("{}: {e}", display_path(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tokens {
        Tokens {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: 42,
        }
    }

    #[test]
    fn round_trip_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("token.json");
        save_tokens(&path, &sample()).unwrap();
        save_tokens(&path, &sample()).unwrap(); // overwriting works too

        let loaded = load_tokens(&path).unwrap().unwrap();
        assert_eq!(loaded.access_token, "a");
        assert_eq!(loaded.refresh_token.as_deref(), Some("r"));
        assert_eq!(loaded.expires_at, 42);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
            let dir_mode = fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
        }
    }

    #[cfg(unix)]
    #[test]
    fn loose_permissions_are_tightened_on_load() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.json");
        save_tokens(&path, &sample()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_tokens(&path).unwrap().is_some());
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn missing_file_is_none_and_garbage_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.json");
        assert!(load_tokens(&path).unwrap().is_none());
        fs::write(&path, "not json").unwrap();
        assert!(load_tokens(&path).is_err());
        delete_tokens(&path);
        assert!(load_tokens(&path).unwrap().is_none());
    }

    #[tokio::test]
    async fn lock_is_exclusive_bounded_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.json");
        let held = lock_tokens(&path, Duration::from_secs(1)).await.unwrap();
        let waited = lock_tokens(&path, Duration::from_millis(250)).await;
        assert!(
            matches!(waited, Err(Error::Transient(_))),
            "a second lock must time out"
        );
        drop(held);
        assert!(lock_tokens(&path, Duration::from_millis(250)).await.is_ok());
    }

    #[test]
    fn revocation_marker_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token.json");
        assert!(!is_revoked(&path));
        mark_revoked(&path);
        assert!(is_revoked(&path));
        clear_revoked(&path);
        assert!(!is_revoked(&path));
    }

    #[test]
    fn debug_hides_secrets() {
        let tokens = Tokens {
            access_token: "secret-access".into(),
            refresh_token: Some("secret-refresh".into()),
            expires_at: 1,
        };
        assert!(!format!("{tokens:?}").contains("secret"));
    }
}
