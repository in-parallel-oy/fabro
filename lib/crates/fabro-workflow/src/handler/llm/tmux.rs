//! ponytail: rebase anchor — tmux backend.
//!
//! Interactive tmux backend for `backend = "tmux"` (and the `--backend tmux`
//! run-level override). Unlike the API/ACP backends, fabro does **not** spawn
//! the agent: Overseer owns a long-lived tmux session (created with
//! `tmux new-session`, never by fabro — D7) running a human-driven REPL. Each
//! codergen node turn:
//!
//!   1. snapshots git state,
//!   2. resolves the Overseer session name from the inherited process env
//!      (`OVERSEER_SESSION`, set by Overseer on `new-session`) with a derived
//!      fallback,
//!   3. waits for the per-session attention marker to reach `waiting` (input
//!      arbitration — the `seq` is the gate, the terminal bell is only a hint,
//!      D11),
//!   4. bracketed-pastes the prompt into the pane and submits it (the backend
//!      drives node-to-node submission; the D8 no-Enter rule constrains
//!      Overseer's steer relay, not fabro),
//!   5. awaits the marker `seq` advancing past the pre-paste value,
//!   6. captures the pane output and returns it as the node response, letting
//!      the shared `AgentHandler` routing-extraction chain (response text →
//!      `status.json` → last-touched file) drive routing.
//!
//! Out of scope for `backend = "tmux"`: workflows whose routing needs
//! structured provider stop-reason / token-usage fields beyond the free-text
//! node output. tmux yields only pane text plus the attention marker, so
//! `usage` is always `None` (same as ACP) and routing must come from the node's
//! own emitted text or status file (Task 1.4).

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fabro_agent::{AgentEvent, Sandbox, ToolEnvProvider, shell_quote};
use fabro_graphviz::graph::Node;
use fabro_model::{Catalog, ModelRef, ProviderId, TokenCounts};
use fabro_types::{SessionCapability, StageTiming};
use fabro_util::time::elapsed_ms;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::super::agent::{CodergenBackend, CodergenResult, CodergenRunRequest, OneShotRequest};
use super::api::EffectiveRequestControls;
use super::changed_files;
use crate::context::{Context, WorkflowContext, keys};
use crate::error::Error;
use crate::event::{Emitter, Event, StageScope};
use crate::handler::NodeTimeoutPolicy;
use crate::steering_hub::SteeringHub;

/// Poll cadence for marker reads while gating/awaiting a turn.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Max poll cycles to wait for a freshly-launched pane to settle before the first
/// paste when no marker exists yet (~30s at `POLL_INTERVAL`). A REPL that never
/// quiesces still proceeds after this — a soft cap, not a hard gate.
const SETTLE_MAX_POLLS: u32 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
struct TmuxConversationReuseKey {
    run_id: String,
    thread_id: String,
}

impl TmuxConversationReuseKey {
    fn from_request(request: &CodergenRunRequest<'_>) -> Option<Self> {
        if request.context.fidelity() != keys::Fidelity::Full {
            return None;
        }
        let thread_id = request.thread_id?.trim();
        if thread_id.is_empty() {
            return None;
        }
        Some(Self {
            run_id: request.context.run_id(),
            thread_id: thread_id.to_string(),
        })
    }
}

/// Per-session attention marker written by Overseer at
/// `<worktree>/.overseer/<session>.json`. Only `state` and `seq` gate fabro;
/// `session`/`at` are advisory and tolerated-if-absent.
#[derive(Debug, Clone, Deserialize)]
struct AttentionMarker {
    #[serde(default)]
    state: String,
    #[serde(default)]
    seq: u64,
}

/// Interactive tmux backend. Mirrors the ACP builder shape so `initialize.rs`
/// wiring stays uniform. No credential resolver: the interactive REPL uses the
/// human's own local auth (D2).
pub struct TmuxBackend {
    #[allow(dead_code, reason = "reserved for future env injection into tmux pane")]
    tool_env: Option<Arc<dyn ToolEnvProvider>>,
    #[allow(dead_code, reason = "reserved for steer relay integration (D8)")]
    steering_hub: Option<Arc<SteeringHub>>,
    #[allow(dead_code, reason = "reserved for future model-control resolution")]
    catalog: Option<Arc<Catalog>>,
}

