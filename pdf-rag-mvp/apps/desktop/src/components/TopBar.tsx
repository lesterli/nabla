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

// Each stage gets a weight within a single file's progress
const stageWeight: Record<string, number> = {
  parse: 0.15,
  chunk: 0.50,
  embed: 0.80,
  done: 1.0,
  error: 1.0,
  skip: 1.0,
};

const stageLabels: Record<string, string> = {
  parse: "Parsing",
  chunk: "Summarizing",
  embed: "Embedding",
  done: "Ready",
  error: "Failed",
  skip: "Skipped",
};

export function TopBar({ onImportDone, onSettingsClick }: TopBarProps) {
  const [importing, setImporting] = useState(false);
  const [progress, setProgress] = useState<ImportProgress | null>(null);
  const [errors, setErrors] = useState<string[]>([]);
  const [resultMsg, setResultMsg] = useState<string | null>(null);

  useEffect(() => {
    const unlisten = listen<ImportProgress>("import-progress", (event) => {
      const p = event.payload;
      setProgress(p);
      if (p.stage === "error") {
        setErrors((prev) => [...prev, `${p.file_name}: ${p.message}`]);
      }
      if (p.stage === "done" || p.stage === "error") {
        onImportDone();
      }
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [onImportDone]);

  const handleImport = async (folder = false) => {
    let paths: string[] = [];

    if (folder) {
      // Select a folder — backend will recursively find PDFs
      const dir = await open({ directory: true });
      if (dir) {
        paths = [typeof dir === "string" ? dir : (dir as any).path];
      }
    } else {
      // Select individual PDF files
      const files = await open({
        multiple: true,
        filters: [{ name: "PDF", extensions: ["pdf"] }],
      });
      if (files && files.length > 0) {
        paths =
          typeof files[0] === "string"
            ? (files as string[])
            : files.map((f: any) => f.path);
      }
    }

    if (paths.length > 0) {
      setImporting(true);
      setProgress(null);
      setErrors([]);
      setResultMsg(null);
      try {
        const result = await invoke<string>("import_files", { paths });
        setResultMsg(result);
        onImportDone();
      } catch (e) {
        setErrors((prev) => [...prev, `Import error: ${e}`]);
      } finally {
        setImporting(false);
        setProgress(null);
      }
    }
  };

  // Calculate overall progress: completed files + current file's stage progress
  const calcPercent = () => {
    if (!progress || progress.file_total === 0) return 0;
    const completedFiles = progress.file_index - 1;
    const currentStageWeight = stageWeight[progress.stage] ?? 0.5;
    return Math.round(
      ((completedFiles + currentStageWeight) / progress.file_total) * 100
    );
  };

  const pct = calcPercent();

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
          <div className="flex items-center">
            <button
              onClick={() => handleImport(false)}
              disabled={importing}
              className="px-3 py-1.5 text-sm bg-blue-600 text-white rounded-l-md hover:bg-blue-700 disabled:opacity-50 transition-colors"
            >
              {importing ? "Importing..." : "+ Files"}
            </button>
            <button
              onClick={() => handleImport(true)}
              disabled={importing}
              className="px-2 py-1.5 text-sm bg-blue-700 text-white rounded-r-md hover:bg-blue-800 disabled:opacity-50 transition-colors border-l border-blue-500"
              title="Import all PDFs from a folder"
            >
              Folder
            </button>
          </div>
          <button
            onClick={onSettingsClick}
            className="px-2 py-1.5 text-sm text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200 hover:bg-gray-200 dark:hover:bg-gray-800 rounded-md transition-colors"
            title="Settings"
          >
            Settings
          </button>
        </div>
      </div>

      {/* Progress bar during import */}
      {importing && progress && (
        <div className="px-4 pb-2">
          <div className="flex items-center justify-between text-xs text-gray-500 mb-1">
            <span>
              {progress.file_index}/{progress.file_total}{" "}
              <span className="text-gray-400">—</span>{" "}
              {stageLabels[progress.stage] || progress.stage}:{" "}
              <span className="text-gray-700 dark:text-gray-300 truncate">
                {progress.file_name}
              </span>
            </span>
            <span>{pct}%</span>
          </div>
          <div className="w-full h-1.5 bg-gray-200 dark:bg-gray-800 rounded-full overflow-hidden">
            <div
              className="h-full bg-blue-500 rounded-full transition-all duration-500 ease-out"
              style={{ width: `${pct}%` }}
            />
          </div>
        </div>
      )}

      {/* Result + errors after import */}
      {!importing && (resultMsg || errors.length > 0) && (
        <div className="px-4 pb-2 space-y-1">
          {resultMsg && (
            <p className="text-xs text-green-600 dark:text-green-400">
              {resultMsg}
            </p>
          )}
          {errors.map((err, i) => (
            <p key={i} className="text-xs text-red-500">
              {err}
            </p>
          ))}
          <button
            onClick={() => {
              setResultMsg(null);
              setErrors([]);
            }}
            className="text-xs text-gray-400 hover:text-gray-600"
          >
            dismiss
          </button>
        </div>
      )}
    </header>
  );
}
