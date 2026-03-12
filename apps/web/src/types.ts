export interface DateRange {
  start: string | null;
  end: string | null;
}

export interface ProjectBrief {
  id: string;
  goal: string;
  constraints: string[];
  keywords: string[];
  date_range: DateRange | null;
}

export type PaperId =
  | { kind: "Doi"; value: string }
  | { kind: "Arxiv"; value: string }
  | { kind: "OpenAlex"; value: string }
  | { kind: "DerivedHash"; value: string };

export interface PaperRecord {
  paper_id: PaperId;
  title: string;
  authors: string[];
  year: number | null;
  abstract_text: string | null;
  source_url: string | null;
  source_name: string;
}

export type ScreeningLabel = "Include" | "Maybe" | "Exclude";

export interface ScreeningDecision {
  project_id: string;
  paper_id: PaperId;
  label: ScreeningLabel;
  rationale: string;
  tags: string[];
  confidence: number | null;
}

export interface TopicCandidate {
  id: string;
  project_id: string;
  title: string;
  why_now: string;
  scope: string;
  representative_paper_ids: PaperId[];
  entry_risk: string;
  fallback_scope: string;
}

export interface RunManifest {
  run_id: string;
  project_id: string;
  phase: string;
  created_at: string;
  status: string;
}

export interface WorkflowOutput {
  run_manifest: RunManifest;
  artifact_dir: string;
  screening: ScreeningDecision[];
  topics: TopicCandidate[];
}
