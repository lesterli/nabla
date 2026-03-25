import { useState, useEffect } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface TopBarProps {
  onImportDone: () => void;
}

interface ImportProgress {
  file_name: string;
  stage: string;
  message: string;
}

export function TopBar({ onImportDone }: TopBarProps) {
  const [importing, setImporting] = useState(false);
  const [progress, setProgress] = useState<string | null>(null);

  useEffect(() => {
    const unlisten = listen<ImportProgress>("import-progress", (event) => {
      const p = event.payload;
      setProgress(`${p.file_name}: ${p.message}`);
      if (p.stage === "done" || p.stage === "error") {
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
      setProgress("Starting import...");
      try {
        const result = await invoke<string>("import_files", { paths });
        setProgress(result);
        onImportDone();
      } catch (e) {
        setProgress(`Error: ${e}`);
      } finally {
        setImporting(false);
        setTimeout(() => setProgress(null), 3000);
      }
    }
  };

  return (
    <header className="flex items-center justify-between px-4 py-2 border-b border-gray-200 dark:border-gray-800 bg-gray-50 dark:bg-gray-900">
      <div className="flex items-center gap-3">
        <h1 className="text-lg font-semibold text-gray-900 dark:text-gray-100">
          Nabla PDF
        </h1>
        <span className="text-xs text-gray-400">RAG</span>
        {progress && (
          <span className="text-xs text-blue-500 ml-2 animate-pulse">
            {progress}
          </span>
        )}
      </div>

      <button
        onClick={handleImport}
        disabled={importing}
        className="px-3 py-1.5 text-sm bg-blue-600 text-white rounded-md hover:bg-blue-700 disabled:opacity-50 transition-colors"
      >
        {importing ? "Importing..." : "+ Import"}
      </button>
    </header>
  );
}
