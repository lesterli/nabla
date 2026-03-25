import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Sidebar } from "./components/Sidebar";
import { AskPanel } from "./components/AskPanel";
import { TopBar } from "./components/TopBar";

interface DocumentInfo {
  id: string;
  file_name: string;
  state: string;
  page_count: number | null;
  title: string | null;
}

function App() {
  const [documents, setDocuments] = useState<DocumentInfo[]>([]);
  const [selectedDocIds, setSelectedDocIds] = useState<string[]>([]);

  const refreshDocuments = async () => {
    try {
      const docs = await invoke<DocumentInfo[]>("list_documents");
      setDocuments(docs);
    } catch (e) {
      console.error("Failed to load documents:", e);
    }
  };

  useEffect(() => {
    refreshDocuments();
  }, []);

  const handleDocSelect = (docId: string) => {
    setSelectedDocIds((prev) =>
      prev.includes(docId)
        ? prev.filter((id) => id !== docId)
        : [...prev, docId]
    );
  };

  const handleSelectAll = () => {
    setSelectedDocIds([]);
  };

  return (
    <div className="flex flex-col h-screen bg-white dark:bg-gray-950">
      <TopBar onImportDone={refreshDocuments} />

      <div className="flex flex-1 overflow-hidden">
        <Sidebar
          documents={documents}
          selectedDocIds={selectedDocIds}
          onDocSelect={handleDocSelect}
          onSelectAll={handleSelectAll}
        />

        <main className="flex-1 flex flex-col overflow-hidden">
          <AskPanel
            selectedDocIds={selectedDocIds}
            documents={documents}
          />
        </main>
      </div>
    </div>
  );
}

export default App;
