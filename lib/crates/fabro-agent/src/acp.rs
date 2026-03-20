use crate::config::{ToolHookCallback, ToolHookDecision};
use crate::types::AgentEvent;
use agent_client_protocol::{Agent, Client, ClientSideConnection};
use agent_client_protocol::{
    ContentBlock, CreateTerminalRequest, CreateTerminalResponse, InitializeRequest,
    InitializeResponse, KillTerminalRequest, KillTerminalResponse, NewSessionRequest,
    NewSessionResponse, PermissionOptionKind, PromptRequest, PromptResponse, ReadTextFileRequest,
    ReadTextFileResponse, ReleaseTerminalRequest, ReleaseTerminalResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    Result as AcpResult, SelectedPermissionOutcome, SessionNotification, SessionUpdate,
    TerminalExitStatus, TerminalOutputRequest, TerminalOutputResponse, ToolCallContent,
    WaitForTerminalExitRequest, WaitForTerminalExitResponse, WriteTextFileRequest,
    WriteTextFileResponse,
};
use fabro_sandbox::{ChildProcess, Sandbox};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub struct TerminalState {
    pub child: Arc<TokioMutex<Box<dyn ChildProcess>>>,
    pub output_buffer: Arc<Mutex<String>>,
    pub exit_status: Arc<Mutex<Option<i32>>>,
}

pub struct FabroAcpClient {
    pub sandbox: Arc<dyn Sandbox>,
    pub tool_hooks: Option<Arc<dyn ToolHookCallback>>,
    pub on_event: Arc<dyn Fn(AgentEvent) + Send + Sync>,
    pub files_touched: Arc<Mutex<HashSet<String>>>,
    pub terminals: Arc<Mutex<HashMap<String, TerminalState>>>,
}

