use async_trait::async_trait;
use openssh::{KnownHosts, Session, SessionBuilder};

use super::{SshOutput, SshRunner};
use crate::shell_quote;

use std::sync::Arc;

/// Real SSH implementation using the `openssh` crate (multiplexed connections).
pub struct OpensshRunner {
    session: Arc<Session>,
}

impl OpensshRunner {
    /// Connect to a host via SSH, using the user's SSH agent for authentication.
    /// Commands are executed through a shell (`sh -c`).
    pub async fn connect(destination: &str, config_file: Option<&str>) -> Result<Self, String> {
        let mut builder = SessionBuilder::default();
        builder.known_hosts_check(KnownHosts::Accept);
        if let Some(cfg) = config_file {
            builder.config_file(cfg);
        }
        let session = builder
            .connect(destination)
            .await
            .map_err(|e| format!("SSH connection to {destination} failed: {e}"))?;
        Ok(Self {
            session: Arc::new(session),
        })
    }
}

#[async_trait]
impl SshRunner for OpensshRunner {
    async fn run_command(&self, command: &str) -> Result<SshOutput, String> {
        let output = self
            .session
            .shell(command)
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
        let mut child = self.session.shell(command);
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
            .session
            .shell(&cmd)
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
            .session
            .shell(&cmd)
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
        let mut child = openssh::Session::to_command(session_clone, "sh");
        child.arg("-c").arg(command);

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
        // openssh::Child doesn't have a kill method, and dropping it only kills the local ssh process,
        // not the remote process (it leaves it orphaned).
        // Since we don't know the remote PID here, we just drop the connection, which is known to be incomplete.
        // A full fix requires wrapping the remote execution to track PID or capture signals.
        self.child.take();
        Ok(())
    }
}