impl Default for TmuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl TmuxBackend {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tool_env: None,
            steering_hub: None,
            catalog: None,
        }
    }

    #[must_use]
    pub fn with_tool_env_provider(mut self, provider: Arc<dyn ToolEnvProvider>) -> Self {
        self.tool_env = Some(provider);
        self
    }

    #[must_use]
    pub fn with_steering_hub(mut self, steering_hub: Arc<SteeringHub>) -> Self {
        self.steering_hub = Some(steering_hub);
        self
    }

    #[must_use]
    pub fn with_catalog(mut self, catalog: Arc<Catalog>) -> Self {
        self.catalog = Some(catalog);
        self
    }
}

/// Read the Overseer session handshake from the inherited process env. Fabro
/// attaches to a session Overseer spawned, so these arrive via the inherited
/// environment rather than fabro plumbing.
#[expect(
    clippy::disallowed_methods,
    reason = "tmux backend reads the Overseer session handshake from the inherited process env"
)]
fn overseer_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Resolve the tmux session name to attach to. Prefer the `OVERSEER_SESSION`
/// handshake; otherwise derive a stable fallback from the node + run id.
/// Never emits `tmux new-session` (D7).
fn resolve_session(node: &Node, context: &Context) -> String {
    overseer_env("OVERSEER_SESSION").unwrap_or_else(|| {
        let kind = node.handler_type().unwrap_or("agent");
        format!("overseer_{kind}_{}", context.run_id())
    })
}

/// Resolve the worktree path used for marker / run-state files. Prefer the
/// `OVERSEER_WORKTREE` handshake; otherwise the sandbox working directory.
fn resolve_worktree(sandbox: &Arc<dyn Sandbox>) -> String {
    overseer_env("OVERSEER_WORKTREE").unwrap_or_else(|| sandbox.working_directory().to_string())
}

/// Best-effort read of the attention marker. A missing / unparseable marker
/// returns `None` (treated as "no gating signal available").
async fn read_marker(sandbox: &Arc<dyn Sandbox>, marker_path: &str) -> Option<AttentionMarker> {
    let cmd = format!("cat {} 2>/dev/null", shell_quote(marker_path));
    let result = sandbox
        .exec_command(&cmd, 5_000, None, None, None)
        .await
        .ok()?;
    if !result.is_success() {
        return None;
    }
    serde_json::from_str::<AttentionMarker>(result.stdout.trim()).ok()
}

/// Sleep one poll interval or fail fast on cancellation.
async fn poll_sleep(cancel_token: &CancellationToken) -> Result<(), Error> {
    tokio::select! {
        () = cancel_token.cancelled() => Err(Error::Cancelled),
        () = tokio::time::sleep(POLL_INTERVAL) => Ok(()),
    }
}

/// Returns true once `deadline` (if any) has elapsed.
fn deadline_passed(start: Instant, deadline: Option<Duration>) -> bool {
    deadline.is_some_and(|d| start.elapsed() >= d)
}

impl TmuxBackend {
    fn require_supported_handle(
        &self,
        conversation_key: Option<&TmuxConversationReuseKey>,
    ) -> Result<(), Error> {
        if let Some(key) = conversation_key {
            return Err(Error::handler(format!(
                "backend=\"tmux\" cannot safely run full-fidelity thread \"{}\" for run \"{}\" until workflow live-handle provisioning is available",
                key.thread_id, key.run_id
            )));
        }
        Ok(())
    }

    fn tmux_model_ref(node: &Node) -> ModelRef {
        ModelRef {
            provider: ProviderId::from(node.provider().unwrap_or("tmux")),
            model_id: node.model().unwrap_or("interactive-terminal").to_string(),
            speed: None,
        }
    }

    fn emit_session_started(emitter: &Arc<Emitter>, stage_scope: &StageScope, session_id: &str) {
        emitter.emit_scoped(
            &Event::AgentSessionStarted {
                session_id: session_id.to_string(),
                parent_session_id: None,
                provider: Some("tmux".to_string()),
                model: Some("interactive-terminal".to_string()),
            },
            stage_scope,
        );
    }

