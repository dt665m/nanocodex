//! Encrypted, locked host-owned subscription credentials.
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
#[error("private credential store is unavailable")]
pub(super) struct StoreError;
type HostResult<T> = std::result::Result<T, StoreError>;
const MAX_STORE_BYTES: u64 = 128 * 1024;

#[derive(Clone)]
pub(super) struct NativeStore {
    path: PathBuf,
    magic: &'static [u8],
}
impl NativeStore {
    pub(super) fn new(path: PathBuf, magic: &'static [u8]) -> Self {
        Self { path, magic }
    }
    pub(super) fn with_store<T>(
        &self,
        action: impl FnOnce(&mut PrivateStore) -> HostResult<T>,
    ) -> HostResult<T> {
        let directory = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        private_directory(directory)?;
        let key_path = self.path.with_added_extension("key");
        let lock_path = self.path.with_added_extension("lock");
        let lock = private_open(&lock_path, true)?;
        fs2::FileExt::lock_exclusive(&lock).map_err(|_| StoreError)?;
        let _lock = StoreLock(lock);
        // RAII releases this lock on every return; the OS releases it on process
        // exit. First key publication uses the same lock as all loads and commits.
        let key = match private_read(&key_path, 32)? {
            Some(bytes) if bytes.len() == 32 => bytes,
            Some(_) => return Err(StoreError),
            None => {
                if fs::symlink_metadata(&self.path).is_ok() {
                    return Err(StoreError);
                }
                let key: [u8; 32] = rand::random();
                atomic_private_write(&key_path, &key)?;
                key.to_vec()
            }
        };
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| StoreError)?;
        let stored = private_read(&self.path, MAX_STORE_BYTES)?;
        let is_new = stored.is_none();
        let mut store = match stored {
            Some(bytes) => {
                if !bytes.starts_with(self.magic) || bytes.len() < self.magic.len() + 12 + 16 {
                    return Err(StoreError);
                }
                let plaintext = cipher
                    .decrypt(
                        Nonce::from_slice(&bytes[self.magic.len()..self.magic.len() + 12]),
                        Payload {
                            msg: &bytes[self.magic.len() + 12..],
                            aad: self.magic,
                        },
                    )
                    .map_err(|_| StoreError)?;
                let store: PrivateStore =
                    serde_json::from_slice(&plaintext).map_err(|_| StoreError)?;
                if store.install_id.is_empty() || store.install_id.len() > 128 {
                    return Err(StoreError);
                }
                store
            }
            None => PrivateStore {
                revision: 0,
                install_id: uuid::Uuid::new_v4().to_string(),
                payload: None,
            },
        };
        let prior_revision = store.revision;
        let result = action(&mut store)?;
        if is_new || store.revision != prior_revision {
            let plaintext = serde_json::to_vec(&store).map_err(|_| StoreError)?;
            let nonce: [u8; 12] = rand::random();
            let encrypted = cipher
                .encrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: &plaintext,
                        aad: self.magic,
                    },
                )
                .map_err(|_| StoreError)?;
            let mut bytes = self.magic.to_vec();
            bytes.extend_from_slice(&nonce);
            bytes.extend_from_slice(&encrypted);
            if bytes.len() as u64 > MAX_STORE_BYTES {
                return Err(StoreError);
            }
            atomic_private_write(&self.path, &bytes)?;
        }
        Ok(result)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PrivateStore {
    pub(super) revision: u64,
    pub(super) install_id: String,
    pub(super) payload: Option<String>,
}

struct StoreLock(File);

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

fn private_directory(path: &Path) -> HostResult<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|_| StoreError)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| StoreError)?;
    if !metadata.is_dir() {
        return Err(StoreError);
    }
    check_private(&metadata)
}

fn check_private(_metadata: &fs::Metadata) -> HostResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if _metadata.permissions().mode() & 0o077 != 0
            || _metadata.uid() != nix::unistd::geteuid().as_raw()
        {
            return Err(StoreError);
        }
    }
    Ok(())
}

fn private_open(path: &Path, create: bool) -> HostResult<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(StoreError);
            }
            check_private(&metadata)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {}
        Err(_) => return Err(StoreError),
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| StoreError)?;
    let metadata = file.metadata().map_err(|_| StoreError)?;
    if !metadata.is_file() {
        return Err(StoreError);
    }
    check_private(&metadata)?;
    Ok(file)
}

fn private_read(path: &Path, max_bytes: u64) -> HostResult<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(StoreError),
        Ok(metadata) if !metadata.is_file() => return Err(StoreError),
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    private_open(path, false)?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| StoreError)?;
    if bytes.len() as u64 > max_bytes {
        return Err(StoreError);
    }
    Ok(Some(bytes))
}

fn atomic_private_write(path: &Path, bytes: &[u8]) -> HostResult<()> {
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Refuse existing symlinks/public files, including when replacing a store.
    if fs::symlink_metadata(path).is_ok() {
        let _ = private_open(path, false)?;
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(|_| StoreError)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| StoreError)?;
    }
    temporary.write_all(bytes).map_err(|_| StoreError)?;
    temporary.as_file().sync_all().map_err(|_| StoreError)?;
    temporary.persist(path).map_err(|_| StoreError)?;
    #[cfg(unix)]
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| StoreError)?;
    Ok(())
}
