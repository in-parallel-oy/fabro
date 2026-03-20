---
date: 2026-03-20
topic: acp-agent-configuration
---

# ACP Agent Configuration

## What We're Building
We are refactoring how the Agent Client Protocol (ACP) is configured in Fabro. Currently, `acp` is treated as a top-level "backend" alongside `api` and `cli`. This creates a conceptual mismatch, as ACP is a protocol for communicating with an external agent, not a backend execution environment. We will replace the `[acp]` configuration block with a unified `[agent]` block that specifies the agent's protocol type and command.

## Why This Approach
We considered several approaches, including an `[agents]` map (similar to `[mcp_servers]`) and attaching the protocol to the model configuration. We chose the `[agent]` block approach because it provides the best balance of simplicity and elegance without requiring a notable refactoring. It unifies the concept of the "agent" running the task while keeping the internal backend routing logic mostly intact.

## Key Decisions
- **Replace `[acp]` with `[agent]`**: The configuration will use an `[agent]` block instead of `[acp]`.
- **Agent Protocol Type**: The `[agent]` block will include a `type` field (e.g., `acp`, `api`, `cli`) to determine the protocol.
- **Agent Command**: The `[agent]` block will include a `command` field for external agents (like ACP or CLI).
- **Minimal Internal Refactoring**: The internal `BackendRouter` will read `config.agent.type` instead of looking for `config.acp`, preserving the existing `AcpCodergenBackend` implementation.

## Resolved Questions
- **Default Agent Type**: If the `[agent]` block is omitted from the configuration, the default agent type will be `api` (the internal agent).
- **Workflow Graph Override**: Since this is a WIP branch and backward compatibility is not required, we will rename the `backend` attribute to `agent` in the workflow graph. Nodes will specify `agent: acp;` or `agent: cli;` instead of `backend: acp;`.

## Next Steps
→ `/ce:plan` for implementation details
