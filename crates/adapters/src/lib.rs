use anyhow::Result;
use nabla_contracts::{
    PaperRecord, ProjectBrief, ScreeningDecision, ScreeningLabel, TopicCandidate,
};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::NamedTempFile;

pub trait AgentAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn screen(&self, brief: &ProjectBrief, papers: &[PaperRecord]) -> Result<Vec<ScreeningDecision>>;
    fn propose(
        &self,
        brief: &ProjectBrief,
        papers: &[PaperRecord],
        decisions: &[ScreeningDecision],
    ) -> Result<Vec<TopicCandidate>>;
}

#[derive(Debug, Clone, Copy)]
pub enum LocalCliProvider {
    Codex,
    Claude,
}

pub struct LocalCliAdapter {
    provider: LocalCliProvider,
}

impl LocalCliAdapter {
    pub fn codex() -> Self {
        Self {
            provider: LocalCliProvider::Codex,
        }
    }

    pub fn claude() -> Self {
        Self {
            provider: LocalCliProvider::Claude,
        }
    }

    fn run_with_schema<T: DeserializeOwned>(&self, prompt: &str, schema: Value) -> Result<T> {
        match self.provider {
            LocalCliProvider::Codex => self.run_codex(prompt, schema),
            LocalCliProvider::Claude => self.run_claude(prompt, schema),
        }
    }

