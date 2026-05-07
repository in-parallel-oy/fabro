import { useEffect, useMemo, useState } from "react";
import { useParams } from "react-router";
import { XMarkIcon } from "@heroicons/react/24/outline";

import { StageSidebar } from "../components/stage-sidebar";
import type { Stage } from "../components/stage-sidebar";
import { EmptyState } from "../components/state";
import { useRun, useRunStageEvents, useRunStages } from "../lib/queries";
import { STAGE_ACTIVITY_EVENT_TYPES, type StageActivityEventType } from "../lib/run-events";
import { mapRunStagesToSidebarStages } from "../lib/stage-sidebar";
import { getNumber, getString, type UnknownRecord } from "../lib/unknown";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

export const handle = { wide: true, fullHeight: true };

type TurnType =
  | { kind: "system"; ts: string; content: string }
  | { kind: "assistant"; ts: string; content: string }
  | { kind: "tool"; ts: string; toolName: string; input: string; result: string; isError: boolean }
  | { kind: "command"; ts: string; script: string; running: boolean; exitCode: number | null; durationMs: number };

const STAGE_ACTIVITY_EVENT_SET = new Set<string>(STAGE_ACTIVITY_EVENT_TYPES);

function assertNever(value: never): never {
  throw new Error(`Unhandled stage activity event type: ${value}`);
}

function activityEventStageId(event: EventEnvelope): string | undefined {
  if (typeof event.stage_id === "string") return event.stage_id;
  if (typeof event.node_id === "string") return event.node_id;
  return getString(event.properties ?? {}, "node_id");
}

interface PendingTool {
  ts: string;
  toolName: string;
  input: string;
}

interface PendingCommand {
  ts: string;
  script: string;
}

export function eventsToActivity(events: EventEnvelope[], stageId: string): TurnType[] {
  const turns: TurnType[] = [];
  const pendingTools = new Map<string, PendingTool>();
  let pendingCommand: PendingCommand | undefined;

  for (const e of events) {
    const eventName = e.event;
    if (
      activityEventStageId(e) !== stageId ||
      !eventName ||
      !STAGE_ACTIVITY_EVENT_SET.has(eventName)
    ) {
      continue;
    }
    const eventType = eventName as StageActivityEventType;
    const props: UnknownRecord = e.properties ?? {};
    switch (eventType) {
      case "stage.prompt":
        turns.push({ kind: "system", ts: e.ts, content: getString(props, "text") ?? e.text ?? "" });
        break;
      case "agent.message": {
        const msg = getString(props, "text") ?? e.text ?? "";
        if (msg) turns.push({ kind: "assistant", ts: e.ts, content: msg });
        break;
      }
      case "agent.tool.started": {
        const callId = getString(props, "tool_call_id") ?? e.tool_call_id ?? "";
        const args = props.arguments ?? e.arguments;
        pendingTools.set(callId, {
          ts: e.ts,
          toolName: getString(props, "tool_name") ?? e.tool_name ?? "",
          input: typeof args === "string" ? args : JSON.stringify(args ?? ""),
        });
        break;
      }
      case "agent.tool.completed": {
        const callId = getString(props, "tool_call_id") ?? e.tool_call_id ?? "";
        const started = pendingTools.get(callId);
        pendingTools.delete(callId);
        const output = props.output ?? e.output ?? "";
        const result = typeof output === "string" ? output : JSON.stringify(output, null, 2);
        turns.push({
          kind: "tool",
          ts: started?.ts ?? e.ts,
          toolName: started?.toolName ?? getString(props, "tool_name") ?? e.tool_name ?? "",
          input: started?.input ?? "",
          result,
          isError: (props.is_error ?? e.is_error) === true,
        });
        break;
      }
      case "command.started": {
        pendingCommand = {
          ts: e.ts,
          script: getString(props, "script") ?? "",
        };
        break;
      }
      case "command.completed": {
        turns.push({
          kind: "command",
          ts: pendingCommand?.ts ?? e.ts,
          script: pendingCommand?.script ?? "",
          running: false,
          exitCode: getNumber(props, "exit_code") ?? null,
          durationMs: getNumber(props, "duration_ms") ?? 0,
        });
        pendingCommand = undefined;
        break;
      }
      default:
        assertNever(eventType);
    }
  }

  if (pendingCommand) {
    turns.push({
      kind: "command",
      ts: pendingCommand.ts,
      script: pendingCommand.script,
      running: true,
      exitCode: null,
      durationMs: 0,
    });
  }

  return turns;
}

