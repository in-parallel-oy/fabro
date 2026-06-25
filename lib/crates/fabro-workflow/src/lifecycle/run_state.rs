//! ponytail: rebase anchor — tmux backend.
//!
//! Publishes the **run-state artifact** at `<worktree>/.overseer/run.json`
//! (D9 / task 1.5) — the contract Overseer's `workflow_status` and DAG surface
//! read. It is distinct from the per-session attention *marker* (which the
//! installed agent hook writes and the tmux backend gates on).
//!
//! Why a lifecycle delegate and not the tmux backend: the backend only sees a
//! single agent-node turn. The full node list + edges live on the graph, the
//! per-node outcome is decided by the executor *after* the backend returns, and
//! the terminal node is an `exit` node the backend never runs at all — so only
//! the run lifecycle can populate `nodes`, `pendingGate`, and `terminalReached`
//! correctly. The fields and JSON casing here mirror Overseer's `WorkflowStatus`
//! decoder verbatim (`runId`, `currentNode`, `nodes[{id,label,outcome,visits}]`,
//! `edges`, `pendingGate`, `terminalReached`).
//!
//! Inert unless Overseer is supervising: publishing is gated on the
//! `OVERSEER_WORKTREE` handshake env (the worktree Overseer scans + where the
//! marker lives). A normal headless `fabro run` writes nothing.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fabro_agent::shell_quote;
use fabro_core::error::Result as CoreResult;
use fabro_core::lifecycle::{NodeDecision, RunLifecycle};
use fabro_core::state::ExecutionState;
use fabro_sandbox::Sandbox;
use fabro_types::graph::Graph as GvGraph;
use fabro_types::{RunId, StageOutcome};
use serde::Serialize;

use super::event::stage_scope_for;
use crate::graph::{WorkflowGraph, WorkflowNode};
use crate::outcome::BilledModelUsage;
use crate::workflow_agent_session;

type WfRunState = ExecutionState<Option<BilledModelUsage>>;
type WfNodeResult = fabro_core::outcome::NodeResult<Option<BilledModelUsage>>;
type WfNodeDecision = NodeDecision<Option<BilledModelUsage>>;

const OUTCOME_PENDING: &str = "pending";
const OUTCOME_RUNNING: &str = "running";
const OUTCOME_SUCCEEDED: &str = "succeeded";
const OUTCOME_FAILED: &str = "failed";
const OUTCOME_SKIPPED: &str = "skipped";

/// Map a finished node's `StageOutcome` onto Overseer's `WorkflowNode.Outcome`
/// vocabulary. `PartiallySucceeded` reads as `succeeded` (the surface has no
/// partial bucket; routing already decided whether it advances).
fn outcome_word(status: StageOutcome) -> &'static str {
    match status {
        StageOutcome::Succeeded | StageOutcome::PartiallySucceeded => OUTCOME_SUCCEEDED,
        StageOutcome::Failed { .. } => OUTCOME_FAILED,
        StageOutcome::Skipped => OUTCOME_SKIPPED,
    }
}

/// Read the Overseer worktree handshake from the inherited process env.
#[expect(
    clippy::disallowed_methods,
    reason = "run-state publisher gates on the Overseer worktree handshake from the inherited env"
)]
fn overseer_worktree() -> Option<String> {
    std::env::var("OVERSEER_WORKTREE")
        .ok()
        .filter(|v| !v.is_empty())
}

#[derive(Clone)]
struct NodeProgress {
    label: String,
    outcome: &'static str,
    /// Visit count for back-edge loops (verify→fix→verify); 0 until first entry.
    visits: u32,
}