    fn run_codex<T: DeserializeOwned>(&self, prompt: &str, schema: Value) -> Result<T> {
        let mut schema_file = NamedTempFile::new()?;
        serde_json::to_writer_pretty(schema_file.as_file_mut(), &schema)?;
        schema_file.as_file_mut().flush()?;

        let output_file = NamedTempFile::new()?;
        let output_path = output_file.into_temp_path();

        let mut child = Command::new("codex")
            .args([
                "exec",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--color",
                "never",
                "--output-schema",
            ])
            .arg(schema_file.path())
            .arg("--output-last-message")
            .arg(&output_path)
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        {
            let mut stdin = child.stdin.take().expect("stdin was configured as piped");
            stdin.write_all(prompt.as_bytes())?;
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "codex failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let response_text = std::fs::read_to_string(&output_path)?;
        parse_structured_response(&response_text)
    }

    fn run_claude<T: DeserializeOwned>(&self, prompt: &str, schema: Value) -> Result<T> {
        let schema_json = serde_json::to_string(&schema)?;
        let mut child = Command::new("claude")
            .args([
                "-p",
                "--output-format",
                "json",
                "--json-schema",
                &schema_json,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        {
            let mut stdin = child.stdin.take().expect("stdin was configured as piped");
            stdin.write_all(prompt.as_bytes())?;
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "claude failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        parse_structured_response(&String::from_utf8_lossy(&output.stdout))
    }
}

#[derive(Debug, Deserialize)]
struct ScreeningResponse {
    items: Vec<ScreeningItem>,
}

#[derive(Debug, Deserialize)]
struct ScreeningItem {
    paper_id: String,
    label: String,
    rationale: String,
    tags: Vec<String>,
    confidence: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct TopicResponse {
    items: Vec<TopicItem>,
}

#[derive(Debug, Deserialize)]
struct TopicItem {
    title: String,
    why_now: String,
    scope: String,
    representative_paper_ids: Vec<String>,
    entry_risk: String,
    fallback_scope: String,
}

impl AgentAdapter for LocalCliAdapter {
    fn name(&self) -> &'static str {
        match self.provider {
            LocalCliProvider::Codex => "codex",
            LocalCliProvider::Claude => "claude",
        }
    }

    fn screen(&self, brief: &ProjectBrief, papers: &[PaperRecord]) -> Result<Vec<ScreeningDecision>> {
        let papers_payload: Vec<_> = papers
            .iter()
            .map(|paper| {
                json!({
                    "paper_id": paper.paper_id.as_key(),
                    "title": paper.title,
                    "year": paper.year,
                    "abstract_text": paper.abstract_text,
                })
            })
            .collect();
        let brief_payload = json!({
            "goal": brief.goal,
            "constraints": brief.constraints,
            "keywords": brief.keywords,
        });
        let prompt = format!(
            "You are screening papers for a research topic selection workflow.\n\
Project brief:\n{}\n\
Papers:\n{}\n\
For every paper, assign exactly one label from include/maybe/exclude. Keep rationale concise and use at most two tags.",
            serde_json::to_string_pretty(&brief_payload)?,
            serde_json::to_string_pretty(&papers_payload)?
        );

        let response: ScreeningResponse = self.run_with_schema(&prompt, screening_schema())?;
        let paper_ids: BTreeMap<String, _> = papers
            .iter()
            .map(|paper| (paper.paper_id.as_key(), paper.paper_id.clone()))
            .collect();

        response
            .items
            .into_iter()
            .map(|item| {
                let label = parse_screening_label(&item.label)?;
                let paper_id = paper_ids
                    .get(&item.paper_id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("screening response referenced unknown paper_id {}", item.paper_id))?;
                Ok(ScreeningDecision {
                    project_id: brief.id.clone(),
                    paper_id,
                    label,
                    rationale: item.rationale,
                    tags: item.tags,
                    confidence: item.confidence,
                })
            })
            .collect()
    }

    fn propose(
        &self,
        brief: &ProjectBrief,
        papers: &[PaperRecord],
        decisions: &[ScreeningDecision],
    ) -> Result<Vec<TopicCandidate>> {
        let paper_map: BTreeMap<String, _> = papers
            .iter()
            .map(|paper| {
                (
                    paper.paper_id.as_key(),
                    json!({
                        "paper_id": paper.paper_id.as_key(),
                        "title": paper.title,
                        "year": paper.year,
                        "abstract_text": paper.abstract_text,
                    }),
                )
            })
            .collect();
        let decisions_payload: Vec<_> = decisions
            .iter()
            .map(|decision| {
                json!({
                    "paper_id": decision.paper_id.as_key(),
                    "label": match decision.label {
                        ScreeningLabel::Include => "include",
                        ScreeningLabel::Maybe => "maybe",
                        ScreeningLabel::Exclude => "exclude",
                    },
                    "rationale": decision.rationale,
                    "tags": decision.tags,
                    "confidence": decision.confidence,
                })
            })
            .collect();
        let brief_payload = json!({
            "goal": brief.goal,
            "constraints": brief.constraints,
            "keywords": brief.keywords,
        });
        let prompt = format!(
            "You are proposing candidate research directions from screened papers.\n\
Project brief:\n{}\n\
Papers:\n{}\n\
Screening decisions:\n{}\n\
Generate 2 to 3 candidate topic directions. Only use paper_ids from the provided papers.",
            serde_json::to_string_pretty(&brief_payload)?,
            serde_json::to_string_pretty(&paper_map.values().collect::<Vec<_>>())?,
            serde_json::to_string_pretty(&decisions_payload)?,
        );

        let response: TopicResponse = self.run_with_schema(&prompt, topic_schema())?;
        let paper_ids: BTreeMap<String, _> = papers
            .iter()
            .map(|paper| (paper.paper_id.as_key(), paper.paper_id.clone()))
            .collect();

        response
            .items
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                let representative_paper_ids = item
                    .representative_paper_ids
                    .into_iter()
                    .map(|paper_id| {
                        paper_ids
                            .get(&paper_id)
                            .cloned()
                            .ok_or_else(|| anyhow::anyhow!("topic response referenced unknown paper_id {paper_id}"))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(TopicCandidate {
                    id: format!("topic-{}", index + 1),
                    project_id: brief.id.clone(),
                    title: item.title,
                    why_now: item.why_now,
                    scope: item.scope,
                    representative_paper_ids,
                    entry_risk: item.entry_risk,
                    fallback_scope: item.fallback_scope,
                })
            })
            .collect()
    }
}

fn screening_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "items": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "paper_id": { "type": "string" },
                        "label": { "type": "string", "enum": ["include", "maybe", "exclude"] },
                        "rationale": { "type": "string" },
                        "tags": { "type": "array", "items": { "type": "string" } },
                        "confidence": { "type": ["number", "null"] }
                    },
                    "required": ["paper_id", "label", "rationale", "tags", "confidence"]
                }
            }
        },
        "required": ["items"]
    })
}

fn topic_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "items": {
                "type": "array",
                "minItems": 2,
                "maxItems": 3,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "title": { "type": "string" },
                        "why_now": { "type": "string" },
                        "scope": { "type": "string" },
                        "representative_paper_ids": {
                            "type": "array",
                            "items": { "type": "string" }
                        },
                        "entry_risk": { "type": "string" },
                        "fallback_scope": { "type": "string" }
                    },
                    "required": [
                        "title",
                        "why_now",
                        "scope",
                        "representative_paper_ids",
                        "entry_risk",
                        "fallback_scope"
                    ]
                }
            }
        },
        "required": ["items"]
    })
}

