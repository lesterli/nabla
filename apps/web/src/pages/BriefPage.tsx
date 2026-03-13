import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { createRun, getRun } from "../api";
import type { ProjectBrief, RunManifest } from "../types";

function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 48);
}

const PHASES = [
  { key: "collect", label: "Collecting papers" },
  { key: "screen", label: "Screening papers" },
  { key: "propose", label: "Generating topics" },
] as const;

export default function BriefPage() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [goal, setGoal] = useState("");
  const [constraints, setConstraints] = useState<string[]>([""]);
  const [keywords, setKeywords] = useState<string[]>([""]);
  const [quickMode, setQuickMode] = useState(
    () => localStorage.getItem("nabla_quick_mode") === "1",
  );
  const [activeRunId, setActiveRunId] = useState(
    () => localStorage.getItem("nabla_run_id") ?? "",
  );

  const isRunning = !!activeRunId;

  const mutation = useMutation({
    mutationFn: (brief: ProjectBrief) => createRun(brief),
    onSuccess: (manifest, brief) => {
      localStorage.setItem("nabla_project_id", brief.id);
      localStorage.setItem("nabla_run_id", manifest.run_id);
      localStorage.setItem("nabla_quick_mode", quickMode ? "1" : "0");
      setActiveRunId(manifest.run_id);
      queryClient.setQueryData(["run", manifest.run_id], manifest);
    },
  });

  const { data: activeRun } = useQuery({
    queryKey: ["run", activeRunId],
    queryFn: () => getRun(activeRunId),
    enabled: !!activeRunId,
    refetchInterval: (query) => {
      const run = query.state.data as RunManifest | undefined;
      if (!run) return 1000;
      return run.status === "pending" || run.status === "running" ? 1000 : false;
    },
  });

  useEffect(() => {
    if (!activeRun) return;
    if (activeRun.status === "completed") {
      localStorage.removeItem("nabla_run_id");
      setActiveRunId("");
      navigate(quickMode ? "/topics" : "/screening");
    }
    if (activeRun.status === "failed") {
      localStorage.removeItem("nabla_run_id");
    }
  }, [activeRun, navigate, quickMode]);

  const phaseIndex = useMemo(() => {
    if (!activeRun) return -1;
    const idx = PHASES.findIndex((p) => p.key === activeRun.phase);
    // frame/done map outside PHASES — treat frame as before collect, done as all complete
    if (activeRun.phase === "frame") return -1;
    if (activeRun.phase === "done") return PHASES.length;
    return idx;
  }, [activeRun]);

  function submit(e: React.FormEvent) {
    e.preventDefault();
    const brief: ProjectBrief = {
      id: slugify(goal) || "project",
      goal,
      constraints: constraints.filter(Boolean),
      keywords: keywords.filter(Boolean),
      date_range: null,
    };
    mutation.mutate(brief);
  }

  return (
    <form onSubmit={submit} className="space-y-6 max-w-xl">
      <h1 className="text-xl font-semibold">Project Brief</h1>

      <label className="block">
        <span className="text-sm font-medium">Goal</span>
        <input
          className="mt-1 block w-full rounded border border-gray-300 px-3 py-2 text-sm"
          value={goal}
          onChange={(e) => setGoal(e.target.value)}
          placeholder="e.g. neural operator methods for PDE discovery"
          required
        />
      </label>

      <fieldset className="space-y-2">
        <legend className="text-sm font-medium">Constraints</legend>
        {constraints.map((c, i) => (
          <div key={i} className="flex gap-2">
            <input
              className="block w-full rounded border border-gray-300 px-3 py-2 text-sm"
              value={c}
              onChange={(e) => {
                const next = [...constraints];
                next[i] = e.target.value;
                setConstraints(next);
              }}
              placeholder="e.g. focus on recent papers"
            />
            {constraints.length > 1 && (
              <button
                type="button"
                className="text-red-500 text-sm"
                onClick={() => setConstraints(constraints.filter((_, j) => j !== i))}
              >
                remove
              </button>
            )}
          </div>
        ))}
        <button
          type="button"
          className="text-sm text-blue-600"
          onClick={() => setConstraints([...constraints, ""])}
        >
          + add constraint
        </button>
      </fieldset>

      <fieldset className="space-y-2">
        <legend className="text-sm font-medium">Keywords</legend>
        {keywords.map((k, i) => (
          <div key={i} className="flex gap-2">
            <input
              className="block w-full rounded border border-gray-300 px-3 py-2 text-sm"
              value={k}
              onChange={(e) => {
                const next = [...keywords];
                next[i] = e.target.value;
                setKeywords(next);
              }}
              placeholder="e.g. neural operator"
              required={i === 0}
            />
            {keywords.length > 1 && (
              <button
                type="button"
                className="text-red-500 text-sm"
                onClick={() => setKeywords(keywords.filter((_, j) => j !== i))}
              >
                remove
              </button>
            )}
          </div>
        ))}
        <button
          type="button"
          className="text-sm text-blue-600"
          onClick={() => setKeywords([...keywords, ""])}
        >
          + add keyword
        </button>
      </fieldset>

      <label className="flex items-start gap-3 rounded border border-gray-200 bg-gray-50 px-3 py-3 text-sm">
        <input
          type="checkbox"
          className="mt-0.5"
          checked={quickMode}
          onChange={(e) => {
            setQuickMode(e.target.checked);
            localStorage.setItem("nabla_quick_mode", e.target.checked ? "1" : "0");
          }}
        />
        <span>
          <span className="block font-medium text-gray-900">Quick mode</span>
          <span className="text-gray-600">
            Skip screening and jump straight to topic recommendations.
          </span>
        </span>
      </label>

      {!isRunning && (
        <button
          type="submit"
          disabled={mutation.isPending}
          className="rounded bg-gray-900 px-4 py-2 text-sm text-white hover:bg-gray-700 disabled:opacity-50"
        >
          {mutation.isPending ? "Submitting..." : "Create Run"}
        </button>
      )}

      {mutation.isError && (
        <p className="text-sm text-red-600">{(mutation.error as Error).message}</p>
      )}

      {activeRun && activeRun.status !== "completed" && (
        <div className="rounded-lg border border-gray-200 bg-white p-4 space-y-4">
          <div className="flex items-center gap-3">
            {activeRun.status === "failed" ? (
              <div className="h-8 w-8 rounded-full bg-red-100 flex items-center justify-center text-red-600 text-sm font-bold shrink-0">!</div>
            ) : (
              <div className="h-8 w-8 rounded-full border-2 border-blue-500 border-t-transparent animate-spin shrink-0" />
            )}
            <div>
              <p className="text-sm font-medium text-gray-900">
                {activeRun.status === "failed" ? "Run failed" : "Running..."}
              </p>
              <p className="text-xs text-gray-500">
                {activeRun.status === "failed"
                  ? "Check server logs or retry with a narrower brief."
                  : "This may take a few minutes with LLM adapters."}
              </p>
            </div>
          </div>

          <div className="flex gap-1">
            {PHASES.map((phase, index) => {
              const state = stepState(activeRun, index, phaseIndex);
              return (
                <div key={phase.key} className="flex-1 space-y-1">
                  <div
                    className={`h-1.5 rounded-full ${
                      state === "done"
                        ? "bg-green-500"
                        : state === "current"
                          ? "bg-blue-500 animate-pulse"
                          : "bg-gray-200"
                    }`}
                  />
                  <p className={`text-xs ${state === "upcoming" ? "text-gray-400" : "text-gray-600"}`}>
                    {phase.label}
                  </p>
                </div>
              );
            })}
          </div>

          {activeRun.status === "failed" && (
            <button
              type="button"
              onClick={() => {
                localStorage.removeItem("nabla_run_id");
                setActiveRunId("");
              }}
              className="text-sm text-blue-600 hover:underline"
            >
              Dismiss and retry
            </button>
          )}
        </div>
      )}
    </form>
  );
}

function stepState(
  run: RunManifest,
  index: number,
  currentIndex: number,
): "done" | "current" | "upcoming" {
  if (run.status === "completed" || run.phase === "done") return "done";
  if (index < currentIndex) return "done";
  if (index === currentIndex) return "current";
  return "upcoming";
}
