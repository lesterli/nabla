import type { DocumentInfo } from "../App";

interface DocPreviewProps {
  documents: DocumentInfo[];
  summaries: Record<string, string>;
}

export function DocPreview({ documents, summaries }: DocPreviewProps) {
  if (documents.length === 0) return null;

  return (
    <div className="border-b border-gray-200 dark:border-gray-800 bg-gray-50 dark:bg-gray-900 px-6 py-4 overflow-y-auto max-h-64">
      <h3 className="text-xs font-medium text-gray-400 uppercase tracking-wider mb-3">
        {documents.length === 1 ? "Document" : `${documents.length} Documents`} Selected
      </h3>

      <div className="space-y-3">
        {documents.map((doc) => (
          <div key={doc.id} className="flex gap-3">
            <div className="shrink-0 w-10 h-10 bg-blue-100 dark:bg-blue-900 rounded-lg flex items-center justify-center text-blue-600 dark:text-blue-400 text-lg">
              📄
            </div>
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2">
                <span className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">
                  {doc.title || doc.file_name}
                </span>
                {doc.page_count && (
                  <span className="text-xs text-gray-400 shrink-0">
                    {doc.page_count} pages
                  </span>
                )}
              </div>
              {summaries[doc.id] ? (
                <p className="text-sm text-gray-600 dark:text-gray-400 mt-1 line-clamp-3">
                  {summaries[doc.id]}
                </p>
              ) : (
                <p className="text-xs text-gray-400 mt-1 italic">
                  Loading summary...
                </p>
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
