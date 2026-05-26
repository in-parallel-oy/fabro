# ACP Granular Session Events Plan

Date: 2026-05-26

## Summary

The ACP backend (introduced 2026-05-11, steering added 2026-05-20) emits only four lifecycle events per agent stage today: `agent.acp.started`, `agent.acp.completed`, `agent.acp.cancelled`, `agent.acp.timed_out`. The subprocess's per-tool, per-message, per-thought, and plan activity is **already on the wire** (ACP delivers it as `SessionUpdate` notifications), but `fabro-acp::run_acp_turn` consumes only the `AgentMessageChunk` variant to assemble the final response text and discards every other variant.

This plan adds **ACP-native granular event types** to Fabro's event taxonomy, paired with the existing `agent.acp.*` family, so downstream consumers (CLI, run-card, web UI, third-party SSE clients, the metafactory Slack activity surface) can render live tool calls / agent narration / thought streams / plans without coupling to the in-process agent runtime's event shapes.

**Deliberate non-goal**: do NOT translate ACP `SessionUpdate` variants into the legacy `agent.tool.started/completed/message/sub.*` events (emitted by `fabro-agent`'s in-process runtime via `tool_execution.rs:274,285`). That would be lossy (`agent_thought_chunk` and `plan` have no legacy home) and couples two protocols by accident. The two runtimes' event surfaces stay distinct; consumers that render both pick the right namespace.

## Goals

- Stream per-tool-call lifecycle (start, status updates, content deltas, terminal status) from ACP-backed stages into `RunEvent`s.
- Stream per-message and per-thought narration as discrete events instead of collapsing into the terminal `agent.acp.completed`'s `stdout`.
- Stream the agent's plan (when ACP emits one) as a structured event.
- Preserve fidelity: every `SessionUpdate` field that downstream consumers might want survives into the persisted event.
- Keep the existing `agent.acp.started/completed/cancelled/timed_out` lifecycle events unchanged — they remain the canonical subprocess-lifecycle signal.

## Non-Goals

- Do not introduce a bidirectional mapping between ACP `SessionUpdate` variants and legacy `agent.tool.*` events. Each runtime emits in its own namespace.
- Do not change `agent.acp.completed.stdout`; the accumulated narrative text stays available for consumers that want a single coalesced blob (e.g. the run summary).
- Do not introduce events for `available_commands_update` — introspection-only, not worth persisting per occurrence. If a consumer needs it later, that's a separate event.
- Do not touch the in-process runtime's `AgentEvent` taxonomy.
- Do not add steering or interactivity to the new events; this is read-only event emission.

## Wire-Level Reality

ACP defines seven `SessionUpdate` variants (from `agent-client-protocol@0.11.1`, `schema/schema.json:2679`):

| ACP variant | Wire shape | Existing handling in `fabro-acp::run_acp_turn` |
|---|---|---|
| `user_message_chunk` | `ContentChunk { content: ContentBlock }` | discarded (`_ => {}`) |
| `agent_message_chunk` | `ContentChunk { content: ContentBlock }` | accumulated into response text |
| `agent_thought_chunk` | `ContentChunk { content: ContentBlock }` | discarded |
| `tool_call` | `ToolCall { tool_call_id, title, kind, status, content?, locations?, raw_input?, raw_output? }` | discarded |
| `tool_call_update` | `ToolCallUpdate { tool_call_id, fields: <partial-ToolCall-shape> }` | discarded |
| `plan` | `Plan { entries: Vec<PlanEntry> }` | discarded |
| `available_commands_update` | `AvailableCommandsUpdate { available_commands }` | discarded |

Everything but `agent_message_chunk` is silently dropped at `fabro-acp/src/session.rs:407-428`.

## New Event Types

Six new `agent.acp.*` events, paired with the existing four:

| Event name | Source variant | Props |
|---|---|---|
| `agent.acp.tool_call` | `tool_call` | `{ tool_call_id, title, kind, status, content?, locations?, raw_input?, raw_output?, visit }` |
| `agent.acp.tool_call_update` | `tool_call_update` | `{ tool_call_id, fields: <ToolCallUpdateFields>, visit }` |
| `agent.acp.message` | `agent_message_chunk` | `{ content: ContentBlock, visit }` |
| `agent.acp.thought` | `agent_thought_chunk` | `{ content: ContentBlock, visit }` |
| `agent.acp.plan` | `plan` | `{ entries: Vec<PlanEntry>, visit }` |
| `agent.acp.user_message` | `user_message_chunk` | `{ content: ContentBlock, visit }` |

`available_commands_update` is intentionally not persisted.

**`visit`** is the one Fabro-side addition (stage-visit counter, not an ACP concept). The callback closure captures the surrounding `stage_scope.visit` at registration time.

**`content`/`tool_call`/`tool_call_update.fields`/`plan.entries`/`ContentBlock`/`PlanEntry`** all re-export the corresponding types from `agent_client_protocol::schema` rather than duplicating their shape — keeps Fabro's event schema locked to ACP's spec and lets `serde` derive forward-compat.

## Implementation Changes

### Sequencing

- **Commit 1** — extend `fabro-acp` with the dispatch callback. No new event types yet; this is purely the plumbing.
- **Commit 2** — add the six new event types in `fabro-types` and the corresponding `EventBody` / `Event` variants. Round-trip tests for serialization. Wire-format tests pinning the canonical envelope.
- **Commit 3** — register the callback in `fabro-workflow::handler::llm::acp::Handler::run`. Translate each `SessionUpdate` variant into the corresponding `Event::AgentAcp*` and emit via `emitter.emit_scoped/2`. Update the existing handler tests to assert the new emissions on a representative ACP fixture (the test crate's `test_support.rs` already has fixtures for several variants).
- **Commit 4** — extend `fabro-cli` events renderer with formatters for the new types. Hide behind a `--verbose` flag for the existing renderers so default CLI output doesn't get noisier.
- **Commit 5** — update `docs-internal/events-strategy.md` and the `docs/public/api-reference/fabro-api.yaml` OpenAPI schema with the new event types.

Each commit leaves the tree in a working state; no commit emits events that aren't yet typed (Commit 1 plumbs only; Commit 2 only adds types; Commit 3 wires emission).

### `lib/crates/fabro-acp/src/session.rs`

- Add `on_session_update: Option<Arc<dyn Fn(&SessionUpdate) + Send + Sync>>` to `AcpRunRequest`.
- In the `tokio::select!` arm at lines 402-428, invoke `on_session_update` (when set) for *every* notification — including `AgentMessageChunk` — before the existing text accumulation. The callback is fire-and-forget; failures are logged via `tracing` but never abort the prompt loop.
- Keep the `AgentMessageChunk` accumulator intact (it still drives the response-text contract).
- Test in `test_support.rs` style: a synthetic ACP server replays each `SessionUpdate` variant; assert the callback fires with each verbatim.

### `lib/crates/fabro-types/src/run_event/`

- Add six new struct types in `run_event/misc.rs` (alongside the existing `AgentAcp*Props`):

  ```rust
  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  pub struct AgentAcpToolCallProps {
      pub tool_call_id: String,
      pub visit:        u32,
      #[serde(flatten)]
      pub call:         agent_client_protocol::schema::ToolCall,
  }
  ```

  Same pattern for `AgentAcpToolCallUpdateProps`, `AgentAcpMessageProps`, `AgentAcpThoughtProps`, `AgentAcpPlanProps`, `AgentAcpUserMessageProps`. The `#[serde(flatten)]` strategy keeps the wire shape identical to ACP's verbatim — consumers can deserialize directly into `agent_client_protocol::schema::ToolCall` if they re-use the ACP crate.

- Add six new variants to the `EventBody` enum (`run_event/mod.rs:213-352`):

  ```rust
  #[serde(rename = "agent.acp.tool_call")]
  AgentAcpToolCall(AgentAcpToolCallProps),
  #[serde(rename = "agent.acp.tool_call_update")]
  AgentAcpToolCallUpdate(AgentAcpToolCallUpdateProps),
  #[serde(rename = "agent.acp.message")]
  AgentAcpMessage(AgentAcpMessageProps),
  #[serde(rename = "agent.acp.thought")]
  AgentAcpThought(AgentAcpThoughtProps),
  #[serde(rename = "agent.acp.plan")]
  AgentAcpPlan(AgentAcpPlanProps),
  #[serde(rename = "agent.acp.user_message")]
  AgentAcpUserMessage(AgentAcpUserMessageProps),
  ```

- Extend `EventBody::event_name()` (`mod.rs:575-578`) with arms for each.

- Wire-shape characterization tests in `run_event/tests.rs`: round-trip each new variant through `RunEvent::to_value` → `from_value` and assert byte-for-byte equality.

### `lib/crates/fabro-workflow/src/event/events.rs`

- Add the six matching variants to `Event::AgentAcp*` (line 689+):

  ```rust
  AgentAcpToolCall {
      node_id:      Node,
      tool_call_id: String,
      call:         ToolCall,
      visit:        u32,
  },
  // ...
  ```

- Extend `event_name()` and `to_run_event` conversions accordingly.

### `lib/crates/fabro-workflow/src/handler/llm/acp.rs`

- Around the existing `on_activity` construction (lines 78-81), build an `on_session_update` Arc that captures `Arc::clone(&emitter)` and `stage_scope.clone()`:

  ```rust
  let on_session_update = {
      let emitter = Arc::clone(emitter);
      let stage_scope = stage_scope.clone();
      let node_id = node.id.clone();
      Arc::new(move |update: &SessionUpdate| {
          let event = match update {
              SessionUpdate::ToolCall(call) => Event::AgentAcpToolCall {
                  node_id:      node_id.clone(),
                  tool_call_id: call.tool_call_id.clone(),
                  call:         call.clone(),
                  visit:        stage_scope.visit,
              },
              SessionUpdate::ToolCallUpdate(update) => Event::AgentAcpToolCallUpdate { ... },
              SessionUpdate::AgentMessageChunk(chunk) => Event::AgentAcpMessage { ... },
              SessionUpdate::AgentThoughtChunk(chunk) => Event::AgentAcpThought { ... },
              SessionUpdate::Plan(plan) => Event::AgentAcpPlan { ... },
              SessionUpdate::UserMessageChunk(chunk) => Event::AgentAcpUserMessage { ... },
              SessionUpdate::AvailableCommandsUpdate(_) => return,
          };
          emitter.emit_scoped(&event, &stage_scope);
      }) as Arc<dyn Fn(&SessionUpdate) + Send + Sync>
  };
  ```

- Pass `on_session_update: Some(on_session_update)` into `AcpRunRequest`.

- The existing `AgentAcpStarted` / `AgentAcpCompleted` / `AgentAcpCancelled` / `AgentAcpTimedOut` emissions stay exactly as today; they remain the canonical subprocess lifecycle signal. The new events stream *during* the gap between started and completed.

### `lib/crates/fabro-cli/src/events.rs` (and renderer modules)

- Default rendering for the new event types: one-line summary per event (e.g. `tool_call build_artifact (in_progress)` / `message: <truncated>` / `plan: 3 entries`).
- Verbose rendering: pretty-print the full `ToolCall` / `ContentBlock` / `PlanEntry`.
- The existing `agent.acp.completed.stdout` rendering stays — useful for run-summary commands.

### `docs/public/api-reference/fabro-api.yaml`

- Add the six new event schemas under `components.schemas` mirroring the Rust types.
- Add them to the SSE event-type enum at `paths./runs/{id}/events.get.responses.200.content.text/event-stream`.
- Bump the OpenAPI `info.version`.

## Test Plan

- `fabro-acp`: a `test_support.rs` fixture per variant (callback fires; payload arrives verbatim; ordering preserved relative to other notifications). The existing `agent_message_chunk` accumulator behavior is not regressed.
- `fabro-types`: round-trip serialization for each new `EventBody` variant. Test that `EventBody::event_name()` returns the documented string.
- `fabro-workflow`: handler test with a synthetic ACP fixture emitting all six variant types — assert the emitter receives the correct `Event::AgentAcp*` for each, with the right `node_id`, `visit`, `tool_call_id`, etc.
- `fabro-server`: SSE replay test confirming the new event types pass through the existing `/runs/{id}/events` stream unchanged.
- `fabro-cli`: snapshot tests for the new renderer arms.
- End-to-end smoke: dispatch a `codex-acp`-backed workflow against a non-trivial repo (e.g. `narayan-core` / `assist` workflow) and confirm the new events appear in `fabro events <run_id> --json`.

## Migration / Compatibility

- **Wire-format additive.** No existing event type changes. Existing consumers (run store, SSE, CLI, JSONL sinks) ignore unknown event types — verified by the `EventBody::Unknown` fallback at `fabro-types/src/run_event/mod.rs:373`.
- **No breaking API changes.** `fabro-acp`'s `AcpRunRequest` gains an optional field; existing callers compile unchanged.
- **`agent.acp.completed.stdout`** continues to carry the accumulated agent text. New consumers that prefer `agent.acp.message`-stream rendering can use that; legacy consumers (e.g. run-summary commands) keep the coalesced blob.
- **Storage**: persisted events go through the standard `EventBody` serialization path; no schema migration needed.

## Risks

- **Volume.** A long agent run with hundreds of tool calls + thousands of message chunks could 10×–100× the per-run event count. Mitigation: the run store, SSE, and persistence paths are already sized for high-throughput agent events (in-process runtime emits at similar granularity); coalescing at the persistence boundary is a separate optimization if needed.
- **`tool_call_update` semantics.** ACP allows multiple updates per tool call (status transitions, content deltas, terminal status). Each update becomes a separate `agent.acp.tool_call_update` event. Consumers responsible for in-place rendering (e.g. metafactory's Slack `task_update` cards) key off `tool_call_id` and apply each update in arrival order — same pattern the in-process renderer already follows.
- **`ContentBlock` is a tagged union.** Re-exporting from `agent-client-protocol` preserves the variant taxonomy across versions. If ACP adds a new `ContentBlock` variant, downstream deserializers either tolerate via `#[serde(other)]` or fail closed; the choice belongs to each consumer.
- **`available_commands_update` omission.** If a consumer later needs persistent visibility into command-set changes, add `agent.acp.commands_updated` as a separate event in a follow-up.

## Open Questions

- **Should `agent.acp.tool_call`'s `raw_input` be redacted at emission time?** The in-process runtime emits raw arguments in `agent.tool.started`; the redaction set is a consumer-side concern there. Same default here: emit verbatim, let consumers redact. Document this clearly.
- **Whether to deduplicate identical `tool_call_update`s.** ACP servers can re-send the same update; the bridge currently emits each verbatim. Defer until observed in practice.
- **Renderer for `agent.acp.plan` in the CLI**. Probably worth a structured `Plan rendered:` block, but the design can wait until a real ACP backend that emits plans is in regular use (today both `codex-acp` and `claude-agent-acp` emit plans sporadically).