    fn emit_session_activated(
        emitter: &Arc<Emitter>,
        stage_scope: &StageScope,
        node: &Node,
        session_id: &str,
        thread_id: Option<&str>,
    ) {
        emitter.emit_scoped(
            &Event::AgentSessionActivated {
                node_id: node.id.clone(),
                visit: stage_scope.visit,
                session_id: session_id.to_string(),
                thread_id: thread_id.map(str::to_string),
                provider: Some("tmux".to_string()),
                model: Some("interactive-terminal".to_string()),
                reasoning_effort: None,
                speed: None,
                permission_level: None,
                capabilities: vec![SessionCapability::Steer],
            },
            stage_scope,
        );
    }

    fn emit_agent_event(
        emitter: &Arc<Emitter>,
        stage_scope: &StageScope,
        node: &Node,
        session_id: &str,
        event: AgentEvent,
    ) {
        emitter.emit_scoped(
            &Event::Agent {
                stage: node.id.clone(),
                visit: stage_scope.visit,
                event,
                session_id: Some(session_id.to_string()),
                parent_session_id: None,
                tool_call_id: None,
            },
            stage_scope,
        );
    }

    fn emit_turn_failure(
        emitter: &Arc<Emitter>,
        stage_scope: &StageScope,
        node: &Node,
        session_id: &str,
        error: &Error,
    ) {
        Self::emit_agent_event(
            emitter,
            stage_scope,
            node,
            session_id,
            AgentEvent::Warning {
                kind: "tmux_turn_failed".to_string(),
                message: error.to_string(),
                details: serde_json::json!({
                    "backend": "tmux",
                    "nodeId": node.id,
                }),
            },
        );
    }

    fn emit_session_closed(
        emitter: &Arc<Emitter>,
        stage_scope: &StageScope,
        node: &Node,
        session_id: &str,
    ) {
        emitter.emit_scoped(
            &Event::AgentSessionDeactivated {
                node_id: node.id.clone(),
                visit: stage_scope.visit,
                session_id: session_id.to_string(),
            },
            stage_scope,
        );
        emitter.emit_scoped(
            &Event::AgentSessionEnded {
                session_id: session_id.to_string(),
                parent_session_id: None,
            },
            stage_scope,
        );
    }

    /// Block until the session marker reports `waiting` (ready for input). A
    /// missing marker is treated as ready so a freshly-attached session that
    /// has not yet written one is not deadlocked. Returns the `seq` observed at
    /// the moment input is permitted.
    async fn await_ready(
        &self,
        sandbox: &Arc<dyn Sandbox>,
        session: &str,
        marker_path: &str,
        emitter: &Arc<Emitter>,
        cancel_token: &CancellationToken,
        deadline: Option<Duration>,
    ) -> Result<u64, Error> {
        let start = Instant::now();
        loop {
            match read_marker(sandbox, marker_path).await {
                Some(marker) if marker.state == "waiting" => return Ok(marker.seq),
                // No marker yet — typically the first turn: a freshly-launched REPL hasn't
                // emitted a hook event (the marker is written on `Stop`/`PreToolUse`, none
                // of which has fired). Don't paste into a still-booting TUI (the keystrokes
                // are dropped and the turn hangs forever). Wait for the pane to settle, then
                // proceed against seq 0.
                None => {
                    self.await_pane_settled(sandbox, session, emitter, cancel_token)
                        .await?;
                    return Ok(0);
                }
                Some(marker) => {
                    if deadline_passed(start, deadline) {
                        // Soft cap; submit against the last-known seq anyway.
                        return Ok(marker.seq);
                    }
                }
            }
            emitter.touch();
            poll_sleep(cancel_token).await?;
        }
    }

    /// Wait until the pane stops changing — two consecutive identical, non-empty
    /// captures `POLL_INTERVAL` apart — so the first paste lands in a ready REPL, not a
    /// booting one. Bounded by `SETTLE_MAX_POLLS`; a pane that never quiesces still
    /// proceeds (best-effort, never a deadlock). Capture failures are treated as
    /// "not settled yet" and keep polling.
    async fn await_pane_settled(
        &self,
        sandbox: &Arc<dyn Sandbox>,
        session: &str,
        emitter: &Arc<Emitter>,
        cancel_token: &CancellationToken,
    ) -> Result<(), Error> {
        let mut prev: Option<String> = None;
        for _ in 0..SETTLE_MAX_POLLS {
            poll_sleep(cancel_token).await?;
            let snap = self
                .capture_pane(sandbox, session, cancel_token)
                .await
                .unwrap_or_default();
            if !snap.trim().is_empty() && prev.as_deref() == Some(snap.as_str()) {
                return Ok(());
            }
            prev = Some(snap);
            emitter.touch();
        }
        Ok(())
    }

