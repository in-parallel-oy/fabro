import { useEffect, useMemo, useState } from "react";
import { useParams } from "react-router";
import {
  Listbox,
  ListboxButton,
  ListboxOption,
  ListboxOptions,
} from "@headlessui/react";
import { XMarkIcon } from "@heroicons/react/24/outline";
import {
  CheckIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  ChevronUpDownIcon,
  CpuChipIcon,
  FunnelIcon,
  MagnifyingGlassIcon,
} from "@heroicons/react/16/solid";
import { CircleStackIcon, ClockIcon } from "@heroicons/react/20/solid";
import { Marked } from "marked";

import { StageSidebar } from "../components/stage-sidebar";
import type { Stage } from "../components/stage-sidebar";
import { EmptyState } from "../components/state";
import { Tooltip } from "../components/ui";
import { formatAbsoluteTs, formatBytes } from "../lib/format";
import {
  useRun,
  useRunStageEvents,
  useRunStageLog,
  useRunStages,
} from "../lib/queries";
import { STAGE_ACTIVITY_EVENT_TYPES, type StageActivityEventType } from "../lib/run-events";
import { mapRunStagesToSidebarStages } from "../lib/stage-sidebar";
import { getNumber, getString, type UnknownRecord } from "../lib/unknown";
import type { EventEnvelope } from "@qltysh/fabro-api-client";

export const handle = { wide: true, fullHeight: true };

type TurnType =
  | { kind: "system"; ts: string; content: string }
  | { kind: "assistant"; ts: string; content: string; inputTokens: number; outputTokens: number }
  | { kind: "tool"; ts: string; toolName: string; input: string; result: string; isError: boolean; durationMs: number }
  | {
      kind: "command";
      ts: string;
      script: string;
      running: boolean;
      exitCode: number | null;
      durationMs: number;
      outputBytes: number;
    };

type CommandTurn = Extract<TurnType, { kind: "command" }>;
type StageKind = "agent" | "command";

type PanelSelection =
  | { kind: "single"; turnIndex: number }
  | { kind: "group"; childTurnIndices: number[] };

const STAGE_ACTIVITY_EVENT_SET = new Set<string>(STAGE_ACTIVITY_EVENT_TYPES);

const EVENT_KINDS = ["system", "assistant", "tool", "command"] as const;
type EventKind = (typeof EVENT_KINDS)[number];

const EVENT_KIND_LABEL: Record<EventKind, string> = {
  system: "System",
  assistant: "Agent",
  tool: "Tool",
  command: "Command",
};

const DEBUG_CATEGORY_TONE: Record<string, string> = {
  agent: "bg-teal-500/15 text-teal-500",
  command: "bg-mint/15 text-mint",
  interview: "bg-coral/15 text-coral",
  run: "bg-overlay-strong text-fg-2",
  stage: "bg-amber/15 text-amber",
  tool: "bg-mint/15 text-mint",
};

function debugCategory(eventName: string): string {
  const dot = eventName.indexOf(".");
  return dot < 0 ? eventName : eventName.slice(0, dot);
}

function debugCategoryLabel(category: string): string {
  if (!category) return "Other";
  return category.charAt(0).toUpperCase() + category.slice(1);
}

function debugCategoryTone(category: string): string {
  return DEBUG_CATEGORY_TONE[category] ?? "bg-overlay text-fg-muted";
}

const EVENTS_TABS = ["transcript", "debug"] as const;
type EventsTab = (typeof EVENTS_TABS)[number];

function eventsTabLabel(tab: EventsTab, stageKind: StageKind): string {
  if (tab === "debug") return "Debug";
  return stageKind === "command" ? "Logs" : "Transcript";
}

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
        if (msg) {
          const billing = (props.billing ?? {}) as UnknownRecord;
          turns.push({
            kind: "assistant",
            ts: e.ts,
            content: msg,
            inputTokens: getNumber(billing, "input_tokens") ?? 0,
            outputTokens: getNumber(billing, "output_tokens") ?? 0,
          });
        }
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
          durationMs: durationBetween(started?.ts, e.ts),
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
          outputBytes: getNumber(props, "output_bytes") ?? 0,
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
      outputBytes: 0,
    });
  }

  return turns;
}

export function turnsToStageKind(turns: TurnType[]): StageKind {
  let hasCommand = false;
  for (const t of turns) {
    if (t.kind === "assistant" || t.kind === "tool") return "agent";
    if (t.kind === "command") hasCommand = true;
  }
  return hasCommand ? "command" : "agent";
}

type ToolTurn = Extract<TurnType, { kind: "tool" }>;

export type DisplayItem =
  | { kind: "single"; turn: TurnType; turnIndex: number }
  | {
      kind: "group";
      toolName: string;
      ts: string;
      durationMs: number;
      children: { turn: ToolTurn; turnIndex: number }[];
    };

