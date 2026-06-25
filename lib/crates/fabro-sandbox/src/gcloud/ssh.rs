//! SSH transport for the gcloud provider.
//!
//! Built on the `openssh` crate (multiplexed system-ssh sessions). Two
//! security properties are mandatory:
//!
//!   * **Host-key pinning.** The VM's host key is fetched out-of-band from the
//!     Compute `guest-attributes` endpoint *after* the insert op is DONE and
//!     written into a per-session `known_hosts`. We connect with
//!     [`KnownHosts::Add`] against that pre-pinned file — **never**
//!     [`KnownHosts::Accept`], which would trust-on-first-use a key we never
//!     verified.
//!   * **Ephemeral key handling.** The per-run private key is materialized into
//!     a `0600` file under a **tmpfs** dir (`/dev/shm`, or an operator-supplied
//!     `FABRO_GCLOUD_SSH_SECRET_DIR` tmpfs mount) for the lifetime of the
//!     session — system ssh reads identities from a path — and removed on drop.
//!     If no tmpfs is available the session **fails closed** rather than writing
//!     the private key to the OS temp dir, which may be persistent disk: the key
//!     never lands on durable storage.
//!     (`ponytail`: an ssh-agent injection path would keep it purely in-memory;
//!     deferred — the openssh crate has no clean agent-injection seam.)

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use openssh::{KnownHosts, Session, SessionBuilder};
use tokio::time;

use crate::sandbox::shell_quote;

/// Output from a single SSH command.
pub struct SshOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
}

/// SSH operations, abstracted for testability.
#[async_trait]
pub trait SshRunner: Send + Sync {
    async fn run_command(&self, command: &str) -> crate::Result<SshOutput>;
    async fn run_command_with_timeout(
        &self,
        command: &str,
        timeout: Duration,
    ) -> crate::Result<SshOutput>;
    async fn upload_file(&self, path: &str, content: &[u8]) -> crate::Result<()>;
    async fn download_file(&self, path: &str) -> crate::Result<Vec<u8>>;
}

/// Parameters required to open a pinned SSH session to a freshly-provisioned
/// VM.
pub struct SshConnectParams {
    pub host: String,
    pub user: String,
    /// OpenSSH private key PEM (in-memory).
    pub private_key: String,
    /// `known_hosts`-style host key line for the VM (`<algo> <blob>`), pinned
    /// from guest attributes.
    pub host_key_line: String,
    pub connect_timeout: Duration,
}

/// Real SSH runner over a multiplexed `openssh` session.
pub struct OpensshRunner {
    session: Arc<Session>,
    /// tmpfs dir holding the ephemeral key + pinned known_hosts; removed on
    /// drop.
    _secret_dir: SecretDir,
}

impl OpensshRunner {
    /// Connect to the VM with a pinned host key and the per-run identity.
    pub async fn connect(params: &SshConnectParams) -> crate::Result<Self> {
        let secret_dir = SecretDir::create()?;
        let key_path = secret_dir.write_file("id", params.private_key.as_bytes(), 0o600)?;
        let known_hosts = format!("{} {}\n", params.host, params.host_key_line);
        let known_hosts_path =
            secret_dir.write_file("known_hosts", known_hosts.as_bytes(), 0o600)?;

        let mut builder = SessionBuilder::default();
        builder
            .user(params.user.clone())
            .known_hosts_check(KnownHosts::Add)
            .user_known_hosts_file(&known_hosts_path)
            .keyfile(&key_path)
            .connect_timeout(params.connect_timeout);

        let session = builder.connect(&params.host).await.map_err(|err| {
            crate::Error::context(format!("SSH connect to {} failed", params.host), err)
        })?;

        Ok(Self {
            session: Arc::new(session),
            _secret_dir: secret_dir,
        })
    }

    /// A cloned handle to the underlying session, for spawning owned
    /// (`'static`) child processes (ACP bidirectional stdio).
    #[must_use]
    pub fn session(&self) -> Arc<Session> {
        Arc::clone(&self.session)
    }
}

