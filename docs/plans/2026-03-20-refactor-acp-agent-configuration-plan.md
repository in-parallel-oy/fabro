---
title: refactor: ACP Agent Configuration
type: refactor
status: active
date: 2026-03-20
origin: docs/brainstorms/2026-03-20-acp-agent-configuration-brainstorm.md
---

# refactor: ACP Agent Configuration

## Enhancement Summary

**Deepened on:** 2026-03-20
**Sections enhanced:** 3
**Research agents used:** architecture-strategist, code-simplicity-reviewer

### Key Improvements
1. **BackendRouter Integration**: Identified a critical flaw in the original plan regarding how `BackendRouter` is instantiated. `run.rs` currently bypasses the router for ACP. The plan now explicitly requires updating `run.rs` to always instantiate `BackendRouter` with all available backends.
2. **Rust Keyword Handling**: Added technical consideration for handling the `type` keyword in Rust structs using `#[serde(rename = "type")]`.
3. **Node Attribute Accessor**: Explicitly added the requirement to rename `Node::backend()` to `Node::agent()` in `fabro-graphviz`.

### New Considerations Discovered
- **Simplicity Alternative**: The `code-simplicity-reviewer` suggested dropping the `type` field entirely and inferring the protocol from the presence of a `command`. While we are sticking with the explicit `type` field as decided in the brainstorm, this is documented as an alternative approach.
- **Validation**: Added a requirement to validate that a `command` is provided when `type = "acp"`.

---

## Overview

We are refactoring how the Agent Client Protocol (ACP) is configured in Fabro. Currently, `acp` is treated as a top-level "backend" alongside `api` and `cli`. This creates a conceptual mismatch, as ACP is a protocol for communicating with an external agent, not a backend execution environment. We will replace the `[acp]` configuration block with a unified `[agent]` block that specifies the agent's protocol type and command.

## Problem Statement / Motivation

Treating `acp` as a backend creates configuration friction and conceptual confusion. By unifying agent configuration under an `[agent]` block, we clarify that ACP is a protocol used by an agent, not a backend itself. This approach provides the best balance of simplicity and elegance without requiring a notable refactoring, keeping the internal backend routing logic mostly intact.

## Proposed Solution

1.  **Replace `[acp]` with `[agent]`**: Update the configuration structs (`ProjectConfig`, `WorkflowRunConfig`, `RunDefaults`) to use an `[agent]` block instead of `[acp]`.
2.  **Agent Protocol Type**: The `[agent]` block will include a `type` field (e.g., `acp`, `api`, `cli`) to determine the protocol. If omitted, it defaults to `api`.
3.  **Agent Command**: The `[agent]` block will include a `command` field for external agents (like ACP or CLI).
4.  **Workflow Graph Override**: Rename the `backend` attribute to `agent` in the workflow graph. Nodes will specify `agent: acp;` or `agent: cli;` instead of `backend: acp;`.
5.  **BackendRouter Integration**: Update `lib/crates/fabro-cli/src/commands/run.rs` to *always* instantiate `BackendRouter`, passing it the `api_backend`, `cli_backend`, and an `Option<AcpCodergenBackend>`. The router will use `config.agent.type` as the default routing destination when a node lacks an explicit `agent` attribute.

## Technical Considerations

-   **Configuration Merging**: Ensure the new `[agent]` block is properly merged in `RunDefaults::merge_overlay`, following the existing pattern for `[llm]` and `[sandbox]`.
-   **OpenAPI Spec**: Update `docs/api-reference/fabro-api.yaml` to reflect the new `AgentConfig` structure and remove `AcpConfig`. This will automatically regenerate the Rust types and TypeScript client.
-   **Rust Keyword Handling**: Since `type` is a reserved keyword in Rust, use `#[serde(rename = "type")]` in the `AgentConfig` struct definition.
-   **Validation**: Add validation in `run.rs` or `BackendRouter` to return a clear error if `type = "acp"` is requested but no `command` is configured.
-   **Backward Compatibility**: Since this is a WIP branch (`prototype-acp`), backward compatibility for the `backend` attribute in workflow graphs is not required.

## Alternative Approaches Considered

-   **Infer Protocol from Command**: The `code-simplicity-reviewer` suggested removing the `type` field entirely and inferring the protocol (if `command` is present, use ACP; otherwise, use API). We rejected this in favor of explicit configuration (`type` field) to allow for future agent types (like `cli`) that might also require a command, and to align with the brainstorm decision.

## System-Wide Impact

-   **Interaction graph**: The configuration parser will read the new `[agent]` block. `run.rs` will instantiate `BackendRouter` with all backends. The `BackendRouter` will inspect the `agent` attribute on nodes and the `config.agent.type` to route to the correct backend (`AcpCodergenBackend`, `AgentApiBackend`, or `AgentCliBackend`).
-   **API surface parity**: The OpenAPI spec, Rust configuration structs, and TypeScript client will all be updated to use the new `AgentConfig`.

## Acceptance Criteria

-   [ ] `docs/api-reference/fabro-api.yaml` is updated to replace `AcpConfig` with `AgentConfig` (containing `type` and `command`).
-   [ ] `lib/crates/fabro-config/src/run.rs` and `project.rs` are updated to use `AgentConfig` instead of `AcpConfig`, using `#[serde(rename = "type")]`.
-   [ ] `RunDefaults::merge_overlay` correctly merges the `[agent]` block.
-   [ ] `lib/crates/fabro-cli/src/commands/run.rs` is updated to always instantiate `BackendRouter` with all available backends and the default agent type.
-   [ ] `BackendRouter` in `lib/crates/fabro-workflows/src/backend/cli.rs` uses the default agent type and the `agent` node attribute for routing.
-   [ ] `Node::backend()` in `lib/crates/fabro-graphviz/src/graph/types.rs` is renamed to `Node::agent()` and looks for the `"agent"` attribute.
-   [ ] The default agent type is `api` when the `[agent]` block is omitted.
-   [ ] Validation fails gracefully if `type = "acp"` but no `command` is provided.
-   [ ] All tests pass, including `cargo test -p fabro-api` and `cargo test -p fabro-workflows`.

## Success Metrics

-   Workflows can be successfully configured and run using the new `[agent]` block with `type = "acp"`.
-   The codebase is cleaner and the conceptual model of agents vs. backends is clearer.

## Dependencies & Risks

-   **Risk**: Breaking existing tests that rely on the `backend` attribute or `[acp]` config.
-   **Mitigation**: Update all relevant tests in `fabro-workflows` and `fabro-config` to use the new configuration structure.

## Sources & References

-   **Origin brainstorm:** [docs/brainstorms/2026-03-20-acp-agent-configuration-brainstorm.md](docs/brainstorms/2026-03-20-acp-agent-configuration-brainstorm.md)
    -   Key decisions carried forward: Replace `[acp]` with `[agent]`, include `type` and `command` fields, default to `api`, rename `backend` attribute to `agent` in workflow graphs, minimal internal refactoring.
-   Configuration merging pattern: `lib/crates/fabro-config/src/run.rs:174` (`RunDefaults::merge_overlay`)
-   Backend routing logic: `lib/crates/fabro-workflows/src/backend/cli.rs:722` (`BackendRouter`)
-   Run command instantiation: `lib/crates/fabro-cli/src/commands/run.rs`