function turnLabel(turn: TurnType): string {
  switch (turn.kind) {
    case "system":
      return "System";
    case "assistant":
      return "Agent";
    case "tool":
      return "Tool";
    case "command":
      return "Command";
  }
}

function turnTone(turn: TurnType): string {
  if (turn.kind === "tool" && turn.isError) {
    return "bg-coral/15 text-coral";
  }
  switch (turn.kind) {
    case "system":
      return "bg-amber/15 text-amber";
    case "assistant":
      return "bg-teal-500/15 text-teal-500";
    case "tool":
    case "command":
      return "bg-mint/15 text-mint";
  }
}

const SUMMARY_MAX_CHARS = 80;

function oneLine(text: string): string {
  const collapsed = text.replace(/\s+/g, " ").trim();
  if (collapsed.length <= SUMMARY_MAX_CHARS) return collapsed;
  return `${collapsed.slice(0, SUMMARY_MAX_CHARS - 1)}…`;
}

const TOOL_NAME_DISPLAY: Record<string, string> = {
  read_file: "Read",
  write_file: "Write",
  edit_file: "Edit",
  shell: "Bash",
  grep: "Grep",
  glob: "Glob",
  read_many_files: "Read Many",
  list_dir: "List Dir",
  web_search: "Web Search",
  web_fetch: "Web Fetch",
};

