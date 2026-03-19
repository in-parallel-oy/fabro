use async_trait::async_trait;
use openssh::{KnownHosts, Session};

use super::{SshOutput, SshRunner};
use crate::shell_quote;

use std::sync::Arc;

/// Real SSH implementation using the `openssh` crate (multiplexed connections).
pub struct OpensshRunner {
    session: Arc<Session>,
    /// When true, commands are sent via `raw_command` (no shell wrapping).
    /// Used for the exe.dev management plane which has a custom SSH command handler.
    raw_mode: bool,
}

impl OpensshRunner {
    /// Connect to a host via SSH, using the user's SSH agent for authentication.
    /// Commands are executed through a shell (`sh -c`).
    pub async fn connect(host: &str) -> Result<Self, String> {
        let session = Session::connect(host, KnownHosts::Accept)
            .await
            .map_err(|e| format!("SSH connection to {host} failed: {e}"))?;
        Ok(Self {
            session: Arc::new(session),
            raw_mode: false,
        })
    }

    /// Connect to a host via SSH in raw mode (no shell wrapping).
    /// Commands are sent directly as the SSH command string.
    /// Used for the exe.dev management plane which has a custom command handler.
    pub async fn connect_raw(host: &str) -> Result<Self, String> {
        let session = Session::connect(host, KnownHosts::Accept)
            .await
            .map_err(|e| format!("SSH connection to {host} failed: {e}"))?;
        Ok(Self {
            session: Arc::new(session),
            raw_mode: true,
        })
    }

    fn build_command(&self, command: &str) -> openssh::OwningCommand<&Session> {
        if self.raw_mode {
            self.session.raw_command(command)
        } else {
            self.session.shell(command)
        }
    }
}

#[async_trait]
impl SshRunner for OpensshRunner {
    async fn run_command(&self, command: &str) -> Result<SshOutput, String> {
        let output = self
            .build_command(command)
            .output()
            .await
            .map_err(|e| format!("SSH command failed: {e}"))?;

        let exit_code = output.status.code().unwrap_or(-1);
        Ok(SshOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code,
        })
    }

    async fn run_command_with_timeout(
        &self,
        command: &str,
        timeout: std::time::Duration,
    ) -> Result<SshOutput, String> {
        let mut child = self.build_command(command);
        let fut = child.output();

        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                Ok(SshOutput {
                    stdout: output.stdout,
                    stderr: output.stderr,
                    exit_code,
                })
            }
            Ok(Err(e)) => Err(format!("SSH command failed: {e}")),
            Err(_) => Err("Command timed out".to_string()),
        }
    }

    async fn upload_file(&self, path: &str, content: &[u8]) -> Result<(), String> {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(content);
        let cmd = format!("echo '{}' | base64 -d > {}", encoded, shell_quote(path),);
        let output = self
            .build_command(&cmd)
            .output()
            .await
            .map_err(|e| format!("SSH upload failed: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Upload to {path} failed: {stderr}"));
        }
        Ok(())
    }

    async fn download_file(&self, path: &str) -> Result<Vec<u8>, String> {
        let cmd = format!("cat {}", shell_quote(path));
        let output = self
            .build_command(&cmd)
            .output()
            .await
            .map_err(|e| format!("SSH download failed: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Download of {path} failed: {stderr}"));
        }
        Ok(output.stdout)
    }

    async fn spawn_command(&self, command: &str) -> Result<Box<dyn crate::ChildProcess>, String> {
        let session_clone = Arc::clone(&self.session);
        let mut child = if self.raw_mode {
            openssh::Session::to_raw_command(session_clone, command)
        } else {
            let mut cmd = openssh::Session::to_command(session_clone, "sh");
            cmd.arg("-c").arg(command);
            cmd
        };
        let spawned = child
            .spawn()
            .await
            .map_err(|e| format!("SSH spawn failed: {e}"))?;
        Ok(Box::new(OpensshChildProcess {
            child: Some(spawned),
        }))
    }
}

pub struct OpensshChildProcess {
    child: Option<openssh::Child<Arc<Session>>>,
}

#[async_trait]
impl crate::ChildProcess for OpensshChildProcess {
    fn take_stdin(&mut self) -> Option<Box<dyn tokio::io::AsyncWrite + Send + Unpin>> {
        self.child.as_mut().and_then(|c| {
            c.stdin()
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncWrite + Send + Unpin>)
        })
    }

    fn take_stdout(&mut self) -> Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>> {
        self.child.as_mut().and_then(|c| {
            c.stdout()
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>)
        })
    }

    fn take_stderr(&mut self) -> Option<Box<dyn tokio::io::AsyncRead + Send + Unpin>> {
        self.child.as_mut().and_then(|c| {
            c.stderr()
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>)
        })
    }

    async fn wait(&mut self) -> Result<i32, String> {
        if let Some(child) = self.child.take() {
            child
                .wait()
                .await
                .map(|s| s.code().unwrap_or(-1))
                .map_err(|e| e.to_string())
        } else {
            Err("Child process already waited on".to_string())
        }
    }

    async fn kill(&mut self) -> Result<(), String> {
        // openssh::Child doesn't have a kill method, but dropping it kills the process
        // if we disconnect the session or just drop the child.
        self.child.take();
        Ok(())
    }
}
