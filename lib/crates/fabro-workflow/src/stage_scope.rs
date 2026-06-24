use fabro_types::{ParallelBranchId, StageId};

use crate::context::{Context as WfContext, WorkflowContext};
use crate::run_dir::visit_from_context;

/// Stage-level scope threaded through event emission to populate
/// `stage_id` / `parallel_group_id` / `parallel_branch_id` on events
/// that happen inside a concrete stage execution.
#[derive(Clone, Debug)]
pub struct StageScope {
    pub node_id: String,
    pub visit: u32,
    pub parallel_group_id: Option<StageId>,
    pub parallel_branch_id: Option<ParallelBranchId>,
}

impl StageScope {
    /// Build a scope from the given node id, sourcing visit count and parallel
    /// ids from the current context.
    pub fn from_context(context: &WfContext, node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            visit: u32::try_from(visit_from_context(context)).unwrap_or(u32::MAX),
            parallel_group_id: context.parallel_group_id(),
            parallel_branch_id: context.parallel_branch_id(),
        }
    }

    /// Build scope for a handler invocation. Prefers the `current_stage_scope`
    /// seeded by the fidelity lifecycle `before_node` hook, and falls back to
    /// synthesizing one from `node_id` for direct-handler call sites (tests,
    /// etc.) that don't go through the full lifecycle.
    pub fn for_handler(context: &WfContext, node_id: impl Into<String>) -> Self {
        context
            .current_stage_scope()
            .unwrap_or_else(|| Self::from_context(context, node_id))
    }

    /// Build scope for the branch-lifecycle events emitted by the parallel
    /// handler (`ParallelBranchStarted`, `ParallelBranchCompleted`, and the
    /// pre-dispatch `GitCommit` for the branch worktree).
    ///
    /// `target_visit` is the visit count of `target_node_id` for this
    /// particular branch dispatch. The parallel handler currently passes
    /// `1` because branches haven't been re-entered yet at the point of
    /// scope construction; a future change that loops a parallel node
    /// must pass the actual visit so envelope `stage_id`s stay accurate.
    #[must_use]
    pub fn for_parallel_branch(
        target_node_id: impl Into<String>,
        target_visit: u32,
        parallel_group_id: StageId,
        parallel_branch_id: ParallelBranchId,
    ) -> Self {
        Self {
            node_id: target_node_id.into(),
            visit: target_visit,
            parallel_group_id: Some(parallel_group_id),
            parallel_branch_id: Some(parallel_branch_id),
        }
    }

    #[must_use]
    pub fn stage_id(&self) -> StageId {
        StageId::new(self.node_id.clone(), self.visit)
    }
}