struct Inner {
    current_node: Option<String>,
    pending_gate: bool,
    terminal_reached: bool,
    nodes: BTreeMap<String, NodeProgress>,
    current_node_metadata: Option<CurrentNodeMetadata>,
    metadata_generation: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunStateDoc<'a> {
    run_id: String,
    current_node: Option<&'a str>,
    nodes: Vec<NodeEntry<'a>>,
    edges: Vec<[&'a str; 2]>,
    pending_gate: bool,
    terminal_reached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_node_metadata: Option<&'a CurrentNodeMetadata>,
}

#[derive(Serialize)]
struct NodeEntry<'a> {
    id: &'a str,
    label: &'a str,
    outcome: &'static str,
    visits: u32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CurrentNodeMetadata {
    schema_version: u32,
    run_id: String,
    node_id: String,
    stage_id: String,
    visit: u32,
    agent_session_id: String,
    generation: u64,
    backend: String,
}

/// Sub-lifecycle that publishes `<worktree>/.overseer/run.json` at each node
/// boundary. No-op when `OVERSEER_WORKTREE` is unset.
pub(crate) struct RunStatePublisher {
    sandbox: Arc<dyn Sandbox>,
    graph: Arc<GvGraph>,
    run_id: RunId,
    worktree: Option<String>,
    inner: Mutex<Inner>,
}

impl RunStatePublisher {
    pub(crate) fn new(sandbox: Arc<dyn Sandbox>, graph: Arc<GvGraph>, run_id: RunId) -> Self {
        let nodes = graph
            .nodes
            .values()
            .map(|n| {
                (
                    n.id.clone(),
                    NodeProgress {
                        label: n.label().to_string(),
                        outcome: OUTCOME_PENDING,
                        visits: 0,
                    },
                )
            })
            .collect();
        Self {
            sandbox,
            worktree: overseer_worktree(),
            graph,
            run_id,
            inner: Mutex::new(Inner {
                current_node: None,
                pending_gate: false,
                terminal_reached: false,
                nodes,
                current_node_metadata: None,
                metadata_generation: 0,
            }),
        }
    }

    /// True iff a human-gate node (`hexagon` → `human` handler) is awaiting a
    /// decision — D9's explicit pending-gate flag, never inferred from a bare
    /// `waiting` marker.
    fn is_gate(node: &WorkflowNode) -> bool {
        node.0.handler_type() == Some("human")
    }

    /// Serialize the accumulated state and write it atomically (temp + rename)
    /// so Overseer never reads a partial file. Best-effort: a sandbox failure is
    /// swallowed (the surface degrades to "unreadable", never crashes the run).
    async fn publish(&self) {
        let Some(worktree) = self.worktree.as_deref() else {
            return;
        };
        let json = {
            let inner = self.inner.lock().expect("run-state mutex poisoned");
            let nodes: Vec<NodeEntry<'_>> = inner
                .nodes
                .iter()
                .map(|(id, p)| NodeEntry {
                    id,
                    label: &p.label,
                    outcome: p.outcome,
                    visits: p.visits,
                })
                .collect();
            let edges: Vec<[&str; 2]> = self
                .graph
                .edges
                .iter()
                .map(|e| [e.from.as_str(), e.to.as_str()])
                .collect();
            let doc = RunStateDoc {
                run_id: self.run_id.to_string(),
                current_node: inner.current_node.as_deref(),
                nodes,
                edges,
                pending_gate: inner.pending_gate,
                terminal_reached: inner.terminal_reached,
                current_node_metadata: inner.current_node_metadata.as_ref(),
            };
            match serde_json::to_string(&doc) {
                Ok(json) => json,
                Err(_) => return,
            }
        };

        let dir = format!("{worktree}/.overseer");
        let tmp = format!("{dir}/run.json.tmp");
        let final_path = format!("{dir}/run.json");
        let _ = self
            .sandbox
            .exec_command(
                &format!("mkdir -p {}", shell_quote(&dir)),
                5_000,
                None,
                None,
                None,
            )
            .await;
        if self.sandbox.write_file(&tmp, &json).await.is_ok() {
            let _ = self
                .sandbox
                .exec_command(
                    &format!("mv -f {} {}", shell_quote(&tmp), shell_quote(&final_path)),
                    5_000,
                    None,
                    None,
                    None,
                )
                .await;
        }
    }
}

#[async_trait]
impl RunLifecycle<WorkflowGraph> for RunStatePublisher {
    async fn on_run_start(&self, _graph: &WorkflowGraph, state: &WfRunState) -> CoreResult<()> {
        if self.worktree.is_none() {
            return Ok(());
        }
        {
            let mut inner = self.inner.lock().expect("run-state mutex poisoned");
            // Seed outcomes from any restored checkpoint state (resume): nodes
            // that already completed must not re-read as `pending`.
            for (id, outcome) in &state.node_outcomes {
                if let Some(p) = inner.nodes.get_mut(id) {
                    p.outcome = outcome_word(outcome.status);
                    if p.visits == 0 {
                        p.visits = 1;
                    }
                }
            }
            let current = state.current_node_id.clone();
            inner.pending_gate = false;
            inner.terminal_reached = false;
            inner.current_node = (!current.is_empty()).then_some(current);
            inner.current_node_metadata = None;
        }
        self.publish().await;
        Ok(())
    }

