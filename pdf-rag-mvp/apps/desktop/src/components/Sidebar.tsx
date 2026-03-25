import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";

interface DocumentInfo {
  id: string;
  file_name: string;
  state: string;
  page_count: number | null;
  title: string | null;
}

interface SidebarProps {
  documents: DocumentInfo[];
  selectedDocIds: string[];
  onDocSelect: (docId: string, multiSelect: boolean) => void;
  onSelectAll: () => void;
  onDocDeleted?: () => void;
}

const stateColor: Record<string, string> = {
  Ready: "bg-green-500",
  Queued: "bg-gray-400",
  Extracting: "bg-yellow-500",
  Chunking: "bg-yellow-500",
  Summarizing: "bg-yellow-500",
  Embedding: "bg-blue-500",
  Failed: "bg-red-500",
};

interface ContextMenu {
  x: number;
  y: number;
  docId: string;
  docName: string;
}

export function Sidebar({
  documents,
  selectedDocIds,
  onDocSelect,
  onSelectAll,
  onDocDeleted,
}: SidebarProps) {
  const allSelected = selectedDocIds.length === 0;
  const [contextMenu, setContextMenu] = useState<ContextMenu | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  // Close menu on outside click
  useEffect(() => {
    const handleClick = () => setContextMenu(null);
    document.addEventListener("click", handleClick);
    return () => document.removeEventListener("click", handleClick);
  }, []);

  const handleContextMenu = (e: React.MouseEvent, doc: DocumentInfo) => {
    e.preventDefault();
    setContextMenu({
      x: e.clientX,
      y: e.clientY,
      docId: doc.id,
      docName: doc.title || doc.file_name,
    });
  };

  const handleDelete = async () => {
    if (!contextMenu) return;
    const confirmed = window.confirm(
      `Delete "${contextMenu.docName}"?\n\nThis removes the document and all its chunks from the database.`
    );
    if (!confirmed) return;

    try {
      await invoke("delete_document", { docId: contextMenu.docId });
      onDocDeleted?.();
    } catch (e) {
      console.error("Delete failed:", e);
    }
    setContextMenu(null);
  };

  return (
    <aside className="w-64 border-r border-gray-200 dark:border-gray-800 flex flex-col bg-gray-50 dark:bg-gray-900 overflow-hidden relative">
      <div className="px-3 py-2 border-b border-gray-200 dark:border-gray-800">
        <button
          onClick={onSelectAll}
          className={`w-full text-left text-sm px-2 py-1 rounded ${
            allSelected
              ? "bg-blue-100 dark:bg-blue-900 text-blue-700 dark:text-blue-300"
              : "text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-800"
          }`}
        >
          All documents ({documents.length})
        </button>
      </div>

      <div className="flex-1 overflow-y-auto">
        {documents.map((doc) => {
          const isSelected = selectedDocIds.includes(doc.id);
          return (
            <button
              key={doc.id}
              onClick={(e) => onDocSelect(doc.id, e.metaKey || e.ctrlKey)}
              onContextMenu={(e) => handleContextMenu(e, doc)}
              className={`w-full text-left px-3 py-2 border-b border-gray-100 dark:border-gray-800 hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors ${
                isSelected ? "bg-blue-50 dark:bg-blue-950" : ""
              }`}
            >
              <div className="flex items-center gap-2">
                <span
                  className={`w-2 h-2 rounded-full shrink-0 ${
                    stateColor[doc.state] || "bg-gray-400"
                  }`}
                />
                <span className="text-sm text-gray-800 dark:text-gray-200 truncate">
                  {doc.title || doc.file_name}
                </span>
              </div>
              <div className="flex items-center gap-2 mt-0.5 ml-4">
                <span className="text-xs text-gray-400">{doc.state}</span>
                {doc.page_count && (
                  <span className="text-xs text-gray-400">
                    {doc.page_count}p
                  </span>
                )}
              </div>
            </button>
          );
        })}

        {documents.length === 0 && (
          <div className="px-4 py-8 text-center text-sm text-gray-400">
            No documents yet.
            <br />
            Click "+ Import" to add PDFs.
          </div>
        )}
      </div>

      {/* Context menu */}
      {contextMenu && (
        <div
          ref={menuRef}
          className="fixed z-50 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-lg py-1 min-w-[160px]"
          style={{ left: contextMenu.x, top: contextMenu.y }}
        >
          <button
            onClick={handleDelete}
            className="w-full text-left px-3 py-2 text-sm text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-950 transition-colors"
          >
            Delete document
          </button>
        </div>
      )}
    </aside>
  );
}
