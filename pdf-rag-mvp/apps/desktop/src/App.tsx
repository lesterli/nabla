import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Sidebar } from "./components/Sidebar";
import { AskPanel } from "./components/AskPanel";
import { DocPreview } from "./components/DocPreview";
import { TopBar } from "./components/TopBar";

export interface DocumentInfo {
  id: string;
  file_name: string;
  state: string;
  page_count: number | null;
  title: string | null;
}

function App() {
  const [documents, setDocuments] = useState<DocumentInfo[]>([]);
  const [selectedDocIds, setSelectedDocIds] = useState<string[]>([]);
  const [docSummaries, setDocSummaries] = useState<Record<string, string>>({});

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

  // Fetch summaries when selection changes
  useEffect(() => {
    if (selectedDocIds.length === 0) return;

    const fetchSummaries = async () => {
      try {
        const summaries = await invoke<string[]>("get_document_summaries", {
          docIds: selectedDocIds,
        });
        const map: Record<string, string> = {};
        selectedDocIds.forEach((id, i) => {
          if (summaries[i]) map[id] = summaries[i];
        });
        setDocSummaries(map);
      } catch (e) {
        console.error("Failed to load summaries:", e);
      }
    };
    fetchSummaries();
  }, [selectedDocIds]);

  const handleDocSelect = (docId: string) => {
    setSelectedDocIds((prev) =>
      prev.includes(docId)
        ? prev.filter((id) => id !== docId)
        : [...prev, docId]
    );
  };

  const handleSelectAll = () => {
    setSelectedDocIds([]);
    setDocSummaries({});
  };

  const selectedDocs = documents.filter((d) => selectedDocIds.includes(d.id));

  return (
    <div className="flex flex-col h-screen bg-white dark:bg-gray-950">
      <TopBar onImportDone={refreshDocuments} />

      <div className="flex flex-1 overflow-hidden">
        <Sidebar
          documents={documents}
          selectedDocIds={selectedDocIds}
          onDocSelect={handleDocSelect}
          onSelectAll={handleSelectAll}
          onDocDeleted={refreshDocuments}
        />

        <main className="flex-1 flex flex-col overflow-hidden">
          {selectedDocIds.length > 0 && (
            <DocPreview
              documents={selectedDocs}
              summaries={docSummaries}
            />
          )}
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
