import type {
  PaperRecord,
  ProjectBrief,
  RunManifest,
  ScreeningDecision,
  TopicCandidate,
  WorkflowOutput,
} from "./types";

const BASE = "/api";

async function json<T>(res: Response): Promise<T> {
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`${res.status}: ${text}`);
  }
  return res.json();
}

export async function createRun(brief: ProjectBrief): Promise<WorkflowOutput> {
  const res = await fetch(`${BASE}/runs`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(brief),
  });
  return json(res);
}

export async function getRun(runId: string): Promise<RunManifest> {
  return json(await fetch(`${BASE}/runs/${runId}`));
}

export async function listRuns(projectId: string): Promise<RunManifest[]> {
  return json(await fetch(`${BASE}/projects/${projectId}/runs`));
}

export async function listPapers(projectId: string): Promise<PaperRecord[]> {
  return json(await fetch(`${BASE}/projects/${projectId}/papers`));
}

export async function listScreening(
  projectId: string,
): Promise<ScreeningDecision[]> {
  return json(await fetch(`${BASE}/projects/${projectId}/screening`));
}

export async function updateScreening(
  projectId: string,
  decisions: ScreeningDecision[],
): Promise<void> {
  const res = await fetch(`${BASE}/projects/${projectId}/screening`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ decisions }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`${res.status}: ${text}`);
  }
}

export async function listTopics(
  projectId: string,
): Promise<TopicCandidate[]> {
  return json(await fetch(`${BASE}/projects/${projectId}/topics`));
}

export async function rerunPropose(
  projectId: string,
): Promise<WorkflowOutput> {
  const res = await fetch(`${BASE}/projects/${projectId}/rerun`, {
    method: "POST",
  });
  return json(res);
}
