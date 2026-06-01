import TerminalView from "../components/terminal-view";
import { FabroToaster, ToastProvider } from "../components/toast";
import { useDocumentTitle } from "../hooks/effects";

export default function RunTerminal({ params }: { params: { id: string } }) {
  useDocumentTitle(`Terminal · ${params.id} · Fabro`);

  return (
    <ToastProvider>
      <div className="h-screen w-screen overflow-hidden">
        <TerminalView runId={params.id} chromeless />
      </div>
      {typeof document !== "undefined" && <FabroToaster />}
    </ToastProvider>
  );
}
