use crate::config::{ToolHookCallback, ToolHookDecision};
use crate::types::AgentEvent;
use agent_client_protocol::{Agent, Client, ClientSideConnection};
use agent_client_protocol::{
    ContentBlock, InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PermissionOptionKind, PromptRequest, PromptResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, Result as AcpResult,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate, ToolCallContent,
};
use fabro_sandbox::ChildProcess;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub struct FabroAcpClient {
    pub tool_hooks: Option<Arc<dyn ToolHookCallback>>,
    pub on_event: Arc<dyn Fn(AgentEvent) + Send + Sync>,
    pub files_touched: Arc<Mutex<HashSet<String>>>,
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
            tool_hooks,
            on_event,
            files_touched,
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
