use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use fabro_agent::Sandbox;
use fabro_agent::acp::AcpTransport;
use fabro_agent::profiles::{assemble_system_prompt, EnvContext};
use agent_client_protocol::{
    InitializeRequest, NewSessionRequest, PromptRequest, ClientCapabilities,
    ContentBlock, TextContent, ProtocolVersion, Implementation, FileSystemCapabilities,
};

use crate::context::Context;
use crate::error::FabroError;
use crate::event::EventEmitter;
use crate::handler::agent::{CodergenBackend, CodergenResult};
use fabro_graphviz::graph::Node;

pub struct AcpCodergenBackend {
    pub command: String,
}

impl AcpCodergenBackend {
    pub fn new(command: String) -> Self {
        Self { command }
    }
}

#[async_trait]
impl CodergenBackend for AcpCodergenBackend {
    async fn run(
        &self,
        _node: &Node,
        prompt: &str,
        _context: &Context,
        _thread_id: Option<&str>,
        _emitter: &Arc<EventEmitter>,
        _stage_dir: &Path,
        sandbox: &Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<CodergenResult, FabroError> {
        let child = sandbox
            .spawn_command(&self.command, None, None)
            .await
            .map_err(|e| FabroError::handler(format!("Failed to spawn ACP agent: {}", e)))?;

        let emitter_clone = Arc::clone(_emitter);
        let node_id = _node.id.clone();
        let on_event = Arc::new(move |event: fabro_agent::AgentEvent| {
            emitter_clone.emit(&crate::event::WorkflowRunEvent::Agent {
                stage: node_id.clone(),
                event,
            });
        });

        let mut transport = AcpTransport::new(child, tool_hooks, on_event);

        // 1. Initialize
        let mut init_req = InitializeRequest::new(ProtocolVersion::LATEST);
        init_req.client_info = Some(Implementation::new("fabro", env!("CARGO_PKG_VERSION")));
        
        let mut fs_caps = FileSystemCapabilities::new();
        fs_caps.read_text_file = true;
        fs_caps.write_text_file = true;
        
        let mut caps = ClientCapabilities::new();
        caps.fs = fs_caps;
        caps.terminal = true;
        
        init_req.client_capabilities = caps;

        let _init_res = transport
            .initialize(init_req)
            .await
            .map_err(|e| FabroError::handler(format!("ACP initialize failed: {}", e)))?;

        // Generate System Prompt
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let git_branch = sandbox
            .exec_command("git rev-parse --abbrev-ref HEAD", 5000, None, None, None)
            .await
            .ok()
            .filter(|r| r.exit_code == 0)
            .map(|r| r.stdout.trim().to_string());
        let is_git_repo = git_branch.is_some();
        let git_status_short = if is_git_repo {
            sandbox
                .exec_command("git status --short", 5000, None, None, None)
                .await
                .ok()
                .filter(|r| r.exit_code == 0)
                .map(|r| r.stdout.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        };
        let git_recent_commits = if is_git_repo {
            sandbox
                .exec_command("git log --oneline -10", 5000, None, None, None)
                .await
                .ok()
                .filter(|r| r.exit_code == 0)
                .map(|r| r.stdout.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        };

        let env_context = EnvContext {
            git_branch,
            is_git_repo,
            current_date: today,
            model: "acp-agent".to_string(),
            knowledge_cutoff: "".to_string(),
            git_status_short,
            git_recent_commits,
        };

        let core_prompt = "You are an AI coding agent running in a sandboxed environment.\n\n{env_block}";
        let system_prompt = assemble_system_prompt(
            core_prompt,
            sandbox.as_ref(),
            &env_context,
            &[], // memory
            None, // user_instructions
            &[], // skills
        );

        // 2. New Session
        let mut session_req = NewSessionRequest::new(sandbox.working_directory());
        let mut meta = serde_json::Map::new();
        meta.insert("systemPrompt".to_string(), serde_json::Value::String(system_prompt));
        session_req.meta = Some(meta);

        let session_res = transport
            .new_session(session_req)
            .await
            .map_err(|e| FabroError::handler(format!("ACP new_session failed: {}", e)))?;

        // 3. Prompt
        let prompt_req = PromptRequest::new(
            session_res.session_id,
            vec![ContentBlock::Text(TextContent::new(prompt))],
        );

        let _prompt_res = transport
            .prompt(prompt_req)
            .await
            .map_err(|e| FabroError::handler(format!("ACP prompt failed: {}", e)))?;

        // Extract text from response
        let mut response_text = String::new();
        while let Ok(chunk) = transport.rx.try_recv() {
            response_text.push_str(&chunk);
        }

        Ok(CodergenResult::Text {
            text: response_text,
            usage: None,
            files_touched: Vec::new(),
            last_file_touched: None,
        })
    }
}
