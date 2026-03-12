import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { listPapers } from "../api";
import type { PaperRecord } from "../types";

function paperIdLabel(paper: PaperRecord): string {
  return `${paper.paper_id.kind}:${paper.paper_id.value}`;
}

function useProjectId(): string {
  return localStorage.getItem("nabla_project_id") ?? "";
}

export default function PapersPage() {
  const projectId = useProjectId();
  const { data: papers, isLoading, error } = useQuery({
    queryKey: ["papers", projectId],
    queryFn: () => listPapers(projectId),
    enabled: !!projectId,
  });

  if (!projectId) return <p className="text-gray-500">No project selected. Go to Brief first.</p>;
  if (isLoading) return <p className="text-gray-500">Loading papers...</p>;
  if (error) return <p className="text-red-600">{(error as Error).message}</p>;
  if (!papers?.length) return <p className="text-gray-500">No papers collected yet.</p>;

  return (
    <div className="space-y-4">
      <h1 className="text-xl font-semibold">
        Paper Set <span className="text-gray-400 font-normal text-base">({papers.length} papers)</span>
      </h1>
      <table className="w-full text-sm border-collapse">
        <thead>
          <tr className="border-b text-left text-gray-500">
            <th className="py-2 pr-4">Title</th>
            <th className="py-2 pr-4">Authors</th>
            <th className="py-2 pr-4">Year</th>
            <th className="py-2 pr-4">Source</th>
          </tr>
        </thead>
        <tbody>
          {papers.map((paper) => (
            <PaperRow key={paperIdLabel(paper)} paper={paper} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

function PaperRow({ paper }: { paper: PaperRecord }) {
  const [expanded, setExpanded] = useState(false);

  return (
    <>
      <tr className="border-b hover:bg-gray-50">
        <td className="py-2 pr-4">
          <div className="flex items-start gap-1">
            {paper.source_url ? (
              <a
                href={paper.source_url}
                target="_blank"
                rel="noopener noreferrer"
                className="text-blue-600 hover:underline"
              >
                {paper.title}
              </a>
            ) : (
              <span>{paper.title}</span>
            )}
            {paper.abstract_text && (
              <button
                onClick={() => setExpanded(!expanded)}
                className="text-gray-400 hover:text-gray-600 shrink-0 ml-1"
                title="Toggle abstract"
              >
                {expanded ? "\u25B2" : "\u25BC"}
              </button>
            )}
          </div>
        </td>
        <td className="py-2 pr-4 text-gray-600">{paper.authors.slice(0, 3).join(", ")}{paper.authors.length > 3 ? " ..." : ""}</td>
        <td className="py-2 pr-4 text-gray-600">{paper.year ?? "\u2014"}</td>
        <td className="py-2 pr-4 text-gray-600">{paper.source_name}</td>
      </tr>
      {expanded && paper.abstract_text && (
        <tr className="border-b bg-gray-50">
          <td colSpan={4} className="py-2 px-4 text-gray-600 text-xs leading-relaxed">
            {paper.abstract_text}
          </td>
        </tr>
      )}
    </>
  );
}
