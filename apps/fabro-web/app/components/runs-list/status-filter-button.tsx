import { CheckIcon, PlusCircleIcon } from "@heroicons/react/24/outline";
import { Listbox, ListboxButton, ListboxOption, ListboxOptions } from "@headlessui/react";
import type { BoardColumn } from "@qltysh/fabro-api-client";

import { columnStatusDisplay } from "../../data/runs";
import { STATUS_FILTER_OPTIONS } from "./preferences";

export function StatusFilterButton({
  value,
  onChange,
}: {
  value:    Set<BoardColumn>;
  onChange: (next: Set<BoardColumn>) => void;
}) {
  // Empty set and a full selection both mean "no filter is narrowing the
  // result set", so leave the button looking inactive in both cases.
  const active =
    value.size > 0 && value.size < STATUS_FILTER_OPTIONS.length;

  const selectedList = STATUS_FILTER_OPTIONS.filter((c) => value.has(c));
  const buttonLabel = (() => {
    if (!active) return "Status";
    if (selectedList.length === 1) {
      return `Status: ${columnStatusDisplay[selectedList[0]].label}`;
    }
    return `Status: ${selectedList.length}`;
  })();

  const selectedArray = [...value];

  return (
    <Listbox
      value={selectedArray}
      onChange={(next: BoardColumn[]) => onChange(new Set<BoardColumn>(next))}
      multiple
    >
      <ListboxButton
        className={`inline-flex items-center gap-1.5 rounded-md border px-3 py-2 text-xs font-medium transition-colors ${
          active
            ? "border-line-strong bg-panel text-fg-2"
            : "border-line bg-panel/80 text-fg-muted hover:text-fg-3"
        }`}
      >
        <PlusCircleIcon className="size-4" aria-hidden="true" />
        <span>{buttonLabel}</span>
      </ListboxButton>
      <ListboxOptions
        anchor="bottom start"
        className="z-20 mt-1 min-w-[12rem] rounded-md border border-line bg-panel py-1 text-xs shadow-lg focus:outline-none"
      >
        {STATUS_FILTER_OPTIONS.map((status) => {
          const display = columnStatusDisplay[status];
          return (
            <ListboxOption
              key={status}
              value={status}
              className={({ focus }) =>
                `flex cursor-pointer items-center justify-between gap-3 px-3 py-1.5 text-fg-2 ${focus ? "bg-overlay" : ""}`
              }
            >
              {({ selected }) => (
                <>
                  <span className="flex items-center gap-2">
                    <span className={`size-2 shrink-0 rounded-full ${display.dot}`} aria-hidden="true" />
                    <span className={selected ? display.text : "text-fg-2"}>{display.label}</span>
                  </span>
                  {selected ? (
                    <CheckIcon className="size-4 shrink-0 text-teal-500" aria-hidden="true" />
                  ) : (
                    <span className="size-4 shrink-0" aria-hidden="true" />
                  )}
                </>
              )}
            </ListboxOption>
          );
        })}
      </ListboxOptions>
    </Listbox>
  );
}
