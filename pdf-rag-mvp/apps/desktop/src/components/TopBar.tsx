import { open } from "@tauri-apps/plugin-dialog";
import { invoke } from "@tauri-apps/api/core";

interface TopBarProps {
  onImportDone: () => void;
}

export function TopBar({ onImportDone }: TopBarProps) {
  const handleImport = async () => {
    const files = await open({
      multiple: true,
      filters: [{ name: "PDF", extensions: ["pdf"] }],
    });
    if (files && files.length > 0) {
      const paths = typeof files[0] === "string" ? files : files.map((f: any) => f.path);
      try {
        await invoke("import_files", { paths });
        onImportDone();
      } catch (e) {
        console.error("Import failed:", e);
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
      </div>

      <button
        onClick={handleImport}
        className="px-3 py-1.5 text-sm bg-blue-600 text-white rounded-md hover:bg-blue-700 transition-colors"
      >
        + Import
      </button>
    </header>
  );
}
