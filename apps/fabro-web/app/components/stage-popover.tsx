import { useMemo } from "react";
import type { EventEnvelope, StageState } from "@qltysh/fabro-api-client";

import { formatTokenCount } from "../lib/format";
import { useRunStageEvents } from "../lib/queries";
import {
  formatStageLabel,
  stageStatusLabel,
  stageStatusTone,
  type Stage,
} from "../lib/stage-sidebar";
import { timeAgo } from "../lib/time";
import {
  getArray,
  getNumber,
  getObject,
  getString,
} from "../lib/unknown";
import { PopoverHeader, PopoverRow, PopoverRows } from "./ui";

const REASON_MAX_CHARS = 240;

export interface StageSummary {
  attempt?: number;
  maxAttempts?: number;
  failureMessage?: string;
  notes?: string;
  inputTokens?: number;
  outputTokens?: number;
  filesTouchedCount?: number;
  systemActor?: string;
  exitCode?: number;
}

/**
 * Reduce a stage's event stream to the latest values relevant to the popover.
 * Events arrive in `seq` order, so later events overwrite earlier ones —
 * giving us the most recent attempt counts, failure message, and billing.
 */
export function deriveStageSummary(events: EventEnvelope[]): StageSummary {
  const summary: StageSummary = {};
  for (const e of events) {
    const props = e.properties ?? {};
    switch (e.event) {
      case "stage.started": {
        const attempt = getNumber(props, "attempt");
        const max = getNumber(props, "max_attempts");
        if (attempt !== undefined) summary.attempt = attempt;
        if (max !== undefined) summary.maxAttempts = max;
        break;
      }
      case "stage.completed": {
        readFailure(summary, getObject(props, "failure"));
        readBilling(summary, getObject(props, "billing"));
        readTermination(summary, getObject(props, "termination"));
        const notes = getString(props, "notes");
        if (notes !== undefined) summary.notes = notes;
        const files = getArray(props, "files_touched");
        if (files !== undefined) summary.filesTouchedCount = files.length;
        break;
      }
      case "stage.failed": {
        readFailure(summary, getObject(props, "failure"));
        readBilling(summary, getObject(props, "billing"));
        break;
      }
    }
  }
  return summary;
}

function readFailure(summary: StageSummary, failure: unknown) {
  if (!failure) return;
  const message = getString(failure, "message");
  if (message !== undefined) summary.failureMessage = message;
  const actor = getString(failure, "system_actor");
  if (actor !== undefined) summary.systemActor = actor;
}

function readBilling(summary: StageSummary, billing: unknown) {
  if (!billing) return;
  const input = getNumber(billing, "input_tokens");
  const output = getNumber(billing, "output_tokens");
  if (input !== undefined) summary.inputTokens = input;
  if (output !== undefined) summary.outputTokens = output;
}

function readTermination(summary: StageSummary, termination: unknown) {
  if (!termination) return;
  const exitCode = getNumber(termination, "exit_code");
  if (exitCode !== undefined) summary.exitCode = exitCode;
}