    /// Block until the marker reports the node turn finished: `seq` advanced
    /// past `seq_before` **and** `state == "waiting"`. The `seq` alone is not the
    /// gate — Overseer's hook bumps `seq` on *every* mapped event (`PreToolUse`
    /// → `working`, permission → `blocked`, `Stop` → `waiting`), so a coding
    /// node's first tool call would otherwise look "complete" mid-turn. Node
    /// completion is specifically the `Stop` → `waiting` transition; `working`/
    /// `blocked` are in-progress and keep us polling (a `blocked` permission
    /// prompt is resolved by the human at the pane, then the turn `Stop`s).
    /// Honors cancellation and the optional node deadline.
    async fn await_turn_complete(
        &self,
        sandbox: &Arc<dyn Sandbox>,
        marker_path: &str,
        seq_before: u64,
        emitter: &Arc<Emitter>,
        cancel_token: &CancellationToken,
        deadline: Option<Duration>,
    ) -> Result<(), Error> {
        let start = Instant::now();
        loop {
            if let Some(marker) = read_marker(sandbox, marker_path).await {
                if marker.seq > seq_before && marker.state == "waiting" {
                    return Ok(());
                }
            }
            if deadline_passed(start, deadline) {
                return Err(Error::handler(
                    "tmux turn timed out waiting for the attention marker to advance".to_string(),
                ));
            }
            emitter.touch();
            poll_sleep(cancel_token).await?;
        }
    }

    /// Bracketed-paste `prompt` into the session and submit it. Writes the
    /// prompt to a temp file and `load-buffer`s it (avoids shell-escaping a
    /// large prompt through `set-buffer`).
    async fn send_prompt(
        &self,
        sandbox: &Arc<dyn Sandbox>,
        session: &str,
        prompt: &str,
        cancel_token: &CancellationToken,
    ) -> Result<(), Error> {
        let prompt_path = format!("/tmp/fabro_tmux_{}.txt", uuid::Uuid::new_v4());
        sandbox
            .write_file(&prompt_path, prompt)
            .await
            .map_err(|e| Error::handler_with_source("Failed to write tmux prompt file", e))?;
        // load-buffer → paste-buffer -p (bracketed) → Enter to submit. `&&`-chained so a
        // failure (e.g. the target session vanished) propagates as a non-zero exit; the
        // `rm` cleanup runs in an EXIT trap so it can't mask that failure (a plain
        // trailing `rm` would make the whole command "succeed" even when the paste never
        // landed — which silently hangs the turn).
        let cmd = format!(
            "trap 'rm -f {file}' EXIT; \
             tmux load-buffer -- {file} && \
             tmux paste-buffer -p -t {session} && \
             tmux send-keys -t {session} Enter",
            file = shell_quote(&prompt_path),
            session = shell_quote(session),
        );
        let result = sandbox
            .exec_command(&cmd, 30_000, None, None, Some(cancel_token.child_token()))
            .await
            .map_err(|e| {
                Error::handler_with_source("Failed to paste prompt into tmux session", e)
            })?;
        if !result.is_success() {
            return Err(Error::handler(format!(
                "tmux paste failed (session \"{session}\"): {}",
                result.stderr.trim()
            )));
        }
        Ok(())
    }

    /// Capture the pane contents (including scrollback) as the node response
    /// text. `-S -` starts the capture at the top of the history so a node turn
    /// whose output exceeds one screen is not silently truncated before the
    /// routing-extraction chain sees it (`-p` alone captures only the visible
    /// viewport).
    async fn capture_pane(
        &self,
        sandbox: &Arc<dyn Sandbox>,
        session: &str,
        cancel_token: &CancellationToken,
    ) -> Result<String, Error> {
        let cmd = format!("tmux capture-pane -p -S - -t {}", shell_quote(session));
        let result = sandbox
            .exec_command(&cmd, 30_000, None, None, Some(cancel_token.child_token()))
            .await
            .map_err(|e| Error::handler_with_source("Failed to capture tmux pane", e))?;
        if !result.is_success() {
            return Err(Error::handler(format!(
                "tmux capture-pane failed (session \"{session}\"): {}",
                result.stderr.trim()
            )));
        }
        Ok(result.stdout.trim_end().to_string())
    }
}