export function humanizeToolName(raw: string): string {
  if (!raw) return "tool";
  if (TOOL_NAME_DISPLAY[raw]) return TOOL_NAME_DISPLAY[raw];
  // MCP tools are namespaced like `mcp__<server>__<tool>`; display the trailing segment.
  const lastSegment = raw.split("__").pop() ?? raw;
  return lastSegment
    .split(/[_-]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");
}

export function turnSummary(turn: TurnType): string {
  switch (turn.kind) {
    case "system":
    case "assistant":
      return oneLine(turn.content);
    case "tool":
      return humanizeToolName(turn.toolName);
    case "command":
      return oneLine(turn.script) || (turn.running ? "running…" : "");
  }
}

export function formatElapsed(eventTs: string, runStart: string | undefined): string {
  if (!runStart) return "";
  const startMs = Date.parse(runStart);
  const eventMs = Date.parse(eventTs);
  if (Number.isNaN(startMs) || Number.isNaN(eventMs)) return "";
  const delta = Math.max(0, Math.floor((eventMs - startMs) / 1000));
  const hours = Math.floor(delta / 3600);
  const minutes = Math.floor((delta % 3600) / 60);
  const seconds = delta % 60;
  return `${hours}:${minutes.toString().padStart(2, "0")}:${seconds.toString().padStart(2, "0")}`;
}

function EventRow({
  turn,
  runStart,
  selected,
  onSelect,
}: {
  turn: TurnType;
  runStart: string | undefined;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={`grid w-full grid-cols-[5rem_1fr_auto] items-center gap-4 rounded-md px-2 py-1.5 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500 ${
        selected ? "bg-overlay" : ""
      }`}
    >
      <span
        className={`inline-flex w-fit items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider ${turnTone(turn)}`}
      >
        {turnLabel(turn)}
      </span>
      <span className="min-w-0 truncate text-sm text-fg-3">
        {turnSummary(turn)}
      </span>
      <span className="font-mono text-xs tabular-nums text-fg-muted">
        {formatElapsed(turn.ts, runStart)}
      </span>
    </button>
  );
}

function DetailField({
  label,
  children,
  mono = false,
}: {
  label: string;
  children: React.ReactNode;
  mono?: boolean;
}) {
  return (
    <div>
      <div className="mb-1 text-xs font-medium uppercase tracking-wider text-fg-muted">
        {label}
      </div>
      <div className={mono ? "font-mono text-sm text-fg-3" : "text-sm text-fg-3"}>
        {children}
      </div>
    </div>
  );
}

function CodeBlock({ children }: { children: string }) {
  return (
    <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
      {children || <span className="text-fg-muted">empty</span>}
    </pre>
  );
}

function EventDetails({ turn, runStart }: { turn: TurnType; runStart: string | undefined }) {
  const elapsed = formatElapsed(turn.ts, runStart);
  const absolute = (() => {
    const ms = Date.parse(turn.ts);
    if (Number.isNaN(ms)) return turn.ts;
    return new Date(ms).toLocaleString();
  })();

  return (
    <div className="space-y-5">
      <DetailField label="When" mono>
        {elapsed ? `${elapsed} · ${absolute}` : absolute}
      </DetailField>

      {(turn.kind === "system" || turn.kind === "assistant") && (
        <DetailField label="Content">
          <CodeBlock>{turn.content}</CodeBlock>
        </DetailField>
      )}

      {turn.kind === "tool" && (
        <>
          <DetailField label="Tool" mono>
            {humanizeToolName(turn.toolName)}{" "}
            <span className="text-fg-muted">({turn.toolName})</span>
          </DetailField>
          <DetailField label="Input">
            <CodeBlock>{turn.input}</CodeBlock>
          </DetailField>
          <DetailField label={turn.isError ? "Error" : "Result"}>
            <CodeBlock>{turn.result}</CodeBlock>
          </DetailField>
        </>
      )}

      {turn.kind === "command" && (
        <>
          <DetailField label="Status" mono>
            {turn.running
              ? "Running…"
              : `exit ${turn.exitCode ?? "?"}${
                  turn.durationMs
                    ? ` · ${
                        turn.durationMs < 1000
                          ? `${turn.durationMs}ms`
                          : `${(turn.durationMs / 1000).toFixed(1)}s`
                      }`
                    : ""
                }`}
          </DetailField>
          <DetailField label="Script">
            <CodeBlock>{turn.script}</CodeBlock>
          </DetailField>
        </>
      )}
    </div>
  );
}

function EventDetailsPanel({
  turn,
  runStart,
  onClose,
}: {
  turn: TurnType | null;
  runStart: string | undefined;
  onClose: () => void;
}) {
  useEffect(() => {
    if (!turn) return;
    function handleKey(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [turn, onClose]);

  return (
    <div
      className={`relative shrink-0 self-stretch overflow-hidden transition-[width] duration-200 ease-out ${
        turn ? "w-[28rem]" : "w-0"
      }`}
      aria-hidden={turn ? undefined : true}
    >
      <div className="absolute inset-y-0 right-0 flex w-[28rem] flex-col border-l border-line bg-panel">
        <div className="flex shrink-0 items-center justify-between border-b border-line px-5 py-3">
          <h2 className="text-sm font-medium text-fg">
            {turn ? `${turnLabel(turn)} event` : ""}
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close details"
            className="rounded-md p-1 text-fg-muted transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
          >
            <XMarkIcon className="size-5" />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-4">
          {turn ? <EventDetails turn={turn} runStart={runStart} /> : null}
        </div>
      </div>
    </div>
  );
}

export default function RunStages() {
  const { id, stageId } = useParams();
  const runQuery = useRun(id);
  const stagesQuery = useRunStages(id);
  const stages = useMemo(
    () => mapRunStagesToSidebarStages(stagesQuery.data),
    [stagesQuery.data],
  );

  const selectedStage = stages.find((s: Stage) => s.id === stageId) ?? stages[0];
  const selectedStageId = selectedStage?.id;
  const stageEventsQuery = useRunStageEvents(id, selectedStageId);
  const turns = useMemo(
    () =>
      selectedStageId
        ? eventsToActivity(stageEventsQuery.data ?? [], selectedStageId)
        : [],
    [stageEventsQuery.data, selectedStageId],
  );

  const [openIndex, setOpenIndex] = useState<number | null>(null);
  useEffect(() => {
    setOpenIndex(null);
  }, [selectedStageId]);
  const openTurn = openIndex != null ? turns[openIndex] ?? null : null;

  if (!id || !stages.length) {
    return (
      <div className="py-12">
        <EmptyState
          title="No stages yet"
          description="Stages will appear here once the run begins executing."
        />
      </div>
    );
  }

  const runStart = runQuery.data?.created_at;

  return (
    <div className="-mt-6 flex h-full min-h-0">
      <div className="shrink-0 pb-6 pr-3 pt-6">
        <StageSidebar stages={stages} runId={id} selectedStageId={selectedStage.id} />
      </div>

      <div className="relative w-px shrink-0">
        <div
          aria-hidden="true"
          className="absolute inset-x-0 top-0 -bottom-6 bg-line"
        />
      </div>

      <div className="min-w-0 flex-1 overflow-y-auto pb-6 pl-3 pt-6">
        {turns.map((turn: TurnType, i: number) => (
          <EventRow
            key={`turn-${i}`}
            turn={turn}
            runStart={runStart}
            selected={openIndex === i}
            onSelect={() => setOpenIndex(i)}
          />
        ))}
      </div>

      <EventDetailsPanel
        turn={openTurn}
        runStart={runStart}
        onClose={() => setOpenIndex(null)}
      />
    </div>
  );
}