/// Keyword-based test adapter for local development without LLM calls.
pub struct TestAdapter;

impl AgentAdapter for TestAdapter {
    fn name(&self) -> &'static str {
        "test"
    }

    fn screen(&self, brief: &ProjectBrief, papers: &[PaperRecord]) -> Result<Vec<ScreeningDecision>> {
        let keywords: Vec<String> = brief
            .keywords
            .iter()
            .map(|keyword| keyword.trim().to_lowercase())
            .collect();
        Ok(papers
            .iter()
            .map(|paper| {
                let haystack = format!(
                    "{} {}",
                    paper.title.to_lowercase(),
                    paper.abstract_text.clone().unwrap_or_default().to_lowercase()
                );
                let score = keywords
                    .iter()
                    .filter(|keyword| haystack.contains(keyword.as_str()))
                    .count();
                let (label, confidence, rationale) = match score {
                    0 => (
                        ScreeningLabel::Exclude,
                        Some(0.25),
                        "No project keyword overlap found in title or abstract.".to_string(),
                    ),
                    1 => (
                        ScreeningLabel::Maybe,
                        Some(0.55),
                        "One project keyword matched; worth manual inspection.".to_string(),
                    ),
                    _ => (
                        ScreeningLabel::Include,
                        Some(0.85),
                        "Multiple project keywords matched; likely relevant to the topic.".to_string(),
                    ),
                };
                let mut tags = Vec::new();
                for keyword in &keywords {
                    if haystack.contains(keyword) {
                        tags.push(keyword.clone());
                    }
                    if tags.len() == 2 {
                        break;
                    }
                }
                if tags.is_empty() {
                    tags.push("background".into());
                }

                ScreeningDecision {
                    project_id: brief.id.clone(),
                    paper_id: paper.paper_id.clone(),
                    label,
                    rationale,
                    tags,
                    confidence,
                }
            })
            .collect())
    }

    fn propose(
        &self,
        brief: &ProjectBrief,
        papers: &[PaperRecord],
        decisions: &[ScreeningDecision],
    ) -> Result<Vec<TopicCandidate>> {
        let paper_map: std::collections::BTreeMap<String, &PaperRecord> =
            papers.iter().map(|paper| (paper.paper_id.as_key(), paper)).collect();
        let mut grouped: std::collections::BTreeMap<String, Vec<&ScreeningDecision>> = std::collections::BTreeMap::new();
        for decision in decisions.iter().filter(|decision| decision.label != ScreeningLabel::Exclude) {
            let key = decision
                .tags
                .first()
                .cloned()
                .unwrap_or_else(|| "background".to_string());
            grouped.entry(key).or_default().push(decision);
        }
        if grouped.is_empty() {
            grouped.insert("background".into(), decisions.iter().take(3).collect());
        }

        Ok(grouped
            .into_iter()
            .take(3)
            .enumerate()
            .map(|(index, (tag, grouped_decisions))| {
                let representative_paper_ids: Vec<_> = grouped_decisions
                    .iter()
                    .take(3)
                    .map(|decision| decision.paper_id.clone())
                    .collect();
                let paper_titles: std::collections::BTreeSet<_> = grouped_decisions
                    .iter()
                    .filter_map(|decision| {
                        paper_map
                            .get(&decision.paper_id.as_key())
                            .map(|paper| paper.title.clone())
                    })
                    .collect();
                let scope = if paper_titles.is_empty() {
                    "Build a bounded reading list around the topic tag and compare the cited methods."
                        .to_string()
                } else {
                    format!(
                        "Start from {} and compare the methods and evaluation settings they use.",
                        paper_titles.into_iter().take(2).collect::<Vec<_>>().join("; ")
                    )
                };

                TopicCandidate {
                    id: format!("topic-{}", index + 1),
                    project_id: brief.id.clone(),
                    title: format!("{} focus: {}", brief.goal, tag),
                    why_now: format!(
                        "This direction clusters papers matching '{}' and gives a focused entry point.",
                        tag
                    ),
                    scope,
                    representative_paper_ids,
                    entry_risk:
                        "The cluster may still contain broad or mixed papers and needs human review."
                            .to_string(),
                    fallback_scope: format!(
                        "Limit the next reading pass to papers tagged '{}' and recent related variants.",
                        tag
                    ),
                }
            })
            .collect())
    }
}