export function groupConsecutiveTools(
  filtered: { turn: TurnType; index: number }[],
): DisplayItem[] {
  const out: DisplayItem[] = [];
  let buf: { turn: ToolTurn; turnIndex: number }[] = [];

  function flush() {
    if (buf.length === 0) return;
    if (buf.length === 1) {
      out.push({ kind: "single", turn: buf[0].turn, turnIndex: buf[0].turnIndex });
    } else {
      const first = buf[0].turn;
      const totalMs = buf.reduce((sum, b) => sum + b.turn.durationMs, 0);
      out.push({
        kind: "group",
        toolName: first.toolName,
        ts: first.ts,
        durationMs: totalMs,
        children: buf,
      });
    }
    buf = [];
  }

  for (const { turn, index } of filtered) {
    const groupable = turn.kind === "tool" && !turn.isError;
    if (groupable && (buf.length === 0 || buf[0].turn.toolName === turn.toolName)) {
      buf.push({ turn, turnIndex: index });
      continue;
    }
    flush();
    if (groupable) {
      buf.push({ turn, turnIndex: index });
    } else {
      out.push({ kind: "single", turn, turnIndex: index });
    }
  }
  flush();
  return out;
}

const STAGE_MODEL_EVENT_NAMES = new Set([
  "stage.prompt",
  "agent.session.activated",
  "agent.cli.started",
]);

