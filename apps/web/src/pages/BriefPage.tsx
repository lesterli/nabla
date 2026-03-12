import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { createRun } from "../api";
import type { ProjectBrief } from "../types";

function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "")
    .slice(0, 48);
}

export default function BriefPage() {
  const navigate = useNavigate();
  const [goal, setGoal] = useState("");
  const [constraints, setConstraints] = useState<string[]>([""]);
  const [keywords, setKeywords] = useState<string[]>([""]);

  const mutation = useMutation({
    mutationFn: (brief: ProjectBrief) => createRun(brief),
    onSuccess: () => navigate("/papers"),
  });

  function submit(e: React.FormEvent) {
    e.preventDefault();
    const brief: ProjectBrief = {
      id: slugify(goal) || "project",
      goal,
      constraints: constraints.filter(Boolean),
      keywords: keywords.filter(Boolean),
      date_range: null,
    };
    localStorage.setItem("nabla_project_id", brief.id);
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

      <button
        type="submit"
        disabled={mutation.isPending}
        className="rounded bg-gray-900 px-4 py-2 text-sm text-white hover:bg-gray-700 disabled:opacity-50"
      >
        {mutation.isPending ? "Running..." : "Create Run"}
      </button>

      {mutation.isError && (
        <p className="text-sm text-red-600">{(mutation.error as Error).message}</p>
      )}
    </form>
  );
}