fn parse_screening_label(value: &str) -> Result<ScreeningLabel> {
    match value {
        "include" => Ok(ScreeningLabel::Include),
        "maybe" => Ok(ScreeningLabel::Maybe),
        "exclude" => Ok(ScreeningLabel::Exclude),
        other => Err(anyhow::anyhow!("unknown screening label: {other}")),
    }
}

fn parse_structured_response<T: DeserializeOwned>(body: &str) -> Result<T> {
    if let Ok(parsed) = serde_json::from_str::<T>(body.trim()) {
        return Ok(parsed);
    }

    let envelope: Value =
        serde_json::from_str(body).map_err(|err| anyhow::anyhow!("parse CLI JSON envelope: {err}; body={body}"))?;

    for key in ["structured_output", "result", "text", "content"] {
        if let Some(value) = envelope.get(key) {
            if let Ok(parsed) = serde_json::from_value::<T>(value.clone()) {
                return Ok(parsed);
            }
            if let Some(text) = value.as_str() {
                if let Ok(parsed) = serde_json::from_str::<T>(text) {
                    return Ok(parsed);
                }
            }
        }
    }

    Err(anyhow::anyhow!(
        "unable to extract structured response from CLI output: {body}"
    ))
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use super::{parse_structured_response, TestAdapter, AgentAdapter};
    use nabla_contracts::{PaperId, PaperRecord, ProjectBrief, ScreeningLabel};
    use serde::Deserialize;

    fn make_brief() -> ProjectBrief {
        ProjectBrief {
            id: "p1".into(),
            goal: "neural operator".into(),
            constraints: vec![],
            keywords: vec!["neural".into(), "operator".into(), "pde".into()],
            date_range: None,
        }
    }

    fn make_paper(id: &str, title: &str, abstract_text: Option<&str>) -> PaperRecord {
        PaperRecord {
            paper_id: PaperId::DerivedHash(id.into()),
            title: title.into(),
            authors: vec![],
            year: Some(2024),
            abstract_text: abstract_text.map(String::from),
            source_url: None,
            source_name: "test".into(),
        }
    }

    #[test]
    fn screen_excludes_when_no_keywords_match() {
        let adapter = TestAdapter;
        let brief = make_brief();
        let papers = vec![make_paper("a", "Unrelated topic about cooking", None)];
        let decisions = adapter.screen(&brief, &papers).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].label, ScreeningLabel::Exclude);
    }

    #[test]
    fn screen_maybe_when_one_keyword_matches() {
        let adapter = TestAdapter;
        let brief = make_brief();
        let papers = vec![make_paper("a", "Operator methods in chemistry", None)];
        let decisions = adapter.screen(&brief, &papers).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].label, ScreeningLabel::Maybe);
    }

    #[test]
    fn screen_includes_when_multiple_keywords_match() {
        let adapter = TestAdapter;
        let brief = make_brief();
        let papers = vec![make_paper(
            "a",
            "Neural operator methods for PDEs",
            Some("Scientific machine learning"),
        )];
        let decisions = adapter.screen(&brief, &papers).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].label, ScreeningLabel::Include);
    }

    #[test]
    fn propose_falls_back_when_all_excluded() {
        let adapter = TestAdapter;
        let brief = make_brief();
        let papers = vec![make_paper("a", "Completely unrelated topic", None)];
        let decisions = adapter.screen(&brief, &papers).unwrap();
        let topics = adapter.propose(&brief, &papers, &decisions).unwrap();
        assert!(!topics.is_empty());
    }

    #[test]
    fn parses_enveloped_cli_json_result() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Payload {
            answer: String,
        }

        let body = r#"{"type":"result","result":"{\"answer\":\"ok\"}"}"#;
        let parsed: Payload = parse_structured_response(body).unwrap();
        assert_eq!(parsed, Payload { answer: "ok".into() });
    }

    #[test]
    fn parses_claude_structured_output_envelope() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Payload {
            items: Vec<Item>,
        }

        #[derive(Debug, Deserialize, PartialEq)]
        struct Item {
            name: String,
        }

        let body = r#"{"type":"result","subtype":"success","result":"","structured_output":{"items":[{"name":"hello"}]}}"#;
        let parsed: Payload = parse_structured_response(body).unwrap();
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].name, "hello");
    }
}