    async fn before_node(
        &self,
        node: &WorkflowNode,
        state: &WfRunState,
    ) -> CoreResult<WfNodeDecision> {
        if self.worktree.is_some() {
            {
                let mut inner = self.inner.lock().expect("run-state mutex poisoned");
                inner.current_node = Some(node.0.id.clone());
                inner.pending_gate = Self::is_gate(node);
                inner.current_node_metadata = None;
                let visit = if let Some(p) = inner.nodes.get_mut(&node.0.id) {
                    p.outcome = OUTCOME_RUNNING;
                    p.visits = p.visits.saturating_add(1);
                    Some(p.visits)
                } else {
                    None
                };
                if visit.is_some() && node.0.handler_type() == Some("agent") {
                    inner.metadata_generation = inner.metadata_generation.saturating_add(1);
                    let run_id = self.run_id.to_string();
                    let scope = stage_scope_for(state, &node.0.id);
                    inner.current_node_metadata = Some(CurrentNodeMetadata {
                        schema_version: 1,
                        run_id: run_id.clone(),
                        node_id: node.0.id.clone(),
                        stage_id: scope.stage_id().to_string(),
                        visit: scope.visit,
                        agent_session_id: workflow_agent_session::session_id_for_scope(
                            &run_id, &scope, "tmux",
                        ),
                        generation: inner.metadata_generation,
                        backend: "tmux".to_string(),
                    });
                }
            }
            self.publish().await;
        }
        Ok(NodeDecision::Continue)
    }

    async fn after_node(
        &self,
        node: &WorkflowNode,
        result: &mut WfNodeResult,
        _state: &WfRunState,
    ) -> CoreResult<()> {
        if self.worktree.is_some() {
            {
                let mut inner = self.inner.lock().expect("run-state mutex poisoned");
                // The node finished; any gate it represented is resolved until a
                // later `before_node` re-arms it.
                inner.pending_gate = false;
                inner.current_node_metadata = None;
                if let Some(p) = inner.nodes.get_mut(&node.0.id) {
                    p.outcome = outcome_word(result.outcome.status);
                }
            }
            self.publish().await;
        }
        Ok(())
    }

    async fn on_terminal_reached(
        &self,
        _node: &WorkflowNode,
        _goal_gates_passed: bool,
        _state: &WfRunState,
    ) {
        if self.worktree.is_some() {
            {
                let mut inner = self.inner.lock().expect("run-state mutex poisoned");
                inner.terminal_reached = true;
                inner.pending_gate = false;
                inner.current_node_metadata = None;
            }
            self.publish().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_node_metadata_serializes_with_overseer_contract_fields() {
        let metadata = CurrentNodeMetadata {
            schema_version: 1,
            run_id: "run-1".to_string(),
            node_id: "verify".to_string(),
            stage_id: "verify@2".to_string(),
            visit: 2,
            agent_session_id: "workflow-tmux-run-1-verify-v2".to_string(),
            generation: 7,
            backend: "tmux".to_string(),
        };
        let json = serde_json::to_value(&metadata).unwrap();
        assert_eq!(json["schemaVersion"], 1);
        assert_eq!(json["runId"], "run-1");
        assert_eq!(json["nodeId"], "verify");
        assert_eq!(json["stageId"], "verify@2");
        assert_eq!(json["visit"], 2);
        assert_eq!(json["agentSessionId"], "workflow-tmux-run-1-verify-v2");
        assert_eq!(json["generation"], 7);
    }
}
