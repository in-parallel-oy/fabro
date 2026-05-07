import {
  useEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import {
  ArrowPathIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  ClockIcon,
  FolderIcon,
  RectangleStackIcon,
  SignalIcon,
} from "@heroicons/react/20/solid";
import { Link, Outlet, useLocation, useMatches } from "react-router";
import { Menu, MenuButton, MenuItem, MenuItems } from "@headlessui/react";

import { InterviewDock } from "../components/interview-dock";
import { SteerComposer } from "../components/steer-composer";
import { ErrorState } from "../components/state";
import { useToast } from "../components/toast";
import { SECONDARY_BUTTON_CLASS } from "../components/ui";
import {
  isRunStatus,
  mapRunSummaryToRunItem,
  runStatusDisplay,
  type RunSummary,
} from "../data/runs";
import { useDemoMode } from "../lib/demo-mode";
import {
  useArchiveRun,
  useCancelRun,
  usePreviewRun,
  useUnarchiveRun,
  type LifecycleMutationResult,
  type PreviewMutationResult,
} from "../lib/mutations";
import { formatRelativeTime } from "../lib/format";
import { useRunEvents } from "../lib/run-events";
import { useRunToasts } from "../hooks/use-run-toasts";
import { useRun, useRunQuestions } from "../lib/queries";
import {
  canArchive,
  canCancel,
  canUnarchive,
  isTerminalCancelledRun,
  mapError,
  type LifecycleAction,
  type LifecycleActionError,
} from "../lib/run-actions";

const allTabs = [
  { name: "Overview", path: "", count: null, demoOnly: false },
  { name: "Stages", path: "/stages", count: null, demoOnly: false },
  { name: "Files Changed", path: "/files", count: null, demoOnly: false },
  { name: "Graph", path: "/graph", count: null, demoOnly: false },
  { name: "Billing", path: "/billing", count: null, demoOnly: false },
];

export const handle = { hideHeader: true };

const ACTIONS_TRIGGER_CLASS =
  `${SECONDARY_BUTTON_CLASS} disabled:cursor-not-allowed disabled:opacity-60`;

const MENU_ITEM_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-fg-3 transition-colors data-focus:bg-overlay data-focus:text-fg data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

const MENU_ITEM_DANGER_CLASS =
  "flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-coral transition-colors data-focus:bg-coral/10 data-focus:text-coral data-focus:outline-hidden disabled:cursor-not-allowed disabled:opacity-60";

function classNames(...classes: Array<string | false | null | undefined>) {
  return classes.filter(Boolean).join(" ");
}

function useTickingNow(intervalMs: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(id);
  }, [intervalMs]);
  return now;
}

type RunDetailRun = ReturnType<typeof mapRunSummaryToRunItem> & {
  statusLabel: string;
  statusDot: string;
  statusText: string;
};

export type RunDetailActionResult = PreviewMutationResult | LifecycleMutationResult;

export interface LifecycleToastState {
  activeArchiveToastId: string | null;
  lastProcessed: Record<LifecycleAction, RunDetailActionResult | null>;
}

type ToastApi = Pick<ReturnType<typeof useToast>, "push" | "dismiss">;

const INITIAL_LIFECYCLE_TOAST_STATE: LifecycleToastState = {
  activeArchiveToastId: null,
  lastProcessed: { cancel: null, archive: null, unarchive: null },
};

export function lifecycleActionVisibility(status: string | null | undefined) {
  return {
    showPrimaryCancel: canCancel(status),
    showArchive: canArchive(status),
    showUnarchive: canUnarchive(status),
  };
}

function buildRunDetailRun(summary: RunSummary): RunDetailRun {
  const item = mapRunSummaryToRunItem(summary);
  const rawStatus = summary.status;
  const statusKind = rawStatus.kind;
  const display = isRunStatus(statusKind)
    ? runStatusDisplay[statusKind]
    : { label: statusKind, dot: "bg-fg-muted", text: "text-fg-muted" };

  return {
    ...item,
    statusLabel: display.label,
    statusDot: display.dot,
    statusText: display.text,
  };
}

export function meta({ data }: any) {
  const run = data?.run;
  return [{ title: run ? `${run.title} — Fabro` : "Run — Fabro" }];
}