#[async_trait]
impl SshRunner for OpensshRunner {
    async fn run_command(&self, command: &str) -> crate::Result<SshOutput> {
        let output = self
            .session
            .shell(command)
            .output()
            .await
            .map_err(|err| crate::Error::context("SSH command failed", err))?;
        Ok(SshOutput {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    async fn run_command_with_timeout(
        &self,
        command: &str,
        timeout: Duration,
    ) -> crate::Result<SshOutput> {
        match time::timeout(timeout, self.run_command(command)).await {
            Ok(result) => result,
            Err(_) => Err(crate::Error::message("SSH command timed out")),
        }
    }

    async fn upload_file(&self, path: &str, content: &[u8]) -> crate::Result<()> {
        let encoded = STANDARD.encode(content);
        let cmd = format!(
            "echo {} | base64 -d > {}",
            shell_quote(&encoded),
            shell_quote(path)
        );
        let output = self.run_command(&cmd).await?;
        if output.exit_code != 0 {
            return Err(crate::Error::message(format!(
                "SSH upload to {path} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }

    async fn download_file(&self, path: &str) -> crate::Result<Vec<u8>> {
        let output = self
            .run_command(&format!("cat {}", shell_quote(path)))
            .await?;
        if output.exit_code != 0 {
            return Err(crate::Error::message(format!(
                "SSH download of {path} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(output.stdout)
    }
}

/// A short-lived directory on a tmpfs (RAM-backed) mount holding SSH secrets.
/// Creation fails rather than falling back to persistent disk. Best-effort
/// removed on drop.
struct SecretDir {
    path: PathBuf,
}

impl SecretDir {
    /// Create the secret dir on a tmpfs-backed base, failing closed when none is
    /// available (never writes the private key to persistent disk).
    fn create() -> crate::Result<Self> {
        Self::create_in(tmpfs_base()?)
    }

    /// Create the secret dir under an explicit base. The caller is responsible
    /// for the base being RAM-backed; `create` enforces that, this is the
    /// dependency-free seam used by tests.
    fn create_in(base: PathBuf) -> crate::Result<Self> {
        let unique = format!("fabro-gcloud-ssh-{}", uuid::Uuid::new_v4());
        let path = base.join(unique);
        std::fs::create_dir(&path)
            .map_err(|err| crate::Error::context("Failed to create SSH secret dir", err))?;
        set_mode(&path, 0o700)?;
        Ok(Self { path })
    }

    fn write_file(&self, name: &str, content: &[u8], mode: u32) -> crate::Result<PathBuf> {
        let path = self.path.join(name);
        std::fs::write(&path, content)
            .map_err(|err| crate::Error::context("Failed to write SSH secret", err))?;
        set_mode(&path, mode)?;
        Ok(path)
    }
}

impl Drop for SecretDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Resolve a tmpfs-backed base directory for the short-lived SSH private key.
///
/// Order: an operator-supplied `FABRO_GCLOUD_SSH_SECRET_DIR` tmpfs mount, else
/// `/dev/shm` (the canonical Linux tmpfs). We deliberately do **not** fall back
/// to `std::env::temp_dir()`: on most hosts that is persistent disk, and the
/// per-run private key must never touch durable storage. With no tmpfs we fail
/// closed so the run errors instead of silently leaking the key to disk.
#[expect(
    clippy::disallowed_methods,
    reason = "Intentional process-env lookup facade for the operator-supplied tmpfs override; no fixed EnvVars entry exists for this gcloud-only knob."
)]
fn tmpfs_base() -> crate::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("FABRO_GCLOUD_SSH_SECRET_DIR") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return Ok(path);
        }
        return Err(crate::Error::message(
            "FABRO_GCLOUD_SSH_SECRET_DIR is set but is not a directory; point it at a writable tmpfs mount",
        ));
    }
    let shm = PathBuf::from("/dev/shm");
    if shm.is_dir() {
        return Ok(shm);
    }
    Err(crate::Error::message(
        "no tmpfs available for the ephemeral SSH key: /dev/shm is missing and \
         FABRO_GCLOUD_SSH_SECRET_DIR is unset. Refusing to write the per-run private key to \
         persistent disk — mount a tmpfs (e.g. /dev/shm) on the control-plane host.",
    ))
}

#[cfg(unix)]
fn set_mode(path: &std::path::Path, mode: u32) -> crate::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|err| crate::Error::context("Failed to set SSH secret permissions", err))
}

#[cfg(not(unix))]
fn set_mode(_path: &std::path::Path, _mode: u32) -> crate::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_dir_is_removed_on_drop() {
        // Use `create_in` with a cross-platform base so the drop behaviour is
        // testable on dev hosts without `/dev/shm` (e.g. macOS).
        let path = {
            let dir = SecretDir::create_in(std::env::temp_dir()).unwrap();
            dir.write_file("id", b"secret", 0o600).unwrap();
            dir.path.clone()
        };
        assert!(!path.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "Test reads the override env var to skip when an operator override is set; no EnvVars entry exists for this gcloud-only knob."
    )]
    fn tmpfs_base_uses_dev_shm_not_temp_dir() {
        // The production resolver must pick the tmpfs, never the (possibly
        // persistent) OS temp dir. Only asserted when no operator override is in
        // play and the canonical tmpfs exists.
        if std::env::var_os("FABRO_GCLOUD_SSH_SECRET_DIR").is_none()
            && std::path::Path::new("/dev/shm").is_dir()
        {
            assert_eq!(tmpfs_base().unwrap(), PathBuf::from("/dev/shm"));
        }
    }
}