export function extractStageModel(
  events: EventEnvelope[],
  stageId: string,
): string | null {
  let model: string | null = null;
  for (const e of events) {
    if (activityEventStageId(e) !== stageId) continue;
    if (!e.event || !STAGE_MODEL_EVENT_NAMES.has(e.event)) continue;
    const candidate = getString(e.properties ?? {}, "model");
    if (candidate) model = candidate;
  }
  return model;
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

const SAFE_HTTP_URL_RE = /^https?:\/\//i;
const SAFE_MAILTO_URL_RE = /^mailto:/i;

function isSafeMarkdownHref(href: string): boolean {
  return (
    SAFE_HTTP_URL_RE.test(href) ||
    SAFE_MAILTO_URL_RE.test(href) ||
    href.startsWith("#") ||
    (href.startsWith("/") && !href.startsWith("//"))
  );
}

const markedSafe = new Marked();
markedSafe.use({
  async: false,
  walkTokens(token) {
    if (
      (token.type === "link" || token.type === "image") &&
      typeof token.href === "string" &&
      !isSafeMarkdownHref(token.href)
    ) {
      token.href = "";
    }
  },
  renderer: {
    html() {
      return "";
    },
  },
});

function Markdown({ content }: { content: string }) {
  const html = useMemo(
    () => markedSafe.parse(content, { async: false }) as string,
    [content],
  );
  return (
    <div
      className="prose prose-sm max-w-none text-fg-3 prose-headings:text-fg-2 prose-strong:text-fg-2 prose-code:rounded prose-code:bg-overlay-strong prose-code:px-1 prose-code:py-0.5 prose-code:text-[0.8em] prose-code:font-mono prose-code:text-fg-3 prose-code:before:content-none prose-code:after:content-none prose-pre:bg-overlay-strong prose-pre:text-fg-3 prose-a:text-teal-500"
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
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

function durationBetween(startTs: string | undefined, endTs: string): number {
  if (!startTs) return 0;
  const startMs = Date.parse(startTs);
  const endMs = Date.parse(endTs);
  if (Number.isNaN(startMs) || Number.isNaN(endMs)) return 0;
  return Math.max(0, endMs - startMs);
}

function formatDurationMs(ms: number): string {
  return `${(ms / 1000).toFixed(1)}s`;
}

function formatTokenCount(n: number): string {
  if (n < 1000) return `${n}`;
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

export function turnMetric(turn: TurnType): string | null {
  switch (turn.kind) {
    case "assistant": {
      if (turn.inputTokens === 0 && turn.outputTokens === 0) return null;
      return `${formatTokenCount(turn.inputTokens)} / ${formatTokenCount(turn.outputTokens)}`;
    }
    case "tool":
    case "command":
      return turn.durationMs > 0 ? formatDurationMs(turn.durationMs) : null;
    case "system":
      return null;
  }
}

export function searchableText(turn: TurnType): string {
  switch (turn.kind) {
    case "system":
    case "assistant":
      return turn.content;
    case "tool":
      return `${humanizeToolName(turn.toolName)} ${turn.toolName} ${turn.input} ${turn.result}`;
    case "command":
      return turn.script;
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
  const metric = turnMetric(turn);
  const MetricIcon = metric == null ? null : turn.kind === "assistant" ? CircleStackIcon : ClockIcon;
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={`grid w-full grid-cols-[5rem_1fr_auto_auto] items-center gap-4 px-5 py-2.5 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500 ${
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
      <span className="inline-flex items-center justify-end gap-1.5 font-mono text-xs tabular-nums text-fg-muted">
        {turn.kind === "tool" && turn.isError && (
          <span className="rounded bg-coral/15 px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wider text-coral">
            Error
          </span>
        )}
        {MetricIcon && <MetricIcon className="size-3" aria-hidden="true" />}
        {metric ?? ""}
      </span>
      <Tooltip label={formatAbsoluteTs(turn.ts)}>
        <span className="pl-3 font-mono text-xs tabular-nums text-fg-muted">
          {formatElapsed(turn.ts, runStart)}
        </span>
      </Tooltip>
    </button>
  );
}

const TOOL_GROUP_TONE = "bg-mint/15 text-mint";

function ToolGroupRow({
  group,
  runStart,
  selected,
  onSelect,
}: {
  group: Extract<DisplayItem, { kind: "group" }>;
  runStart: string | undefined;
  selected: boolean;
  onSelect: () => void;
}) {
  const metric = group.durationMs > 0 ? formatDurationMs(group.durationMs) : null;
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={`grid w-full grid-cols-[5rem_1fr_auto_auto] items-center gap-4 px-5 py-2.5 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500 ${
        selected ? "bg-overlay" : ""
      }`}
    >
      <span
        className={`inline-flex w-fit items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider ${TOOL_GROUP_TONE}`}
      >
        Tool
      </span>
      <span className="min-w-0 truncate text-sm text-fg-3">
        {humanizeToolName(group.toolName)} x{group.children.length}
      </span>
      <span className="inline-flex items-center justify-end gap-1.5 font-mono text-xs tabular-nums text-fg-muted">
        {metric && <ClockIcon className="size-3" aria-hidden="true" />}
        {metric ?? ""}
      </span>
      <Tooltip label={formatAbsoluteTs(group.ts)}>
        <span className="pl-3 font-mono text-xs tabular-nums text-fg-muted">
          {formatElapsed(group.ts, runStart)}
        </span>
      </Tooltip>
    </button>
  );
}

function DebugRow({
  event,
  runStart,
  selected,
  onSelect,
}: {
  event: EventEnvelope;
  runStart: string | undefined;
  selected: boolean;
  onSelect: () => void;
}) {
  const eventName = event.event ?? "";
  const category = debugCategory(eventName);
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-pressed={selected}
      className={`grid w-full grid-cols-[5rem_1fr_auto] items-center gap-4 px-5 py-2.5 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500 ${
        selected ? "bg-overlay" : ""
      }`}
    >
      <span
        className={`inline-flex w-fit items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wider ${debugCategoryTone(category)}`}
      >
        {debugCategoryLabel(category)}
      </span>
      <span className="min-w-0 truncate font-mono text-xs text-fg-2">
        {eventName}
      </span>
      <Tooltip label={formatAbsoluteTs(event.ts)}>
        <span className="font-mono text-xs tabular-nums text-fg-muted">
          {formatElapsed(event.ts, runStart)}
        </span>
      </Tooltip>
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

function prettyJson(raw: string): { text: string; isJson: boolean } {
  if (!raw || !raw.trim()) return { text: "", isJson: false };
  try {
    return { text: JSON.stringify(JSON.parse(raw), null, 2), isJson: true };
  } catch {
    return { text: raw, isJson: false };
  }
}

const JSON_TOKEN_RE =
  /"(?:\\.|[^"\\])*"|\b(?:true|false|null)\b|-?\d+(?:\.\d+)?(?:[eE][+\-]?\d+)?/g;

function highlightJson(text: string): React.ReactNode[] {
  const parts: React.ReactNode[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  let key = 0;
  JSON_TOKEN_RE.lastIndex = 0;
  while ((match = JSON_TOKEN_RE.exec(text)) !== null) {
    if (match.index > lastIndex) {
      parts.push(text.slice(lastIndex, match.index));
    }
    const token = match[0];
    let cls: string;
    if (token.startsWith('"')) {
      const after = text.slice(JSON_TOKEN_RE.lastIndex);
      cls = /^\s*:/.test(after) ? "text-teal-300" : "text-mint";
    } else if (token === "true" || token === "false") {
      cls = "text-coral";
    } else if (token === "null") {
      cls = "text-fg-muted";
    } else {
      cls = "text-amber";
    }
    parts.push(
      <span key={key++} className={cls}>
        {token}
      </span>,
    );
    lastIndex = JSON_TOKEN_RE.lastIndex;
  }
  if (lastIndex < text.length) parts.push(text.slice(lastIndex));
  return parts;
}

function JsonBlock({ value }: { value: string }) {
  const pretty = useMemo(() => prettyJson(value), [value]);
  const tokens = useMemo(
    () => (pretty.isJson ? highlightJson(pretty.text) : null),
    [pretty.isJson, pretty.text],
  );
  return (
    <pre className="max-h-96 overflow-auto whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
      {!pretty.text ? (
        <span className="text-fg-muted">empty</span>
      ) : (
        tokens ?? pretty.text
      )}
    </pre>
  );
}

function EventDetails({
  turn,
  runStart,
  hideMeta = false,
}: {
  turn: TurnType;
  runStart: string | undefined;
  hideMeta?: boolean;
}) {
  const elapsed = formatElapsed(turn.ts, runStart);
  const absolute = (() => {
    const ms = Date.parse(turn.ts);
    if (Number.isNaN(ms)) return turn.ts;
    return new Date(ms).toLocaleString();
  })();

  return (
    <div className="space-y-5">
      {!hideMeta && (
        <DetailField label="When" mono>
          {elapsed ? `${elapsed} · ${absolute}` : absolute}
        </DetailField>
      )}

      {(turn.kind === "system" || turn.kind === "assistant") && (
        <DetailField label="Content">
          <Markdown content={turn.content} />
        </DetailField>
      )}

      {turn.kind === "tool" && (
        <>
          {!hideMeta && (
            <DetailField label="Tool" mono>
              {humanizeToolName(turn.toolName)}{" "}
              <span className="text-fg-muted">({turn.toolName})</span>
            </DetailField>
          )}
          <DetailField label="Input">
            <JsonBlock value={turn.input} />
          </DetailField>
          <DetailField label={turn.isError ? "Error" : "Result"}>
            <JsonBlock value={turn.result} />
          </DetailField>
        </>
      )}

      {turn.kind === "command" && (
        <>
          <DetailField label="Status" mono>
            {turn.running
              ? "Running…"
              : `exit ${turn.exitCode ?? "?"}${
                  turn.durationMs ? ` · ${formatDurationMs(turn.durationMs)}` : ""
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

function decodeBase64Utf8(b64: string): string {
  const binary = atob(b64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
  return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
}

function LogStream({
  runId,
  stageId,
  label,
  byteCount,
  enabled,
}: {
  runId: string;
  stageId: string;
  label: string;
  byteCount: number;
  enabled: boolean;
}) {
  const { data, error, isLoading } = useRunStageLog(runId, stageId, enabled && byteCount > 0);
  const text = useMemo(() => {
    if (!data?.bytes_base64) return "";
    try {
      return decodeBase64Utf8(data.bytes_base64);
    } catch {
      return "";
    }
  }, [data]);
  const truncated =
    data && data.total_bytes > data.next_offset ? data.total_bytes - data.next_offset : 0;

  return (
    <section>
      <header className="mb-1 flex items-baseline justify-between gap-2">
        <h3 className="text-xs font-medium uppercase tracking-wider text-fg-muted">
          {label}
        </h3>
        {byteCount > 0 && (
          <span className="font-mono text-[11px] tabular-nums text-fg-muted">
            {formatBytes(byteCount)}
          </span>
        )}
      </header>
      <pre
        className="overflow-x-auto whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3"
      >
        {byteCount === 0 ? (
          <span className="text-fg-muted">empty</span>
        ) : isLoading && !data ? (
          <span className="text-fg-muted">loading…</span>
        ) : error ? (
          <span className="text-coral">Failed to load output.</span>
        ) : (
          text || <span className="text-fg-muted">empty</span>
        )}
      </pre>
      {truncated > 0 && (
        <p className="mt-1 text-[11px] text-fg-muted">
          Showing first {formatBytes(data!.next_offset)} of {formatBytes(data!.total_bytes)}.
        </p>
      )}
    </section>
  );
}

function CommandStatus({ turn }: { turn: CommandTurn }) {
  const exitTone =
    turn.exitCode == null
      ? "text-fg-muted"
      : turn.exitCode === 0
        ? "text-mint"
        : "text-coral";
  return (
    <span className="ml-auto inline-flex items-center gap-x-3 text-xs">
      {turn.running ? (
        <span className="inline-flex items-center gap-1.5 text-amber">
          <span className="size-1.5 animate-pulse rounded-full bg-amber" />
          Running…
        </span>
      ) : (
        <span className={`font-mono tabular-nums ${exitTone}`}>
          exit {turn.exitCode ?? "?"}
        </span>
      )}
      {turn.durationMs > 0 && (
        <span className="font-mono tabular-nums text-fg-muted">
          {formatDurationMs(turn.durationMs)}
        </span>
      )}
    </span>
  );
}

function CommandScript({ script }: { script: string }) {
  return (
    <section>
      <h3 className="mb-1 text-xs font-medium uppercase tracking-wider text-fg-muted">
        Command
      </h3>
      <pre className="overflow-x-auto whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
        {script || <span className="text-fg-muted">empty</span>}
      </pre>
    </section>
  );
}

function CommandLogs({
  runId,
  stageId,
  turn,
}: {
  runId: string;
  stageId: string;
  turn: CommandTurn | null;
}) {
  if (!turn) {
    return (
      <div className="px-2 py-6 text-sm text-fg-muted">No command output yet.</div>
    );
  }
  return (
    <div className="space-y-5 pl-3 pr-4 sm:pr-6 lg:pr-8">
      <CommandScript script={turn.script} />
      <LogStream
        runId={runId}
        stageId={stageId}
        label="Output"
        byteCount={turn.outputBytes}
        enabled={!turn.running}
      />
    </div>
  );
}

function DetailsPanel({
  title,
  isOpen,
  onClose,
  children,
}: {
  title: string;
  isOpen: boolean;
  onClose: () => void;
  children: React.ReactNode;
}) {
  useEffect(() => {
    if (!isOpen) return;
    function handleKey(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [isOpen, onClose]);

  return (
    <div
      className={`relative shrink-0 self-stretch overflow-hidden transition-[width] duration-200 ease-out ${
        isOpen ? "w-[28rem]" : "w-0"
      }`}
      aria-hidden={isOpen ? undefined : true}
    >
      <div className="absolute inset-y-0 right-0 flex w-[28rem] flex-col border-l border-line bg-panel">
        <div className="flex shrink-0 items-center justify-between border-b border-line px-5 py-3">
          <h2 className="text-sm font-medium text-fg">{title}</h2>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close details"
            className="rounded-md p-1 text-fg-muted transition-colors hover:bg-overlay hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-teal-500"
          >
            <XMarkIcon className="size-5" />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-5 pt-4 pb-[calc(1rem+var(--fabro-interview-dock-clearance,0px))]">
          {isOpen ? children : null}
        </div>
      </div>
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
  return (
    <DetailsPanel
      title={turn ? `${turnLabel(turn)} event` : ""}
      isOpen={turn != null}
      onClose={onClose}
    >
      {turn ? <EventDetails turn={turn} runStart={runStart} /> : null}
    </DetailsPanel>
  );
}

const TOOL_INPUT_PREVIEW_KEYS = ["command", "path", "pattern", "url", "query", "script"];

function toolInputPreview(turn: ToolTurn): string {
  const raw = turn.input;
  if (!raw) return "";
  try {
    const parsed = JSON.parse(raw);
    if (typeof parsed === "string") return oneLine(parsed);
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      const obj = parsed as Record<string, unknown>;
      for (const k of TOOL_INPUT_PREVIEW_KEYS) {
        const v = obj[k];
        if (typeof v === "string" && v) return oneLine(v);
      }
    }
  } catch {
    // input wasn't valid JSON; fall through to oneLine of the raw string
  }
  return oneLine(raw);
}

function ToolGroupChildRow({
  child,
  runStart,
  expanded,
  onToggle,
}: {
  child: { turn: ToolTurn; turnIndex: number };
  runStart: string | undefined;
  expanded: boolean;
  onToggle: () => void;
}) {
  const { turn } = child;
  const metric = turn.durationMs > 0 ? formatDurationMs(turn.durationMs) : null;
  const elapsed = formatElapsed(turn.ts, runStart);
  const Chevron = expanded ? ChevronDownIcon : ChevronRightIcon;
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={expanded}
      className={`grid w-full grid-cols-[1fr_auto_auto] items-center gap-3 px-5 py-2.5 text-left transition-colors hover:bg-overlay focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-teal-500 ${
        expanded ? "bg-overlay" : ""
      }`}
    >
      <span className="min-w-0 truncate font-mono text-xs text-fg-3">
        {toolInputPreview(turn)}
      </span>
      <span className="inline-flex items-center justify-end gap-1.5 font-mono text-xs tabular-nums text-fg-muted">
        {metric && <ClockIcon className="size-3" aria-hidden="true" />}
        {metric ?? ""}
        <Tooltip label={formatAbsoluteTs(turn.ts)}>
          <span className="pl-3 tabular-nums">{elapsed}</span>
        </Tooltip>
      </span>
      <Chevron className="size-4 text-fg-muted" aria-hidden="true" />
    </button>
  );
}

function ToolGroupDetails({
  group,
  runStart,
}: {
  group: Extract<DisplayItem, { kind: "group" }>;
  runStart: string | undefined;
}) {
  const [expandedIndex, setExpandedIndex] = useState<number | null>(null);
  useEffect(() => {
    setExpandedIndex(null);
  }, [group]);

  const elapsed = formatElapsed(group.ts, runStart);
  const totalDuration = group.durationMs > 0 ? formatDurationMs(group.durationMs) : null;

  return (
    <div className="-mx-5 -mt-4">
      <div className="flex items-baseline gap-3 border-b border-line px-5 py-3">
        <span className="text-sm font-medium text-fg">
          {humanizeToolName(group.toolName)}{" "}
          <span className="text-fg-muted">x{group.children.length}</span>
        </span>
        <span className="ml-auto inline-flex items-center gap-1.5 font-mono text-xs tabular-nums text-fg-muted">
          {elapsed}
          {totalDuration && (
            <>
              <span aria-hidden="true">·</span>
              <ClockIcon className="size-3" aria-hidden="true" />
              {totalDuration}
            </>
          )}
        </span>
      </div>
      <ul className="divide-y divide-line">
        {group.children.map((child, i) => (
          <li key={`group-child-${child.turnIndex}`}>
            <ToolGroupChildRow
              child={child}
              runStart={runStart}
              expanded={expandedIndex === i}
              onToggle={() =>
                setExpandedIndex((current) => (current === i ? null : i))
              }
            />
            {expandedIndex === i && (
              <div className="bg-overlay/50 px-5 py-4">
                <EventDetails turn={child.turn} runStart={runStart} hideMeta />
              </div>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

function ToolGroupDetailsPanel({
  group,
  runStart,
  onClose,
}: {
  group: Extract<DisplayItem, { kind: "group" }> | null;
  runStart: string | undefined;
  onClose: () => void;
}) {
  return (
    <DetailsPanel
      title={group ? "Tool group" : ""}
      isOpen={group != null}
      onClose={onClose}
    >
      {group ? <ToolGroupDetails group={group} runStart={runStart} /> : null}
    </DetailsPanel>
  );
}

function DebugEventDetails({ event }: { event: EventEnvelope }) {
  const text = useMemo(() => JSON.stringify(event, null, 2), [event]);
  const tokens = useMemo(() => highlightJson(text), [text]);
  return (
    <pre className="whitespace-pre-wrap rounded-md bg-overlay-strong p-3 font-mono text-xs leading-relaxed text-fg-3">
      {tokens}
    </pre>
  );
}

function DebugEventDetailsPanel({
  event,
  onClose,
}: {
  event: EventEnvelope | null;
  onClose: () => void;
}) {
  return (
    <DetailsPanel
      title={event?.event ?? ""}
      isOpen={event != null}
      onClose={onClose}
    >
      {event ? <DebugEventDetails event={event} /> : null}
    </DetailsPanel>
  );
}

function EventsTabToggle({
  tab,
  stageKind,
  onTabChange,
}: {
  tab: EventsTab;
  stageKind: StageKind;
  onTabChange: (tab: EventsTab) => void;
}) {
  return (
    <div
      role="group"
      aria-label="View"
      className="inline-flex rounded-md bg-panel p-0.5 outline-1 -outline-offset-1 outline-line-strong"
    >
      {EVENTS_TABS.map((value) => {
        const active = tab === value;
        return (
          <button
            key={value}
            type="button"
            onClick={() => onTabChange(value)}
            aria-pressed={active}
            className={`rounded px-2.5 py-1 text-xs font-medium transition-colors focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500 ${
              active
                ? "bg-overlay-strong text-fg"
                : "text-fg-muted hover:text-fg-2"
            }`}
          >
            {eventsTabLabel(value, stageKind)}
          </button>
        );
      })}
    </div>
  );
}

function MultiSelectFilter<T extends string>({
  selected,
  options,
  labelOf,
  onChange,
  emptyMeansAll = false,
}: {
  selected: T[];
  options: readonly T[];
  labelOf: (item: T) => string;
  onChange: (next: T[]) => void;
  emptyMeansAll?: boolean;
}) {
  const allSelected = selected.length === options.length;
  const summary = useMemo(() => {
    if (allSelected || (emptyMeansAll && selected.length === 0)) return "All types";
    if (selected.length === 0) return "No types";
    if (selected.length <= 2) {
      return options
        .filter((o) => selected.includes(o))
        .map(labelOf)
        .join(", ");
    }
    return `${selected.length} types`;
  }, [allSelected, emptyMeansAll, selected, options, labelOf]);

  return (
    <Listbox value={selected} onChange={onChange} multiple>
      <ListboxButton className="inline-flex items-center gap-2 rounded-md bg-panel px-2.5 py-1.5 text-xs text-fg-2 outline-1 -outline-offset-1 outline-line-strong transition-colors hover:bg-overlay-strong focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-teal-500">
        <FunnelIcon className="size-3.5 text-fg-muted" aria-hidden="true" />
        <span className="tabular-nums">{summary}</span>
        <ChevronUpDownIcon className="size-3.5 text-fg-muted" aria-hidden="true" />
      </ListboxButton>
      <ListboxOptions
        transition
        anchor={{ to: "bottom start", gap: 4 }}
        className="z-20 w-44 rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
      >
        {options.map((option) => (
          <ListboxOption
            key={option}
            value={option}
            className="group flex cursor-pointer items-center gap-2.5 px-3 py-1.5 text-xs text-fg-3 data-focus:bg-overlay data-focus:text-fg data-focus:outline-hidden"
          >
            <span className="flex size-3.5 items-center justify-center rounded-sm border border-line-strong bg-panel-alt group-data-selected:border-teal-500 group-data-selected:bg-teal-500">
              <CheckIcon
                className="size-2.5 text-on-primary opacity-0 group-data-selected:opacity-100"
                aria-hidden="true"
              />
            </span>
            <span>{labelOf(option)}</span>
          </ListboxOption>
        ))}
      </ListboxOptions>
    </Listbox>
  );
}

function SearchInput({
  value,
  onChange,
}: {
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <div className="relative w-full max-w-sm min-w-48 flex-1">
      <MagnifyingGlassIcon
        className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-fg-muted"
        aria-hidden="true"
      />
      <input
        type="search"
        name="event-search"
        aria-label="Search events"
        placeholder="Search events"
        autoComplete="off"
        spellCheck={false}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="block w-full rounded-md bg-panel py-1.5 pl-8 pr-2.5 text-xs text-fg outline-1 -outline-offset-1 outline-line-strong placeholder:text-fg-muted focus:outline-2 focus:-outline-offset-1 focus:outline-teal-500 max-sm:text-base/5"
      />
    </div>
  );
}

function EventsToolbar({
  tab,
  stageKind,
  commandTurn,
  onTabChange,
  selectedKinds,
  onKindsChange,
  selectedDebugCategories,
  onDebugCategoriesChange,
  availableDebugCategories,
  search,
  onSearchChange,
  filteredCount,
  totalCount,
  model,
}: {
  tab: EventsTab;
  stageKind: StageKind;
  commandTurn: CommandTurn | null;
  onTabChange: (tab: EventsTab) => void;
  selectedKinds: EventKind[];
  onKindsChange: (kinds: EventKind[]) => void;
  selectedDebugCategories: string[];
  onDebugCategoriesChange: (categories: string[]) => void;
  availableDebugCategories: readonly string[];
  search: string;
  onSearchChange: (value: string) => void;
  filteredCount: number;
  totalCount: number;
  model: string | null;
}) {
  const showFilters = !(tab === "transcript" && stageKind === "command");
  const transcriptAllSelected = selectedKinds.length === EVENT_KINDS.length;
  const debugAllSelected =
    selectedDebugCategories.length === 0 ||
    selectedDebugCategories.length === availableDebugCategories.length;
  const isFiltering =
    showFilters &&
    (tab === "transcript"
      ? !transcriptAllSelected || search.length > 0
      : !debugAllSelected || search.length > 0);

  function clearFilters() {
    if (tab === "transcript") onKindsChange([...EVENT_KINDS]);
    else onDebugCategoriesChange([]);
    onSearchChange("");
  }

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-2 pb-3">
      <EventsTabToggle tab={tab} stageKind={stageKind} onTabChange={onTabChange} />
      {showFilters && (
        <div className="flex flex-1 flex-wrap items-center gap-2">
          {tab === "transcript" ? (
            <MultiSelectFilter<EventKind>
              selected={selectedKinds}
              options={EVENT_KINDS}
              labelOf={(k) => EVENT_KIND_LABEL[k]}
              onChange={onKindsChange}
            />
          ) : (
            <MultiSelectFilter<string>
              selected={selectedDebugCategories}
              options={availableDebugCategories}
              labelOf={debugCategoryLabel}
              onChange={onDebugCategoriesChange}
              emptyMeansAll
            />
          )}
          <SearchInput value={search} onChange={onSearchChange} />
          {isFiltering && (
            <button
              type="button"
              onClick={clearFilters}
              className="rounded px-2 py-1 text-xs text-fg-muted transition-colors hover:bg-overlay hover:text-fg-2 focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-teal-500"
            >
              Clear
            </button>
          )}
        </div>
      )}
      {isFiltering && totalCount > 0 && (
        <span className="text-xs tabular-nums text-fg-muted">
          {filteredCount.toLocaleString()} of {totalCount.toLocaleString()} events
        </span>
      )}
      {model && (
        <span
          className={`inline-flex items-center gap-1.5 text-xs text-fg-muted ${
            showFilters ? "" : "ml-auto"
          }`}
          title="LLM model used for this stage"
        >
          <CpuChipIcon className="size-3.5" aria-hidden="true" />
          <span className="font-mono">{model}</span>
        </span>
      )}
      {!showFilters && commandTurn && <CommandStatus turn={commandTurn} />}
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
  const stageKind = useMemo(() => turnsToStageKind(turns), [turns]);
  const commandTurn = useMemo<CommandTurn | null>(() => {
    for (let i = turns.length - 1; i >= 0; i -= 1) {
      const t = turns[i];
      if (t.kind === "command") return t;
    }
    return null;
  }, [turns]);

  const [panelSelection, setPanelSelection] = useState<PanelSelection | null>(null);
  const [openDebugSeq, setOpenDebugSeq] = useState<number | null>(null);
  useEffect(() => {
    setPanelSelection(null);
    setOpenDebugSeq(null);
  }, [selectedStageId]);

  const [tab, setTab] = useState<EventsTab>("transcript");
  const [selectedKinds, setSelectedKinds] = useState<EventKind[]>([
    ...EVENT_KINDS,
  ]);
  const [selectedDebugCategories, setSelectedDebugCategories] = useState<string[]>([]);
  const [search, setSearch] = useState("");
  const filteredTurns = useMemo<{ turn: TurnType; index: number }[]>(() => {
    const kindSet = new Set(selectedKinds);
    const needle = search.toLowerCase();
    const out: { turn: TurnType; index: number }[] = [];
    turns.forEach((turn, i) => {
      if (!kindSet.has(turn.kind)) return;
      if (needle && !searchableText(turn).toLowerCase().includes(needle)) return;
      out.push({ turn, index: i });
    });
    return out;
  }, [turns, selectedKinds, search]);
  const displayItems = useMemo(
    () => groupConsecutiveTools(filteredTurns),
    [filteredTurns],
  );

  const openTurn =
    panelSelection?.kind === "single" ? turns[panelSelection.turnIndex] ?? null : null;
  const openGroup = useMemo<Extract<DisplayItem, { kind: "group" }> | null>(() => {
    if (panelSelection?.kind !== "group") return null;
    const wanted = panelSelection.childTurnIndices;
    for (const item of displayItems) {
      if (item.kind !== "group") continue;
      if (item.children.length !== wanted.length) continue;
      const matches = item.children.every((c, i) => c.turnIndex === wanted[i]);
      if (matches) return item;
    }
    return null;
  }, [displayItems, panelSelection]);

  const debugEvents = useMemo<EventEnvelope[]>(() => {
    if (!selectedStageId) return [];
    return (stageEventsQuery.data ?? []).filter(
      (e) => activityEventStageId(e) === selectedStageId,
    );
  }, [stageEventsQuery.data, selectedStageId]);
  const openDebugEvent = useMemo<EventEnvelope | null>(
    () =>
      openDebugSeq != null
        ? debugEvents.find((e) => e.seq === openDebugSeq) ?? null
        : null,
    [debugEvents, openDebugSeq],
  );
  const availableDebugCategories = useMemo<string[]>(() => {
    const set = new Set<string>();
    for (const event of debugEvents) {
      if (event.event) set.add(debugCategory(event.event));
    }
    return Array.from(set).sort();
  }, [debugEvents]);
  const stageModel = useMemo(
    () =>
      selectedStageId
        ? extractStageModel(stageEventsQuery.data ?? [], selectedStageId)
        : null,
    [stageEventsQuery.data, selectedStageId],
  );

  const filteredDebugEvents = useMemo<EventEnvelope[]>(() => {
    const useCategoryFilter = selectedDebugCategories.length > 0;
    const cats = new Set(selectedDebugCategories);
    const needle = search.toLowerCase();
    return debugEvents.filter((event) => {
      const name = event.event ?? "";
      if (useCategoryFilter && !cats.has(debugCategory(name))) return false;
      if (needle) {
        const blob = `${name} ${JSON.stringify(event.properties ?? {})}`.toLowerCase();
        if (!blob.includes(needle)) return false;
      }
      return true;
    });
  }, [debugEvents, selectedDebugCategories, search]);

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

  const runStart =
    selectedStage.startedAt ??
    runQuery.data?.start_time ??
    runQuery.data?.created_at;

  return (
    <div className="-mr-4 -mt-6 flex min-h-0 flex-1 sm:-mr-6 lg:-mr-8">
      <div className="shrink-0 pb-6 pr-3 pt-6">
        <StageSidebar stages={stages} runId={id} selectedStageId={selectedStage.id} />
      </div>

      <div className="relative w-px shrink-0">
        <div
          aria-hidden="true"
          className="absolute inset-x-0 top-0 -bottom-6 bg-line"
        />
      </div>

      <div className="flex min-h-0 min-w-0 flex-1 flex-col pt-3">
        <div className="shrink-0 border-b border-line">
          <div className="pl-3 pr-4 sm:pr-6 lg:pr-8">
            <EventsToolbar
              tab={tab}
              stageKind={stageKind}
              commandTurn={commandTurn}
              onTabChange={setTab}
              selectedKinds={selectedKinds}
              onKindsChange={setSelectedKinds}
              selectedDebugCategories={selectedDebugCategories}
              onDebugCategoriesChange={setSelectedDebugCategories}
              availableDebugCategories={availableDebugCategories}
              search={search}
              onSearchChange={setSearch}
              filteredCount={tab === "transcript" ? filteredTurns.length : filteredDebugEvents.length}
              totalCount={tab === "transcript" ? turns.length : debugEvents.length}
              model={stageModel}
            />
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto pt-2 pb-[calc(1.5rem+var(--fabro-interview-dock-clearance,0px))]">
          {tab === "transcript" ? (
            stageKind === "command" ? (
              <CommandLogs runId={id} stageId={selectedStage.id} turn={commandTurn} />
            ) : turns.length > 0 && filteredTurns.length === 0 ? (
              <div className="px-2 py-6 text-sm text-fg-muted">
                No events match these filters.
              </div>
            ) : (
              displayItems.map((item) => {
                if (item.kind === "single") {
                  return (
                    <EventRow
                      key={`turn-${item.turnIndex}`}
                      turn={item.turn}
                      runStart={runStart}
                      selected={
                        panelSelection?.kind === "single" &&
                        panelSelection.turnIndex === item.turnIndex
                      }
                      onSelect={() =>
                        setPanelSelection({ kind: "single", turnIndex: item.turnIndex })
                      }
                    />
                  );
                }
                const childIndices = item.children.map((c) => c.turnIndex);
                const groupKey = `group-${childIndices.join("-")}`;
                const isSelected =
                  panelSelection?.kind === "group" &&
                  panelSelection.childTurnIndices.length === childIndices.length &&
                  panelSelection.childTurnIndices.every((v, i) => v === childIndices[i]);
                return (
                  <ToolGroupRow
                    key={groupKey}
                    group={item}
                    runStart={runStart}
                    selected={isSelected}
                    onSelect={() =>
                      setPanelSelection({
                        kind: "group",
                        childTurnIndices: childIndices,
                      })
                    }
                  />
                );
              })
            )
          ) : debugEvents.length > 0 && filteredDebugEvents.length === 0 ? (
            <div className="px-2 py-6 text-sm text-fg-muted">
              No events match these filters.
            </div>
          ) : (
            filteredDebugEvents.map((event) => (
              <DebugRow
                key={`debug-${event.seq}`}
                event={event}
                runStart={runStart}
                selected={openDebugSeq === event.seq}
                onSelect={() => setOpenDebugSeq(event.seq)}
              />
            ))
          )}
        </div>
      </div>

      {tab === "transcript" ? (
        stageKind === "command" ? null : panelSelection?.kind === "group" ? (
          <ToolGroupDetailsPanel
            group={openGroup}
            runStart={runStart}
            onClose={() => setPanelSelection(null)}
          />
        ) : (
          <EventDetailsPanel
            turn={openTurn}
            runStart={runStart}
            onClose={() => setPanelSelection(null)}
          />
        )
      ) : (
        <DebugEventDetailsPanel
          event={openDebugEvent}
          onClose={() => setOpenDebugSeq(null)}
        />
      )}
    </div>
  );
}