export default function RunDetail({ params }: { params: { id: string } }) {
  const demoMode = useDemoMode();
  const runQuery = useRun(params.id);
  const run = runQuery.data ? buildRunDetailRun(runQuery.data) : null;
  const statusKind = runQuery.data?.status?.kind;
  const isBlocked = statusKind === "blocked";
  const questionsQuery = useRunQuestions(params.id, isBlocked);
  const pendingQuestions = questionsQuery.data ?? [];
  const { pathname } = useLocation();
  const matches = useMatches();
  const basePath = `/runs/${params.id}`;
  const previewMutation = usePreviewRun(params.id);
  const cancelMutation = useCancelRun(params.id);
  const archiveMutation = useArchiveRun(params.id);
  const unarchiveMutation = useUnarchiveRun(params.id);
  const { push, dismiss } = useToast();
  const tabs = allTabs.filter((t) => !t.demoOnly || demoMode);
  const lifecycleToastStateRef = useRef<LifecycleToastState>(INITIAL_LIFECYCLE_TOAST_STATE);
  const [steerOpen, setSteerOpen] = useState(false);
  const now = useTickingNow(30_000);
  const fullHeight = matches.some(
    (m) => (m.handle as { fullHeight?: boolean } | undefined)?.fullHeight,
  );

  useRunEvents(params.id);
  useRunToasts(params.id);

  useEffect(() => {
    if (previewMutation.data?.intent === "preview") {
      window.open(previewMutation.data.url, "_blank");
    }
  }, [previewMutation.data]);

  useEffect(() => {
    lifecycleToastStateRef.current = handleLifecycleToastResult(
      "cancel",
      cancelMutation.data,
      lifecycleToastStateRef.current,
      { push, dismiss },
    );
  }, [cancelMutation.data, dismiss, push]);

  useEffect(() => {
    lifecycleToastStateRef.current = handleLifecycleToastResult(
      "archive",
      archiveMutation.data,
      lifecycleToastStateRef.current,
      { push, dismiss },
    );
  }, [archiveMutation.data, dismiss, push]);

  useEffect(() => {
    lifecycleToastStateRef.current = handleLifecycleToastResult(
      "unarchive",
      unarchiveMutation.data,
      lifecycleToastStateRef.current,
      { push, dismiss },
    );
  }, [dismiss, push, unarchiveMutation.data]);

  if (runQuery.isLoading && !run) {
    return <div className="py-12" />;
  }

  if (!run) {
    return (
      <div className="py-12">
        <ErrorState
          title="Run not found"
          description="The run you're looking for doesn't exist or was deleted."
        />
      </div>
    );
  }

  const visibility = lifecycleActionVisibility(run.lifecycleStatus);
  const previewPending = previewMutation.isMutating;
  const cancelPending = cancelMutation.isMutating;
  const archivePending = archiveMutation.isMutating;
  const unarchivePending = unarchiveMutation.isMutating;
  const hasPendingQuestions = isBlocked && pendingQuestions.length > 0;
  const rootStyle = fullHeight && hasPendingQuestions
    ? ({ "--fabro-interview-dock-clearance": "18rem" } as CSSProperties)
    : undefined;

  return (
    <div
      className={fullHeight ? "flex h-full min-h-0 flex-col" : undefined}
      style={rootStyle}
    >
      <nav
        className={classNames(
          "mb-4 flex items-center gap-1 text-sm text-fg-muted",
          fullHeight && "shrink-0",
        )}
      >
        <Link to="/runs" className="text-fg-3 hover:text-fg">Runs</Link>
        {demoMode && (
          <>
            <ChevronRightIcon className="size-3" />
            <Link to={`/workflows/${run.workflow}`} className="text-fg-3 hover:text-fg">
              {run.workflow}
            </Link>
          </>
        )}
        <ChevronRightIcon className="size-3" />
        <span>{run.title}</span>
      </nav>

      <div
        className={classNames(
          "mb-6 flex flex-wrap items-start gap-4",
          fullHeight && "shrink-0",
        )}
      >
        <div className="min-w-0 flex-1">
          <h2 className="text-xl font-semibold text-fg">{run.title}</h2>
          <div className="mt-2 flex flex-wrap items-center gap-x-5 gap-y-2 text-sm">
            <span className="flex items-center gap-1.5">
              <span className={`size-2 rounded-full ${run.statusDot}`} />
              <span className={`font-medium ${run.statusText}`}>{run.statusLabel}</span>
            </span>
            <span className="flex items-center gap-1.5 font-mono text-xs text-fg-muted">
              <FolderIcon className="size-3.5" aria-hidden="true" />
              {run.repo}
            </span>
            <span className="flex items-center gap-1.5 font-mono text-xs text-fg-muted">
              <RectangleStackIcon className="size-3.5" aria-hidden="true" />
              {run.workflow}
            </span>
            {run.elapsed && (
              <span className="flex items-center gap-1.5 font-mono text-xs text-fg-muted">
                <ClockIcon className="size-3.5" aria-hidden="true" />
                {run.elapsed}
              </span>
            )}
            {run.lastEventAt && (
              <span
                className="flex items-center gap-1.5 font-mono text-xs text-fg-muted"
                title={`Last event ${new Date(run.lastEventAt).toLocaleString()}`}
              >
                <SignalIcon className="size-3.5" aria-hidden="true" />
                {formatRelativeTime(run.lastEventAt, now)}
              </span>
            )}
          </div>
        </div>

        <ActionsMenu
          canSteer={statusKind === "running"}
          onSteer={() => setSteerOpen(true)}
          canPreview={!!run.sandboxId}
          previewPending={previewPending}
          onPreview={() => void previewMutation.trigger({
            port: 3000,
            expires_in_secs: 3600,
          })}
          canArchive={visibility.showArchive}
          archivePending={archivePending}
          onArchive={() => void archiveMutation.trigger()}
          canUnarchive={visibility.showUnarchive}
          unarchivePending={unarchivePending}
          onUnarchive={() => void unarchiveMutation.trigger()}
          canCancel={visibility.showPrimaryCancel}
          cancelPending={cancelPending}
          onCancel={() => void cancelMutation.trigger()}
        />
      </div>

      <div
        className={classNames(
          "relative before:pointer-events-none before:absolute before:bottom-0 before:left-1/2 before:h-px before:w-screen before:-translate-x-1/2 before:bg-line",
          fullHeight && "shrink-0",
        )}
      >
        <nav className="-mb-px flex gap-6">
          {tabs.map((tab) => {
            const tabPath = `${basePath}${tab.path}`;
            const isActive = tab.name === "Stages"
              ? pathname.startsWith(`${basePath}/stages`)
              : pathname === tabPath;
            return (
              <Link
                key={tab.name}
                to={tabPath}
                className={`border-b-2 pb-3 text-sm font-medium transition-colors ${
                  isActive
                    ? "border-teal-500 text-fg"
                    : "border-transparent text-fg-muted hover:border-line-strong hover:text-fg-3"
                }`}
              >
                {tab.name}
                {tab.count != null && (
                  <span className={`ml-1.5 rounded-full px-1.5 py-0.5 text-xs font-normal tabular-nums ${
                    isActive ? "bg-overlay-strong text-fg-3" : "bg-overlay text-fg-muted"
                  }`}>
                    {tab.count}
                  </span>
                )}
              </Link>
            );
          })}
        </nav>
      </div>

      <div className={fullHeight ? "mt-6 flex min-h-0 flex-1 flex-col" : "mt-6"}>
        <Outlet />
      </div>

      <SteerComposer
        runId={params.id}
        open={steerOpen}
        onClose={() => setSteerOpen(false)}
      />

      {hasPendingQuestions && (
        fullHeight ? (
          <InterviewDock runId={params.id} questions={pendingQuestions} />
        ) : (
          <>
            <div aria-hidden="true" className="h-72" />
            <InterviewDock runId={params.id} questions={pendingQuestions} />
          </>
        )
      )}
    </div>
  );
}

