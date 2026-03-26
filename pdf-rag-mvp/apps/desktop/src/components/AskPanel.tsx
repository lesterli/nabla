import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import ReactMarkdown from "react-markdown";

interface DocumentInfo {
  id: string;
  file_name: string;
  state: string;
  page_count: number | null;
  title: string | null;
}

interface SearchResult {
  chunk_id: string;
  document_id: string;
  text: string;
  score: number;
}

interface AskResponse {
  evidence: SearchResult[];
  doc_summaries: string[];
  answer: string;
}

interface AskPanelProps {
  selectedDocIds: string[];
  documents: DocumentInfo[];
}

export function AskPanel({ selectedDocIds, documents }: AskPanelProps) {
  const [query, setQuery] = useState("");
  const [loading, setLoading] = useState(false);
  const [result, setResult] = useState<AskResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleAsk = async () => {
    if (!query.trim()) return;

    setLoading(true);
    setError(null);
    setResult(null);

    try {
      const response = await invoke<AskResponse>("ask_question", {
        prompt: query,
        docIds: selectedDocIds.length > 0 ? selectedDocIds : null,
      });
      setResult(response);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const getDocName = (docId: string): string => {
    const doc = documents.find((d) => d.id === docId);
    return doc?.title || doc?.file_name || docId.slice(0, 8);
  };

  const scopeLabel =
    selectedDocIds.length === 0
      ? "all documents"
      : `${selectedDocIds.length} selected`;

  return (
    <div className="flex flex-col flex-1 min-h-0">
      {/* Results area */}
      <div className="flex-1 overflow-y-auto p-6">
        {!result && !loading && !error && (
          <div className="flex items-center justify-center h-full text-gray-400">
            <div className="text-center">
              <div className="text-4xl mb-4">📄</div>
              <p className="text-lg">Ask anything about your documents</p>
              <p className="text-sm mt-1">
                Searching across {scopeLabel}
              </p>
            </div>
          </div>
        )}

        {loading && (
          <div className="flex items-center justify-center h-full">
            <div className="text-gray-400 animate-pulse">Searching...</div>
          </div>
        )}

        {error && (
          <div className="p-4 bg-red-50 dark:bg-red-950 rounded-lg text-red-700 dark:text-red-300 text-sm">
            {error}
          </div>
        )}

        {result && (
          <div className="space-y-6">
            {/* Document summaries */}
            {result.doc_summaries.length > 0 && (
              <div>
                <h3 className="text-xs font-medium text-gray-400 uppercase tracking-wider mb-2">
                  Document Context
                </h3>
                <div className="space-y-2">
                  {result.doc_summaries.map((s, i) => (
                    <div
                      key={i}
                      className="text-sm text-gray-600 dark:text-gray-400 bg-gray-50 dark:bg-gray-900 p-3 rounded-lg"
                    >
                      {s}
                    </div>
                  ))}
                </div>
              </div>
            )}

            {/* Evidence chunks */}
            <div>
              <h3 className="text-xs font-medium text-gray-400 uppercase tracking-wider mb-2">
                Evidence ({result.evidence.length} chunks)
              </h3>
              <div className="space-y-2">
                {result.evidence.map((hit, i) => (
                  <div
                    key={hit.chunk_id}
                    className="border border-gray-200 dark:border-gray-800 rounded-lg p-3"
                  >
                    <div className="flex items-center justify-between mb-1">
                      <span className="text-xs font-medium text-blue-600 dark:text-blue-400">
                        [{i + 1}] {getDocName(hit.document_id)}
                      </span>
                      <span className="text-xs text-gray-400">
                        score: {hit.score.toFixed(2)}
                      </span>
                    </div>
                    <p className="text-sm text-gray-700 dark:text-gray-300 line-clamp-3">
                      {hit.text}
                    </p>
                  </div>
                ))}
              </div>
            </div>

            {/* Answer */}
            <div>
              <h3 className="text-xs font-medium text-gray-400 uppercase tracking-wider mb-2">
                Answer
              </h3>
              <div className="prose prose-sm dark:prose-invert max-w-none">
                <ReactMarkdown>{result.answer}</ReactMarkdown>
              </div>
            </div>
          </div>
        )}
      </div>

      {/* Input bar — always visible at bottom */}
      <div className="shrink-0 border-t border-gray-200 dark:border-gray-800 p-4 bg-white dark:bg-gray-950">
        <div className="flex gap-2">
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleAsk()}
            placeholder={`Ask about ${scopeLabel}...`}
            className="flex-1 px-3 py-2 border border-gray-300 dark:border-gray-700 rounded-lg text-sm bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500"
            disabled={loading}
          />
          <button
            onClick={handleAsk}
            disabled={loading || !query.trim()}
            className="px-4 py-2 bg-blue-600 text-white text-sm rounded-lg hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
          >
            Ask
          </button>
        </div>
        <div className="mt-1 text-xs text-gray-400">
          Scope: {scopeLabel} | Press Enter to ask
        </div>
      </div>
    </div>
  );
}