impl Client for FabroAcpClient {
    fn request_permission<'life0, 'async_trait>(
        &'life0 self,
        args: RequestPermissionRequest,
    ) -> Pin<Box<dyn Future<Output = AcpResult<RequestPermissionResponse>> + 'async_trait>>
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        Box::pin(async move {
            let tool_name = args
                .tool_call
                .fields
                .title
                .clone()
                .unwrap_or_else(|| "unknown_tool".to_string());
            let tool_input = args
                .tool_call
                .fields
                .raw_input
                .clone()
                .unwrap_or_else(|| serde_json::json!({}));

            let decision = if let Some(hooks) = &self.tool_hooks {
                hooks.pre_tool_use(&tool_name, &tool_input).await
            } else {
                ToolHookDecision::Proceed
            };

            let option_id = match decision {
                ToolHookDecision::Proceed => args
                    .options
                    .iter()
                    .find(|o| {
                        o.kind == PermissionOptionKind::AllowOnce
                            || o.kind == PermissionOptionKind::AllowAlways
                    })
                    .map(|o| o.option_id.clone())
                    .unwrap_or_else(|| agent_client_protocol::PermissionOptionId::new("allow")),
                ToolHookDecision::Block { .. } => args
                    .options
                    .iter()
                    .find(|o| {
                        o.kind == PermissionOptionKind::RejectOnce
                            || o.kind == PermissionOptionKind::RejectAlways
                    })
                    .map(|o| o.option_id.clone())
                    .unwrap_or_else(|| agent_client_protocol::PermissionOptionId::new("reject")),
            };

            Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
            ))
        })
    }

    fn write_text_file<'life0, 'async_trait>(
        &'life0 self,
        args: WriteTextFileRequest,
    ) -> Pin<Box<dyn Future<Output = AcpResult<WriteTextFileResponse>> + 'async_trait>>
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        Box::pin(async move {
            let path = args.path.to_string_lossy();
            self.sandbox
                .write_file(&path, &args.content)
                .await
                .map_err(|e| agent_client_protocol::Error::internal_error())?;
            Ok(WriteTextFileResponse::new())
        })
    }

    fn read_text_file<'life0, 'async_trait>(
        &'life0 self,
        args: ReadTextFileRequest,
    ) -> Pin<Box<dyn Future<Output = AcpResult<ReadTextFileResponse>> + 'async_trait>>
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        Box::pin(async move {
            let path = args.path.to_string_lossy();
            let content = self
                .sandbox
                .read_file(&path, None, None)
                .await
                .map_err(|e| agent_client_protocol::Error::internal_error())?;
            Ok(ReadTextFileResponse::new(content))
        })
    }

    fn session_notification<'life0, 'async_trait>(
        &'life0 self,
        args: SessionNotification,
    ) -> Pin<Box<dyn Future<Output = AcpResult<()>> + 'async_trait>>
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        Box::pin(async move {
            match args.update {
                SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(text_content) = chunk.content {
                        (self.on_event)(AgentEvent::TextDelta {
                            delta: text_content.text.clone(),
                        });
                    }
                }
                SessionUpdate::AgentThoughtChunk(chunk) => {
                    if let ContentBlock::Text(text_content) = chunk.content {
                        (self.on_event)(AgentEvent::ReasoningDelta {
                            delta: text_content.text,
                        });
                    }
                }
                SessionUpdate::ToolCall(tool_call) => {
                    if let Ok(mut files) = self.files_touched.lock() {
                        for loc in &tool_call.locations {
                            files.insert(loc.path.to_string_lossy().to_string());
                        }
                        for content in &tool_call.content {
                            if let ToolCallContent::Diff(diff) = content {
                                files.insert(diff.path.to_string_lossy().to_string());
                            }
                        }
                    }
                    (self.on_event)(AgentEvent::ToolCallStarted {
                        tool_name: serde_json::to_string(&tool_call.kind)
                            .unwrap_or_else(|_| "\"unknown\"".to_string())
                            .trim_matches('"')
                            .to_string(),
                        tool_call_id: tool_call.tool_call_id.to_string(),
                        arguments: tool_call.raw_input.unwrap_or_else(|| serde_json::json!({})),
                    });
                }
                SessionUpdate::ToolCallUpdate(update) => {
                    if let Ok(mut files) = self.files_touched.lock() {
                        if let Some(locations) = &update.fields.locations {
                            for loc in locations {
                                files.insert(loc.path.to_string_lossy().to_string());
                            }
                        }
                        if let Some(content) = &update.fields.content {
                            for c in content {
                                if let ToolCallContent::Diff(diff) = c {
                                    files.insert(diff.path.to_string_lossy().to_string());
                                }
                            }
                        }
                    }
                    if let Some(status) = update.fields.status {
                        if status == agent_client_protocol::ToolCallStatus::Completed {
                            (self.on_event)(AgentEvent::ToolCallCompleted {
                                tool_name: update
                                    .fields
                                    .kind
                                    .map(|k| {
                                        serde_json::to_string(&k)
                                            .unwrap_or_else(|_| "\"unknown\"".to_string())
                                            .trim_matches('"')
                                            .to_string()
                                    })
                                    .unwrap_or_else(|| "unknown".to_string()),
                                tool_call_id: update.tool_call_id.to_string(),
                                output: update
                                    .fields
                                    .raw_output
                                    .unwrap_or_else(|| serde_json::json!({})),
                                is_error: false,
                            });
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        })
    }

    fn create_terminal<'life0, 'async_trait>(
        &'life0 self,
        args: CreateTerminalRequest,
    ) -> Pin<Box<dyn Future<Output = AcpResult<CreateTerminalResponse>> + 'async_trait>>
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        Box::pin(async move {
            let command_str = args.command;
            let mut child = self
                .sandbox
                .spawn_command(&command_str, None, None)
                .await
                .map_err(|_| agent_client_protocol::Error::internal_error())?;

            let mut stdout = child
                .take_stdout()
                .ok_or_else(agent_client_protocol::Error::internal_error)?;
            let mut stderr = child
                .take_stderr()
                .ok_or_else(agent_client_protocol::Error::internal_error)?;

            let output_buffer = Arc::new(Mutex::new(String::new()));

            let out_buf_clone = output_buffer.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut buf_out = [0u8; 1024];
                let mut buf_err = [0u8; 1024];
                loop {
                    tokio::select! {
                        res = stdout.read(&mut buf_out) => {
                            match res {
                                Ok(0) => break,
                                Ok(n) => {
                                    if let Ok(s) = std::str::from_utf8(&buf_out[..n]) {
                                        if let Ok(mut locked) = out_buf_clone.lock() {
                                            locked.push_str(s);
                                        }
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        res = stderr.read(&mut buf_err) => {
                            match res {
                                Ok(0) => break,
                                Ok(n) => {
                                    if let Ok(s) = std::str::from_utf8(&buf_err[..n]) {
                                        if let Ok(mut locked) = out_buf_clone.lock() {
                                            locked.push_str(s);
                                        }
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                    }
                }
            });

            let terminal_id = uuid::Uuid::new_v4().to_string();

            let state = TerminalState {
                child: Arc::new(TokioMutex::new(child)),
                output_buffer,
                exit_status: Arc::new(Mutex::new(None)),
            };

            if let Ok(mut terms) = self.terminals.lock() {
                terms.insert(terminal_id.clone(), state);
            }

            Ok(CreateTerminalResponse::new(terminal_id))
        })
    }

    fn terminal_output<'life0, 'async_trait>(
        &'life0 self,
        args: TerminalOutputRequest,
    ) -> Pin<Box<dyn Future<Output = AcpResult<TerminalOutputResponse>> + 'async_trait>>
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        Box::pin(async move {
            let output = {
                let terms = self
                    .terminals
                    .lock()
                    .map_err(|_| agent_client_protocol::Error::internal_error())?;
                let state = terms
                    .get(&args.terminal_id.to_string())
                    .ok_or_else(agent_client_protocol::Error::internal_error)?;
                let mut out_lock = state
                    .output_buffer
                    .lock()
                    .map_err(|_| agent_client_protocol::Error::internal_error())?;
                let res = out_lock.clone();
                out_lock.clear();
                res
            };
            Ok(TerminalOutputResponse::new(output, false))
        })
    }

    fn wait_for_terminal_exit<'life0, 'async_trait>(
        &'life0 self,
        args: WaitForTerminalExitRequest,
    ) -> Pin<Box<dyn Future<Output = AcpResult<WaitForTerminalExitResponse>> + 'async_trait>>
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        Box::pin(async move {
            let (child_arc, status_arc) = {
                let terms = self
                    .terminals
                    .lock()
                    .map_err(|_| agent_client_protocol::Error::internal_error())?;
                let state = terms
                    .get(&args.terminal_id.to_string())
                    .ok_or_else(agent_client_protocol::Error::internal_error)?;
                (state.child.clone(), state.exit_status.clone())
            };

            let exit_code = {
                let mut child = child_arc.lock().await;
                child
                    .wait()
                    .await
                    .map_err(|_| agent_client_protocol::Error::internal_error())?
            };

            if let Ok(mut s) = status_arc.lock() {
                *s = Some(exit_code);
            }

            let exit_status = TerminalExitStatus::new().exit_code(exit_code as u32);
            Ok(WaitForTerminalExitResponse::new(exit_status))
        })
    }

    fn kill_terminal<'life0, 'async_trait>(
        &'life0 self,
        args: KillTerminalRequest,
    ) -> Pin<Box<dyn Future<Output = AcpResult<KillTerminalResponse>> + 'async_trait>>
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        Box::pin(async move {
            let child_arc = {
                let terms = self
                    .terminals
                    .lock()
                    .map_err(|_| agent_client_protocol::Error::internal_error())?;
                let state = terms
                    .get(&args.terminal_id.to_string())
                    .ok_or_else(agent_client_protocol::Error::internal_error)?;
                state.child.clone()
            };

            let mut child = child_arc.lock().await;
            let _ = child.kill().await;

            Ok(KillTerminalResponse::new())
        })
    }

    fn release_terminal<'life0, 'async_trait>(
        &'life0 self,
        args: ReleaseTerminalRequest,
    ) -> Pin<Box<dyn Future<Output = AcpResult<ReleaseTerminalResponse>> + 'async_trait>>
    where
        Self: 'async_trait,
        'life0: 'async_trait,
    {
        Box::pin(async move {
            let child_arc = {
                let mut terms = self
                    .terminals
                    .lock()
                    .map_err(|_| agent_client_protocol::Error::internal_error())?;
                let state = terms
                    .remove(&args.terminal_id.to_string())
                    .ok_or_else(agent_client_protocol::Error::internal_error)?;
                state.child.clone()
            };

            let mut child = child_arc.lock().await;
            let _ = child.kill().await;

            Ok(ReleaseTerminalResponse::new())
        })
    }
}

pub enum AcpCommand {
    Initialize(
        InitializeRequest,
        oneshot::Sender<AcpResult<InitializeResponse>>,
    ),
    NewSession(
        NewSessionRequest,
        oneshot::Sender<AcpResult<NewSessionResponse>>,
    ),
    Prompt(PromptRequest, oneshot::Sender<AcpResult<PromptResponse>>),
}

pub struct AcpTransport {
    pub child: Box<dyn ChildProcess>,
    pub cmd_tx: mpsc::Sender<AcpCommand>,
}

impl AcpTransport {
    pub fn new(
        mut child: Box<dyn ChildProcess>,
        sandbox: Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn ToolHookCallback>>,
        on_event: Arc<dyn Fn(AgentEvent) + Send + Sync>,
        files_touched: Arc<Mutex<HashSet<String>>>,
    ) -> Self {
        let stdin = child
            .take_stdin()
            .expect("Failed to take stdin")
            .compat_write();
        let stdout = child.take_stdout().expect("Failed to take stdout").compat();

        let client = Arc::new(FabroAcpClient {
            sandbox,
            tool_hooks,
            on_event,
            files_touched,
            terminals: Arc::new(Mutex::new(HashMap::new())),
        });

        let (cmd_tx, mut cmd_rx) = mpsc::channel::<AcpCommand>(1024);

        let handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            let local = tokio::task::LocalSet::new();
            handle.block_on(local.run_until(async move {
                let (connection, io_task) = ClientSideConnection::new(client, stdin, stdout, |fut| {
                    tokio::task::spawn_local(fut);
                });

                tokio::task::spawn_local(async move {
                    if let Err(e) = io_task.await {
                        tracing::error!("ACP IO task failed: {}", e);
                    }
                });

                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        AcpCommand::Initialize(req, reply) => {
                            let res = connection.initialize(req).await;
                            let _ = reply.send(res);
                        }
                        AcpCommand::NewSession(req, reply) => {
                            let res = connection.new_session(req).await;
                            let _ = reply.send(res);
                        }
                        AcpCommand::Prompt(req, reply) => {
                            let res = connection.prompt(req).await;
                            let _ = reply.send(res);
                        }
                    }
                }
            }));
        });

        Self { child, cmd_tx }
    }

    pub async fn initialize(&self, req: InitializeRequest) -> anyhow::Result<InitializeResponse> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::Initialize(req, tx))
            .await?;
        let res = rx.await??;
        Ok(res)
    }

    pub async fn new_session(&self, req: NewSessionRequest) -> anyhow::Result<NewSessionResponse> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::NewSession(req, tx))
            .await?;
        let res = rx.await??;
        Ok(res)
    }

    pub async fn prompt(&self, req: PromptRequest) -> anyhow::Result<PromptResponse> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx.send(AcpCommand::Prompt(req, tx)).await?;
        let res = rx.await??;
        Ok(res)
    }
}