function isLifecycleActionFailure(
  value: RunDetailActionResult,
): value is Extract<LifecycleMutationResult, { ok: false }> {
  return "ok" in value && value.ok === false;
}

export function handleLifecycleToastResult(
  intent: LifecycleAction,
  result: RunDetailActionResult | undefined,
  state: LifecycleToastState,
  toastApi: ToastApi,
): LifecycleToastState {
  if (!result || result.intent !== intent) return state;
  if (state.lastProcessed[intent] === result) return state;

  const nextState: LifecycleToastState = {
    ...state,
    lastProcessed: { ...state.lastProcessed, [intent]: result },
  };

  if (isLifecycleActionFailure(result)) {
    toastApi.push({ message: mapError(result.error, intent), tone: "error" });
    return nextState;
  }

  if (intent === "cancel") {
    toastApi.push({
      message: isTerminalCancelledRun(result.run) ? "Run cancelled." : "Cancellation requested.",
    });
    return nextState;
  }

  if (state.activeArchiveToastId) {
    toastApi.dismiss(state.activeArchiveToastId);
  }

  if (intent === "archive") {
    return {
      ...nextState,
      activeArchiveToastId: toastApi.push({ message: "Run archived." }),
    };
  }

  toastApi.push({ message: "Run restored." });
  return { ...nextState, activeArchiveToastId: null };
}

