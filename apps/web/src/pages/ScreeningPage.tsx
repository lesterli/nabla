import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { listScreening, updateScreening, rerunPropose } from "../api";
import type { ScreeningDecision, ScreeningLabel } from "../types";

function useProjectId(): string {
  return localStorage.getItem("nabla_project_id") ?? "";
}

const LABELS: ScreeningLabel[] = ["Include", "Maybe", "Exclude"];

export default function ScreeningPage() {
  const projectId = useProjectId();
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const { data: decisions, isLoading, error } = useQuery({
    queryKey: ["screening", projectId],
    queryFn: () => listScreening(projectId),
    enabled: !!projectId,
  });

  const [edits, setEdits] = useState<Map<string, ScreeningLabel>>(new Map());

  const saveMutation = useMutation({
    mutationFn: async () => {
      if (!decisions) return;
      const modified = decisions
        .filter((d) => edits.has(paperKey(d)))
        .map((d) => ({ ...d, label: edits.get(paperKey(d))! }));
      if (modified.length > 0) {
        await updateScreening(projectId, modified);
      }
    },
    onSuccess: () => {
      setEdits(new Map());
      queryClient.invalidateQueries({ queryKey: ["screening", projectId] });
    },
  });

  const rerunMutation = useMutation({
    mutationFn: () => rerunPropose(projectId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["topics", projectId] });
      navigate("/topics");
    },
  });

  if (!projectId) return <p className="text-gray-500">No project selected.</p>;
  if (isLoading) return <p className="text-gray-500">Loading screening...</p>;
  if (error) return <p className="text-red-600">{(error as Error).message}</p>;
  if (!decisions?.length) return <p className="text-gray-500">No screening decisions yet.</p>;

  function labelFor(d: ScreeningDecision): ScreeningLabel {
    return edits.get(paperKey(d)) ?? d.label;
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-xl font-semibold">Screening Decisions</h1>
        <div className="flex gap-2">
          <button
            onClick={() => saveMutation.mutate()}
            disabled={edits.size === 0 || saveMutation.isPending}
            className="rounded bg-gray-900 px-3 py-1.5 text-sm text-white hover:bg-gray-700 disabled:opacity-50"
          >
            {saveMutation.isPending ? "Saving..." : `Save (${edits.size})`}
          </button>
          <button
            onClick={() => rerunMutation.mutate()}
            disabled={rerunMutation.isPending}
            className="rounded border border-gray-300 px-3 py-1.5 text-sm hover:bg-gray-100 disabled:opacity-50"
          >
            {rerunMutation.isPending ? "Running..." : "Rerun Propose"}
          </button>
        </div>
      </div>

      {(saveMutation.isError || rerunMutation.isError) && (
        <p className="text-sm text-red-600">
          {((saveMutation.error ?? rerunMutation.error) as Error).message}
        </p>
      )}

      <table className="w-full text-sm border-collapse">
        <thead>
          <tr className="border-b text-left text-gray-500">
            <th className="py-2 pr-4">Paper ID</th>
            <th className="py-2 pr-4">Label</th>
            <th className="py-2 pr-4">Rationale</th>
            <th className="py-2 pr-4">Tags</th>
            <th className="py-2 pr-4">Confidence</th>
          </tr>
        </thead>
        <tbody>
          {decisions.map((d) => (
            <tr key={paperKey(d)} className="border-b hover:bg-gray-50">
              <td className="py-2 pr-4 font-mono text-xs text-gray-600">
                {d.paper_id.value.slice(0, 20)}
              </td>
              <td className="py-2 pr-4">
                <select
                  value={labelFor(d)}
                  onChange={(e) => {
                    const next = new Map(edits);
                    const newLabel = e.target.value as ScreeningLabel;
                    if (newLabel === d.label) {
                      next.delete(paperKey(d));
                    } else {
                      next.set(paperKey(d), newLabel);
                    }
                    setEdits(next);
                  }}
                  className={`rounded border px-2 py-1 text-xs ${labelColor(labelFor(d))}`}
                >
                  {LABELS.map((l) => (
                    <option key={l} value={l}>{l}</option>
                  ))}
                </select>
              </td>
              <td className="py-2 pr-4 text-gray-600">{d.rationale}</td>
              <td className="py-2 pr-4">
                {d.tags.map((t) => (
                  <span key={t} className="inline-block bg-gray-100 rounded px-1.5 py-0.5 text-xs mr-1">
                    {t}
                  </span>
                ))}
              </td>
              <td className="py-2 pr-4 text-gray-600">
                {d.confidence != null ? d.confidence.toFixed(2) : "\u2014"}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function paperKey(d: ScreeningDecision): string {
  return `${d.paper_id.kind}:${d.paper_id.value}`;
}

function labelColor(label: ScreeningLabel): string {
  switch (label) {
    case "Include": return "border-green-300 bg-green-50 text-green-800";
    case "Maybe": return "border-yellow-300 bg-yellow-50 text-yellow-800";
    case "Exclude": return "border-red-300 bg-red-50 text-red-800";
  }
}