#[async_trait]
impl CodergenBackend for TmuxBackend {
    async fn run(&self, request: CodergenRunRequest<'_>) -> Result<CodergenResult, Error> {
        let node = request.node;
        let context = request.context;
        let emitter = request.emitter;
        let sandbox = request.sandbox;
        let launch_start = Instant::now();
        let stage_scope = StageScope::for_handler(context, &node.id);
        let session_id = format!("tmux-{}", uuid::Uuid::new_v4());
        let conversation_key = TmuxConversationReuseKey::from_request(&request);
        let cancel_token = request.cancel_token;

        Self::emit_session_started(emitter, &stage_scope, &session_id);
        Self::emit_session_activated(emitter, &stage_scope, node, &session_id, request.thread_id);

        if let Err(err) = self.require_supported_handle(conversation_key.as_ref()) {
            Self::emit_turn_failure(emitter, &stage_scope, node, &session_id, &err);
            Self::emit_session_closed(emitter, &stage_scope, node, &session_id);
            return Err(err);
        }

        // 1. Snapshot git state before the turn.
        let files_before = changed_files::detect_changed_files(sandbox).await;

        // 2/3. Resolve the Overseer session + worktree (attach, never spawn).
        let session = resolve_session(node, context);
        let worktree = resolve_worktree(sandbox);
        let marker_path = format!("{worktree}/.overseer/{session}.json");
        let deadline = node.timeout();

        // 4/5. Input arbitration: wait until the pane is `waiting`, recording
        //      the seq to detect the turn's completion.
        let seq_before = self
            .await_ready(
                sandbox,
                &session,
                &marker_path,
                emitter,
                &cancel_token,
                deadline,
            )
            .await
            .inspect_err(|err| {
                Self::emit_turn_failure(emitter, &stage_scope, node, &session_id, err);
                Self::emit_session_closed(emitter, &stage_scope, node, &session_id);
            })?;

        // 6. Bracketed-paste + submit.
        Self::emit_agent_event(
            emitter,
            &stage_scope,
            node,
            &session_id,
            AgentEvent::UserInput {
                text: request.prompt.to_string(),
            },
        );
        self.send_prompt(sandbox, &session, request.prompt, &cancel_token)
            .await
            .inspect_err(|err| {
                Self::emit_turn_failure(emitter, &stage_scope, node, &session_id, err);
                Self::emit_session_closed(emitter, &stage_scope, node, &session_id);
            })?;

        // 7. Await the marker advancing past seq_before.
        self.await_turn_complete(
            sandbox,
            &marker_path,
            seq_before,
            emitter,
            &cancel_token,
            deadline,
        )
        .await
        .inspect_err(|err| {
            Self::emit_turn_failure(emitter, &stage_scope, node, &session_id, err);
            Self::emit_session_closed(emitter, &stage_scope, node, &session_id);
        })?;

        // 8. Capture pane output as the node response.
        let text = self
            .capture_pane(sandbox, &session, &cancel_token)
            .await
            .inspect_err(|err| {
                Self::emit_turn_failure(emitter, &stage_scope, node, &session_id, err);
                Self::emit_session_closed(emitter, &stage_scope, node, &session_id);
            })?;
        Self::emit_agent_event(
            emitter,
            &stage_scope,
            node,
            &session_id,
            AgentEvent::AssistantMessage {
                text: text.clone(),
                model: Self::tmux_model_ref(node),
                usage: TokenCounts::default(),
                tool_call_count: 0,
                context_window: None,
            },
        );
        Self::emit_agent_event(
            emitter,
            &stage_scope,
            node,
            &session_id,
            AgentEvent::ProcessingEnd,
        );
        Self::emit_session_closed(emitter, &stage_scope, node, &session_id);

        // Detect changed files for routing fallbacks.
        let (files_touched, last_file_touched) =
            changed_files::files_touched_since(sandbox, &files_before).await;

        // Hand the pane text back; the shared AgentHandler extracts routing from
        // the response text / status.json / last-touched file. usage is None —
        // tmux exposes no token accounting (D2). The run-state artifact
        // (.overseer/run.json) is published by the run lifecycle
        // (`lifecycle::run_state`), which alone sees the full graph, the routing
        // outcome, and the terminal node — not this per-turn backend.
        Ok(CodergenResult::Text {
            text,
            usage: None,
            files_touched,
            last_file_touched,
            timing: StageTiming::wall_only(elapsed_ms(launch_start)),
        })
    }

