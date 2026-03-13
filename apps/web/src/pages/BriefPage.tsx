import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { createRun, getRun, listTopics } from "../api";
import type { ProjectBrief, RunManifest, TopicCandidate } from "../types";

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
  const [manualScreening, setManualScreening] = useState(
    () => localStorage.getItem("nabla_manual_screening") === "1",
  );
  const [activeRunId, setActiveRunId] = useState(
    () => localStorage.getItem("nabla_run_id") ?? "",
  );
  const [completedProjectId, setCompletedProjectId] = useState(
    () => localStorage.getItem("nabla_completed_project") ?? "",
  );

  const isRunning = !!activeRunId;
  const isCompleted = !!completedProjectId;

  const mutation = useMutation({
    mutationFn: (brief: ProjectBrief) => createRun(brief),
    onSuccess: (manifest, brief) => {
      localStorage.setItem("nabla_project_id", brief.id);
      localStorage.setItem("nabla_run_id", manifest.run_id);
      localStorage.setItem("nabla_manual_screening", manualScreening ? "1" : "0");
      setActiveRunId(manifest.run_id);
      setCompletedProjectId("");
      localStorage.removeItem("nabla_completed_project");
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
      if (manualScreening) {
        navigate("/screening");
      } else {
        // Show inline topic summary
        const pid = localStorage.getItem("nabla_project_id") ?? "";
        setCompletedProjectId(pid);
        localStorage.setItem("nabla_completed_project", pid);
      }
    }
    if (activeRun.status === "failed") {
      localStorage.removeItem("nabla_run_id");
    }
  }, [activeRun, navigate, manualScreening]);

  const { data: completedTopics } = useQuery({
    queryKey: ["topics", completedProjectId],
    queryFn: () => listTopics(completedProjectId),
    enabled: !!completedProjectId,
  });

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
    const words = goal.trim().split(/\s+/).filter(Boolean);
    const brief: ProjectBrief = {
      id: slugify(goal) || "project",
      goal,
      constraints: [],
      keywords: words,
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
          onChange={(e) => {
            setGoal(e.target.value);
            if (completedProjectId) {
              setCompletedProjectId("");
              localStorage.removeItem("nabla_completed_project");
            }
          }}
          placeholder="e.g. neural operator methods for PDE discovery"
          required
        />
      </label>

      {/* Constraints and Keywords fields hidden for simplicity — keywords auto-extracted from goal */}

      <label className="flex items-start gap-3 rounded border border-gray-200 bg-gray-50 px-3 py-3 text-sm">
        <input
          type="checkbox"
          className="mt-0.5"
          checked={manualScreening}
          onChange={(e) => {
            setManualScreening(e.target.checked);
            localStorage.setItem("nabla_manual_screening", e.target.checked ? "1" : "0");
          }}
        />
        <span>
          <span className="block font-medium text-gray-900">Manual screening</span>
          <span className="text-gray-600">
            Review and edit paper screening before generating topics.
          </span>
        </span>
      </label>

      {!isRunning && !isCompleted && (
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

          <ProgressBar phaseIndex={phaseIndex} />

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

      {isCompleted && (
        <div className="rounded-lg border border-gray-200 bg-white p-4 space-y-4">
          <div className="flex items-center gap-3">
            <div className="h-8 w-8 rounded-full bg-green-100 flex items-center justify-center text-green-600 text-sm font-bold shrink-0">&#10003;</div>
            <p className="text-sm font-medium text-gray-900">Completed</p>
          </div>
          <ProgressBar phaseIndex={PHASES.length} />
        </div>
      )}
      {completedTopics && completedTopics.length > 0 && (
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <h2 className="text-sm font-semibold text-gray-900">
              Topic Candidates ({completedTopics.length})
            </h2>
            <button
              type="button"
              onClick={() => navigate("/topics")}
              className="text-sm text-blue-600 hover:underline"
            >
              View details
            </button>
          </div>
          {completedTopics.map((topic) => (
            <TopicSummaryCard key={topic.id} topic={topic} />
          ))}
        </div>
      )}
    </form>
  );
}

function ProgressBar({ phaseIndex }: { phaseIndex: number }) {
  return (
    <div className="flex gap-1">
      {PHASES.map((phase, index) => {
        const done = index < phaseIndex;
        const current = index === phaseIndex;
        return (
          <div key={phase.key} className="flex-1 space-y-1">
            <div
              className={`h-1.5 rounded-full ${
                done
                  ? "bg-green-500"
                  : current
                    ? "bg-blue-500 animate-pulse"
                    : "bg-gray-200"
              }`}
            />
            <p className={`text-xs ${!done && !current ? "text-gray-400" : "text-gray-600"}`}>
              {phase.label}
            </p>
          </div>
        );
      })}
    </div>
  );
}

function TopicSummaryCard({ topic }: { topic: TopicCandidate }) {
  return (
    <div className="rounded border border-gray-200 bg-white px-4 py-3 space-y-1">
      <p className="text-sm font-medium text-gray-900">{topic.title}</p>
      <p className="text-xs text-gray-500 line-clamp-2">{topic.why_now}</p>
    </div>
  );
}

