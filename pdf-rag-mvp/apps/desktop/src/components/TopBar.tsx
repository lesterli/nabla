import { useState, useEffect } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface TopBarProps {
  onImportDone: () => void;
  onSettingsClick: () => void;
}

interface ImportProgress {
  file_name: string;
  stage: string;
  file_index: number;
  file_total: number;
  message: string;
}

const stageLabels: Record<string, string> = {
  parse: "Parsing",
  chunk: "Chunking",
  embed: "Embedding",
  done: "Done",
  error: "Error",
  skip: "Skipped",
};

export function TopBar({ onImportDone, onSettingsClick }: TopBarProps) {
  const [importing, setImporting] = useState(false);
  const [progress, setProgress] = useState<ImportProgress | null>(null);
  const [resultMsg, setResultMsg] = useState<string | null>(null);

  useEffect(() => {
    const unlisten = listen<ImportProgress>("import-progress", (event) => {
      setProgress(event.payload);
      if (event.payload.stage === "done" || event.payload.stage === "error") {
        onImportDone();
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [onImportDone]);

  const handleImport = async () => {
    const files = await open({
      multiple: true,
      filters: [{ name: "PDF", extensions: ["pdf"] }],
    });
    if (files && files.length > 0) {
      const paths =
        typeof files[0] === "string"
          ? files
          : files.map((f: any) => f.path);
      setImporting(true);
      setProgress(null);
      setResultMsg(null);
      try {
        const result = await invoke<string>("import_files", { paths });
        setResultMsg(result);
        onImportDone();
      } catch (e) {
        setResultMsg(`Error: ${e}`);
      } finally {
        setImporting(false);
        setProgress(null);
        setTimeout(() => setResultMsg(null), 5000);
      }
    }
  };

  const pct =
    progress && progress.file_total > 0
      ? Math.round((progress.file_index / progress.file_total) * 100)
      : 0;

  return (
    <header className="border-b border-gray-200 dark:border-gray-800 bg-gray-50 dark:bg-gray-900">
      <div className="flex items-center justify-between px-4 py-2">
        <div className="flex items-center gap-3">
          <h1 className="text-lg font-semibold text-gray-900 dark:text-gray-100">
            Nabla PDF
          </h1>
          <span className="text-xs text-gray-400">RAG</span>
        </div>

        <div className="flex items-center gap-2">
          <button
            onClick={handleImport}
            disabled={importing}
            className="px-3 py-1.5 text-sm bg-blue-600 text-white rounded-md hover:bg-blue-700 disabled:opacity-50 transition-colors"
          >
            {importing ? "Importing..." : "+ Import"}
          </button>
          <button
            onClick={onSettingsClick}
            className="px-2 py-1.5 text-sm text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200 hover:bg-gray-200 dark:hover:bg-gray-800 rounded-md transition-colors"
            title="Settings"
          >
            Settings
          </button>
        </div>
      </div>

      {/* Progress bar */}
      {importing && progress && (
        <div className="px-4 pb-2">
          <div className="flex items-center justify-between text-xs text-gray-500 mb-1">
            <span>
              {progress.file_index}/{progress.file_total}{" "}
              <span className="text-gray-400">—</span>{" "}
              {stageLabels[progress.stage] || progress.stage}:{" "}
              <span className="text-gray-700 dark:text-gray-300">
                {progress.file_name}
              </span>
            </span>
            <span>{pct}%</span>
          </div>
          <div className="w-full h-1.5 bg-gray-200 dark:bg-gray-800 rounded-full overflow-hidden">
            <div
              className="h-full bg-blue-500 rounded-full transition-all duration-300"
              style={{ width: `${pct}%` }}
            />
          </div>
          <p className="text-xs text-gray-400 mt-1">{progress.message}</p>
        </div>
      )}

      {/* Result message */}
      {resultMsg && !importing && (
        <div className="px-4 pb-2">
          <p className="text-xs text-green-600 dark:text-green-400">
            {resultMsg}
          </p>
        </div>
      )}
    </header>
  );
}