interface ActionsMenuProps {
  canSteer: boolean;
  onSteer: () => void;
  canPreview: boolean;
  previewPending: boolean;
  onPreview: () => void;
  canArchive: boolean;
  archivePending: boolean;
  onArchive: () => void;
  canUnarchive: boolean;
  unarchivePending: boolean;
  onUnarchive: () => void;
  canCancel: boolean;
  cancelPending: boolean;
  onCancel: () => void;
}

function ActionsMenu(props: ActionsMenuProps) {
  const {
    canSteer, onSteer,
    canPreview, previewPending, onPreview,
    canArchive, archivePending, onArchive,
    canUnarchive, unarchivePending, onUnarchive,
    canCancel, cancelPending, onCancel,
  } = props;

  const hasOps = canPreview || canSteer;
  const hasLifecycle = canArchive || canUnarchive;
  const hasDestructive = canCancel;
  const hasAny = hasOps || hasLifecycle || hasDestructive;
  const anyPending = previewPending || archivePending || unarchivePending || cancelPending;

  if (!hasAny) return null;

  return (
    <Menu as="div" className="shrink-0">
      <MenuButton className={ACTIONS_TRIGGER_CLASS} disabled={anyPending}>
        {anyPending && <ArrowPathIcon className="size-4 animate-spin" aria-hidden="true" />}
        Actions
        <ChevronDownIcon className="-mr-1 size-4 text-fg-muted" aria-hidden="true" />
      </MenuButton>
      <MenuItems
        transition
        anchor={{ to: "bottom end", gap: 4 }}
        className="z-20 w-44 origin-top-right rounded-md bg-panel py-1 outline-1 -outline-offset-1 outline-line-strong transition data-closed:scale-95 data-closed:opacity-0 data-enter:duration-100 data-enter:ease-out data-leave:duration-75 data-leave:ease-in"
      >
        {canPreview && (
          <MenuItem>
            <button
              type="button"
              onClick={onPreview}
              disabled={previewPending}
              className={MENU_ITEM_CLASS}
            >
              {previewPending ? "Opening…" : "Preview"}
            </button>
          </MenuItem>
        )}
        {canSteer && (
          <MenuItem>
            <button type="button" onClick={onSteer} className={MENU_ITEM_CLASS}>
              Steer
            </button>
          </MenuItem>
        )}
        {hasOps && hasLifecycle && <div className="my-1 h-px bg-line" role="separator" />}
        {canArchive && (
          <MenuItem>
            <button
              type="button"
              onClick={onArchive}
              disabled={archivePending}
              className={MENU_ITEM_CLASS}
            >
              {archivePending ? "Archiving…" : "Archive"}
            </button>
          </MenuItem>
        )}
        {canUnarchive && (
          <MenuItem>
            <button
              type="button"
              onClick={onUnarchive}
              disabled={unarchivePending}
              className={MENU_ITEM_CLASS}
            >
              {unarchivePending ? "Restoring…" : "Unarchive"}
            </button>
          </MenuItem>
        )}
        {(hasOps || hasLifecycle) && hasDestructive && (
          <div className="my-1 h-px bg-line" role="separator" />
        )}
        {canCancel && (
          <MenuItem>
            <button
              type="button"
              onClick={onCancel}
              disabled={cancelPending}
              className={MENU_ITEM_DANGER_CLASS}
            >
              {cancelPending ? "Cancelling…" : "Cancel"}
            </button>
          </MenuItem>
        )}
      </MenuItems>
    </Menu>
  );
}
