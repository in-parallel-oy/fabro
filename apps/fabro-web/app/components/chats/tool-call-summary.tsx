import type {
  ThreadAssistantMessagePart,
  ToolCallMessagePart,
  ToolCallMessagePartProps,
} from "@assistant-ui/react";
import { useMessage } from "@assistant-ui/react";
import { WrenchScrewdriverIcon } from "@heroicons/react/24/outline";

const EMPTY_PARTS: readonly ThreadAssistantMessagePart[] = [];

/**
 * Tool-call renderer for the narrow Ask Fabro sidebar. Tool calls are collapsed
 * to a single unobtrusive count ("3 tool calls", or "7 tool calls, 2 with
 * errors") instead of dumping each call's arguments and results into the
 * thread.
 *
 * assistant-ui renders this component once per tool-call part. We render the
 * aggregate summary only on the first tool-call part of the message and skip
 * the rest, so the whole message contributes a single compact line.
 */
export default function ToolCallSummary(props: ToolCallMessagePartProps) {
  const content = useMessage((message) =>
    message.role === "assistant" ? message.content : EMPTY_PARTS,
  );
  const toolCalls = content.filter(
    (part): part is ToolCallMessagePart => part.type === "tool-call",
  );

  // Render once per message, anchored to the first tool-call part.
  if (toolCalls[0]?.toolCallId !== props.toolCallId) {
    return null;
  }

  const total = toolCalls.length;
  const errored = toolCalls.filter((toolCall) => toolCall.isError).length;
  const noun = total === 1 ? "call" : "calls";

  return (
    <div className="my-2 inline-flex items-center gap-1.5 rounded-md border border-line bg-overlay/60 px-2 py-1 text-xs text-fg-muted">
      <WrenchScrewdriverIcon className="size-3.5" aria-hidden="true" />
      <span>
        {total} tool {noun}
        {errored > 0 && (
          <span className="text-rose-300">
            , {errored} with {errored === 1 ? "an error" : "errors"}
          </span>
        )}
      </span>
    </div>
  );
}
