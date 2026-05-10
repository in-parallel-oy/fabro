import { useEffect } from "react";

import TerminalView from "../components/terminal-view";

export default function RunTerminal({ params }: { params: { id: string } }) {
  useEffect(() => {
    const previous = document.title;
    document.title = `Terminal · ${params.id} · Fabro`;
    return () => {
      document.title = previous;
    };
  }, [params.id]);

  return (
    <div className="h-screen w-screen overflow-hidden">
      <TerminalView runId={params.id} chromeless />
    </div>
  );
}
