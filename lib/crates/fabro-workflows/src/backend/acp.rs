use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use agent_client_protocol::{
    ClientCapabilities, ContentBlock, FileSystemCapabilities, Implementation, InitializeRequest,
    NewSessionRequest, PromptRequest, ProtocolVersion, TextContent,
};
use async_trait::async_trait;
use fabro_agent::acp::AcpTransport;
use fabro_agent::profiles::{assemble_system_prompt, EnvContext};
use fabro_agent::Sandbox;

use crate::context::Context;
use crate::error::FabroError;
use crate::event::EventEmitter;
use crate::handler::agent::{CodergenBackend, CodergenResult};
use fabro_graphviz::graph::Node;

pub struct AcpCodergenBackend {
    pub command: String,
    pub env: HashMap<String, String>,
}

impl AcpCodergenBackend {
    pub fn new(command: String) -> Self {
        Self {
            command,
            env: HashMap::new(),
        }
    }

    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = env;
        self
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
        stage_dir: &Path,
        sandbox: &Arc<dyn Sandbox>,
        tool_hooks: Option<Arc<dyn fabro_agent::ToolHookCallback>>,
    ) -> Result<CodergenResult, FabroError> {
        let env_ref = if self.env.is_empty() {
            None
        } else {
            Some(&self.env)
        };

        let child = sandbox
            .spawn_command(&self.command, None, env_ref)
            .await
            .map_err(|e| FabroError::handler(format!("Failed to spawn ACP agent: {}", e)))?;

        let emitter_clone = Arc::clone(_emitter);
        let node_id = _node.id.clone();
        let response_text = Arc::new(Mutex::new(String::new()));
        let response_text_clone = Arc::clone(&response_text);
        let on_event = Arc::new(move |event: fabro_agent::AgentEvent| {
            if let fabro_agent::AgentEvent::TextDelta { delta } = &event {
                if let Ok(mut text) = response_text_clone.lock() {
                    text.push_str(delta);
                }
            }
            emitter_clone.emit(&crate::event::WorkflowRunEvent::Agent {
                stage: node_id.clone(),
                event,
            });
        });

        let files_touched_set = Arc::new(Mutex::new(HashSet::new()));
        let mut transport = AcpTransport::new(child, tool_hooks, on_event, Arc::clone(&files_touched_set));

        let protocol_result = async {
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
            let env_context = EnvContext::generate(sandbox.as_ref(), "acp-agent").await;

            let doc_root = sandbox.working_directory().to_string();
            let memory = fabro_agent::memory::discover_memory(
                sandbox.as_ref(),
                &doc_root,
                sandbox.working_directory(),
                fabro_model::Provider::Anthropic,
            )
            .await;

            let home = dirs::home_dir().map(|p| p.to_string_lossy().to_string());
            let skill_dirs = fabro_agent::skills::default_skill_dirs(home.as_deref(), None);
            let skills = fabro_agent::skills::discover_skills(sandbox.as_ref(), &skill_dirs).await;

            let core_prompt =
                "You are an AI coding agent running in a sandboxed environment.\n\n{env_block}";
            let system_prompt = assemble_system_prompt(
                core_prompt,
                sandbox.as_ref(),
                &env_context,
                &memory,
                None, // user_instructions
                &skills,
            );

            // 2. New Session
            let mut session_req = NewSessionRequest::new(sandbox.working_directory());
            let mut meta = serde_json::Map::new();
            meta.insert(
                "systemPrompt".to_string(),
                serde_json::Value::String(system_prompt),
            );
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

            Ok::<(), FabroError>(())
        }
        .await;

        let _ = transport.child.kill().await;

        protocol_result?;

        // Extract text from response
        let final_text = response_text.lock().unwrap().clone();

        let _ = tokio::fs::create_dir_all(stage_dir).await;
        let provider_used = serde_json::json!({
            "mode": "acp",
            "command": &self.command,
        });
        if let Ok(json) = serde_json::to_string_pretty(&provider_used) {
            let _ = tokio::fs::write(stage_dir.join("provider_used.json"), json).await;
        }

        let mut files_touched: Vec<String> = files_touched_set
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect();
        files_touched.sort();

        // Find the most recently modified file by mtime
        let last_file_touched = if !files_touched.is_empty() {
            let quoted_files: Vec<String> = files_touched
                .iter()
                .filter_map(|f| shlex::try_quote(f).ok().map(|q| q.into_owned()))
                .collect();
            let cmd = format!("ls -t {} | head -1", quoted_files.join(" "));
            if let Ok(result) = sandbox.exec_command(&cmd, 5_000, None, None, None).await {
                let trimmed = result.stdout.trim().to_string();
                if result.exit_code == 0 && !trimmed.is_empty() {
                    Some(trimmed)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        Ok(CodergenResult::Text {
            text: final_text,
            usage: None,
            files_touched,
            last_file_touched,
        })
    }
}
