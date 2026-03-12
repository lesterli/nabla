import { useQuery } from "@tanstack/react-query";
import { listTopics, listPapers } from "../api";
import type { PaperId, PaperRecord, TopicCandidate } from "../types";

function useProjectId(): string {
  return localStorage.getItem("nabla_project_id") ?? "";
}

function paperIdKey(id: PaperId): string {
  return `${id.kind}:${id.value}`;
}

export default function TopicsPage() {
  const projectId = useProjectId();

  const { data: topics, isLoading, error } = useQuery({
    queryKey: ["topics", projectId],
    queryFn: () => listTopics(projectId),
    enabled: !!projectId,
  });

  const { data: papers } = useQuery({
    queryKey: ["papers", projectId],
    queryFn: () => listPapers(projectId),
    enabled: !!projectId,
  });

  const paperMap = new Map<string, PaperRecord>();
  papers?.forEach((p) => paperMap.set(paperIdKey(p.paper_id), p));

  if (!projectId) return <p className="text-gray-500">No project selected.</p>;
  if (isLoading) return <p className="text-gray-500">Loading topics...</p>;
  if (error) return <p className="text-red-600">{(error as Error).message}</p>;
  if (!topics?.length) return <p className="text-gray-500">No topic candidates yet.</p>;

  return (
    <div className="space-y-6">
      <h1 className="text-xl font-semibold">Topic Candidates</h1>
      <div className="grid gap-4">
        {topics.map((topic) => (
          <TopicCard key={topic.id} topic={topic} paperMap={paperMap} />
        ))}
      </div>
    </div>
  );
}

function TopicCard({
  topic,
  paperMap,
}: {
  topic: TopicCandidate;
  paperMap: Map<string, PaperRecord>;
}) {
  return (
    <div className="rounded-lg border bg-white p-5 space-y-3">
      <h2 className="font-semibold text-lg">{topic.title}</h2>

      <Section label="Why now">{topic.why_now}</Section>
      <Section label="Scope">{topic.scope}</Section>
      <Section label="Entry risk">{topic.entry_risk}</Section>
      <Section label="Fallback scope">{topic.fallback_scope}</Section>

      {topic.representative_paper_ids.length > 0 && (
        <div>
          <span className="text-xs font-medium text-gray-500 uppercase tracking-wide">
            Representative papers
          </span>
          <ul className="mt-1 space-y-0.5">
            {topic.representative_paper_ids.map((pid) => {
              const paper = paperMap.get(paperIdKey(pid));
              return (
                <li key={paperIdKey(pid)} className="text-sm text-gray-700">
                  {paper ? (
                    paper.source_url ? (
                      <a href={paper.source_url} target="_blank" rel="noopener noreferrer" className="text-blue-600 hover:underline">
                        {paper.title}
                      </a>
                    ) : (
                      paper.title
                    )
                  ) : (
                    <span className="font-mono text-xs text-gray-400">{pid.value}</span>
                  )}
                </li>
              );
            })}
          </ul>
        </div>
      )}
    </div>
  );
}

function Section({ label, children }: { label: string; children: string }) {
  return (
    <div>
      <span className="text-xs font-medium text-gray-500 uppercase tracking-wide">{label}</span>
      <p className="text-sm text-gray-700 mt-0.5">{children}</p>
    </div>
  );
}