    async fn one_shot(&self, _request: OneShotRequest<'_>) -> Result<CodergenResult, Error> {
        Err(Error::Validation(
            "backend=\"tmux\" is only valid for interactive agent runs".into(),
        ))
    }

    async fn shutdown(&self, _emitter: &Arc<Emitter>) {
        // No-op: the tmux session is owned by Overseer (D7), not fabro.
    }

    fn effective_request_controls(&self, _node: &Node) -> Result<EffectiveRequestControls, Error> {
        // No model controls are meaningful for a human-driven REPL.
        Ok(EffectiveRequestControls::default())
    }

    fn node_timeout_policy(&self, _node: &Node) -> NodeTimeoutPolicy {
        // Interactive turns have no protocol deadline; the executor's
        // wall-clock bounds the marker await.
        NodeTimeoutPolicy::ExecutorEnforced
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabro_agent::{AgentToolRuntime, LocalSandbox};
    use serde_json::json;

    fn request_context(fidelity: keys::Fidelity) -> Context {
        let context = Context::new();
        context.set(keys::INTERNAL_FIDELITY, json!(fidelity.to_string()));
        context
    }

    fn request_for<'a>(
        node: &'a Node,
        context: &'a Context,
        emitter: &'a Arc<Emitter>,
        sandbox: &'a Arc<dyn Sandbox>,
        thread_id: Option<&'a str>,
    ) -> CodergenRunRequest<'a> {
        CodergenRunRequest {
            node,
            prompt: "prompt",
            context,
            thread_id,
            emitter,
            sandbox,
            tool_hooks: None,
            cancel_token: CancellationToken::new(),
            agent_tool_runtime: AgentToolRuntime::default(),
        }
    }

    #[test]
    fn full_fidelity_thread_computes_reuse_key_and_fails_closed() {
        let node = Node::new("work");
        let context = request_context(keys::Fidelity::Full);
        context.set(keys::INTERNAL_RUN_ID, json!("run-123"));
        let emitter = Arc::new(Emitter::default());
        let dir = tempfile::tempdir().unwrap();
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(dir.path().to_path_buf()));
        let request = request_for(&node, &context, &emitter, &sandbox, Some("shared"));

        assert_eq!(
            TmuxConversationReuseKey::from_request(&request),
            Some(TmuxConversationReuseKey {
                run_id: "run-123".to_string(),
                thread_id: "shared".to_string(),
            })
        );
        let err = TmuxBackend::new()
            .require_supported_handle(TmuxConversationReuseKey::from_request(&request).as_ref())
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("cannot safely run full-fidelity thread"),
            "{err}"
        );
    }

    #[test]
    fn non_full_threaded_turn_is_allowed() {
        let node = Node::new("work");
        let context = request_context(keys::Fidelity::SummaryHigh);
        let emitter = Arc::new(Emitter::default());
        let dir = tempfile::tempdir().unwrap();
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(dir.path().to_path_buf()));
        let request = request_for(&node, &context, &emitter, &sandbox, Some("shared"));

        assert_eq!(TmuxConversationReuseKey::from_request(&request), None);
        TmuxBackend::new()
            .require_supported_handle(TmuxConversationReuseKey::from_request(&request).as_ref())
            .expect("non-full fidelity does not require tmux thread reuse");
    }

    #[test]
    fn full_fidelity_blank_thread_has_no_reuse_key() {
        let node = Node::new("work");
        let context = request_context(keys::Fidelity::Full);
        let emitter = Arc::new(Emitter::default());
        let dir = tempfile::tempdir().unwrap();
        let sandbox: Arc<dyn Sandbox> = Arc::new(LocalSandbox::new(dir.path().to_path_buf()));
        let request = request_for(&node, &context, &emitter, &sandbox, Some("  "));

        assert_eq!(TmuxConversationReuseKey::from_request(&request), None);
    }
}