/** Trim, collapse blank-line runs, and cap to ~240 chars with an ellipsis. */
function truncateReason(text: string): { display: string; truncated: boolean } {
  const collapsed = text
    .replace(/\r\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  if (collapsed.length <= REASON_MAX_CHARS) {
    return { display: collapsed, truncated: false };
  }
  return {
    display: `${collapsed.slice(0, REASON_MAX_CHARS - 1).trimEnd()}…`,
    truncated: true,
  };
}

function TruncatedReason({ text }: { text: string }) {
  const { display, truncated } = truncateReason(text);
  return (
    <span
      className="line-clamp-3 break-words text-fg-2"
      title={truncated ? text : undefined}
    >
      {display}
    </span>
  );
}

function StatusPill({ status }: { status: StageState }) {
  return (
    <span
      className={`shrink-0 rounded-md px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide ${stageStatusTone(status)}`}
    >
      {stageStatusLabel(status)}
    </span>
  );
}

function ModelRow({ providerUsed }: { providerUsed: Stage["providerUsed"] }) {
  if (!providerUsed?.model) return null;
  const effort = providerUsed.reasoning_effort;
  return (
    <PopoverRow label="Model">
      <span className="break-all font-mono">
        {effort ? `${providerUsed.model}[${effort}]` : providerUsed.model}
      </span>
    </PopoverRow>
  );
}

function AttemptRow({ summary }: { summary: StageSummary }) {
  if (summary.attempt === undefined) return null;
  const max = summary.maxAttempts;
  return (
    <PopoverRow label="Attempt">
      {max && max > 1 ? `${summary.attempt} of ${max}` : `${summary.attempt}`}
    </PopoverRow>
  );
}

function TokensRow({ summary }: { summary: StageSummary }) {
  if (summary.inputTokens === undefined && summary.outputTokens === undefined) return null;
  const inLabel = formatTokenCount(summary.inputTokens ?? 0, { compactDecimal: true });
  const outLabel = formatTokenCount(summary.outputTokens ?? 0, { compactDecimal: true });
  return (
    <PopoverRow label="Tokens">
      <span className="font-mono tabular-nums">
        {inLabel} in / {outLabel} out
      </span>
    </PopoverRow>
  );
}

function StatusTail({
  stage,
  summary,
  loading,
}: {
  stage: Stage;
  summary: StageSummary;
  loading: boolean;
}) {
  switch (stage.status) {
    case "pending":
    case "cancelled":
      return summary.systemActor ? (
        <PopoverRow label="Cancelled by">{summary.systemActor}</PopoverRow>
      ) : null;
    case "running":
      return (
        <>
          <AttemptRow summary={summary} />
          <ModelRow providerUsed={stage.providerUsed} />
        </>
      );
    case "retrying":
      return (
        <>
          <AttemptRow summary={summary} />
          {summary.failureMessage && (
            <PopoverRow label="Previous failure">
              <TruncatedReason text={summary.failureMessage} />
            </PopoverRow>
          )}
        </>
      );
    case "succeeded":
      return (
        <>
          <ModelRow providerUsed={stage.providerUsed} />
          <TokensRow summary={summary} />
          {summary.filesTouchedCount !== undefined && summary.filesTouchedCount > 0 && (
            <PopoverRow label="Files touched">{summary.filesTouchedCount}</PopoverRow>
          )}
        </>
      );
    case "partially_succeeded":
      return (
        <>
          {summary.notes && (
            <PopoverRow label="Notes">
              <TruncatedReason text={summary.notes} />
            </PopoverRow>
          )}
          <ModelRow providerUsed={stage.providerUsed} />
          <TokensRow summary={summary} />
        </>
      );
    case "failed": {
      const isCommand = stage.handler === "command";
      return (
        <>
          {summary.failureMessage ? (
            <PopoverRow label="Reason">
              <TruncatedReason text={summary.failureMessage} />
            </PopoverRow>
          ) : loading ? (
            <LoadingRow />
          ) : null}
          <AttemptRow summary={summary} />
          {isCommand && summary.exitCode !== undefined ? (
            <PopoverRow label="Exit code">
              <span className="font-mono tabular-nums">{summary.exitCode}</span>
            </PopoverRow>
          ) : (
            <ModelRow providerUsed={stage.providerUsed} />
          )}
        </>
      );
    }
    case "skipped":
      return summary.notes ? (
        <PopoverRow label="Reason">
          <TruncatedReason text={summary.notes} />
        </PopoverRow>
      ) : loading ? (
        <LoadingRow />
      ) : null;
    default:
      return null;
  }
}

function LoadingRow() {
  return (
    <>
      <dt className="text-fg-3" />
      <dd className="text-fg-muted italic">Loading details…</dd>
    </>
  );
}

interface StagePopoverProps {
  runId: string;
  stage: Stage;
  /** Live duration string from the sidebar (formatted, ticking for active stages). */
  duration: string;
}

export function StagePopover({ runId, stage, duration }: StagePopoverProps) {
  const { data: events } = useRunStageEvents(runId, stage.id);
  const summary = useMemo(() => deriveStageSummary(events ?? []), [events]);
  const loading = events === undefined && stage.status !== "pending";

  return (
    <div className="min-w-[14rem]">
      <PopoverHeader>
        <div className="flex items-center justify-between gap-3">
          <span className="font-mono text-fg">{formatStageLabel(stage)}</span>
          <StatusPill status={stage.status} />
        </div>
      </PopoverHeader>
      <PopoverRows>
        <PopoverRow label="Handler">
          <span className="font-mono">{stage.handler}</span>
        </PopoverRow>
        {stage.startedAt && (
          <PopoverRow label="Started">
            <time dateTime={stage.startedAt} title={stage.startedAt}>
              {timeAgo(stage.startedAt)}
            </time>
          </PopoverRow>
        )}
        {duration !== "--" && (
          <PopoverRow label="Duration">
            <span className="font-mono tabular-nums">{duration}</span>
          </PopoverRow>
        )}
        <StatusTail stage={stage} summary={summary} loading={loading} />
      </PopoverRows>
    </div>
  );
}
