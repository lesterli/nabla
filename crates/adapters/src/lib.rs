use anyhow::Result;
use nabla_contracts::{
    PaperRecord, ProjectBrief, ScreeningDecision, ScreeningLabel, TopicCandidate,
};
use std::collections::{BTreeMap, BTreeSet};

pub trait AgentAdapter {
    fn name(&self) -> &'static str;
    fn screen(&self, brief: &ProjectBrief, papers: &[PaperRecord]) -> Result<Vec<ScreeningDecision>>;
    fn propose(
        &self,
        brief: &ProjectBrief,
        papers: &[PaperRecord],
        decisions: &[ScreeningDecision],
    ) -> Result<Vec<TopicCandidate>>;
}

#[derive(Debug, Default, Clone)]
pub struct MockAgentAdapter;

impl MockAgentAdapter {
    fn normalized_keywords(brief: &ProjectBrief) -> Vec<String> {
        brief
            .keywords
            .iter()
            .map(|keyword| keyword.trim().to_lowercase())
            .filter(|keyword| !keyword.is_empty())
            .collect()
    }

    fn extract_tags(keywords: &[String], paper: &PaperRecord) -> Vec<String> {
        let haystack = format!(
            "{} {}",
            paper.title.to_lowercase(),
            paper.abstract_text.clone().unwrap_or_default().to_lowercase()
        );
        let mut tags = Vec::new();
        for keyword in keywords {
            if haystack.contains(keyword) {
                tags.push(keyword.clone());
            }
            if tags.len() == 2 {
                break;
            }
        }
        if tags.is_empty() {
            tags.push("background".to_string());
        }
        tags
    }
}

impl AgentAdapter for MockAgentAdapter {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn screen(&self, brief: &ProjectBrief, papers: &[PaperRecord]) -> Result<Vec<ScreeningDecision>> {
        let keywords = Self::normalized_keywords(brief);
        let decisions = papers
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

                ScreeningDecision {
                    project_id: brief.id.clone(),
                    paper_id: paper.paper_id.clone(),
                    label,
                    rationale,
                    tags: Self::extract_tags(&keywords, paper),
                    confidence,
                }
            })
            .collect();
        Ok(decisions)
    }

    fn propose(
        &self,
        brief: &ProjectBrief,
        papers: &[PaperRecord],
        decisions: &[ScreeningDecision],
    ) -> Result<Vec<TopicCandidate>> {
        let paper_map: BTreeMap<String, &PaperRecord> =
            papers.iter().map(|paper| (paper.paper_id.as_key(), paper)).collect();
        let mut grouped: BTreeMap<String, Vec<&ScreeningDecision>> = BTreeMap::new();
        for decision in decisions.iter().filter(|decision| decision.label != ScreeningLabel::Exclude) {
            let key = decision
                .tags
                .first()
                .cloned()
                .unwrap_or_else(|| "background".to_string());
            grouped.entry(key).or_default().push(decision);
        }

        if grouped.is_empty() {
            grouped.insert(
                "background".to_string(),
                decisions.iter().take(3).collect(),
            );
        }

        let mut topics = Vec::new();
        for (index, (tag, grouped_decisions)) in grouped.into_iter().take(3).enumerate() {
            let representative_paper_ids: Vec<_> = grouped_decisions
                .iter()
                .take(3)
                .map(|decision| decision.paper_id.clone())
                .collect();
            let title = format!("{} focus: {}", brief.goal, tag);
            let paper_titles: BTreeSet<_> = grouped_decisions
                .iter()
                .filter_map(|decision| paper_map.get(&decision.paper_id.as_key()).map(|paper| paper.title.clone()))
                .collect();
            let scope = if paper_titles.is_empty() {
                "Build a bounded reading list around the topic tag and compare the cited methods.".to_string()
            } else {
                format!(
                    "Start from {} and compare the methods and evaluation settings they use.",
                    paper_titles.into_iter().take(2).collect::<Vec<_>>().join("; ")
                )
            };

            topics.push(TopicCandidate {
                id: format!("topic-{}", index + 1),
                project_id: brief.id.clone(),
                title,
                why_now: format!(
                    "This direction clusters papers matching '{}' and gives a focused entry point.",
                    tag
                ),
                scope,
                representative_paper_ids,
                entry_risk: "The cluster may still contain broad or mixed papers and needs human review.".to_string(),
                fallback_scope: format!(
                    "Limit the next reading pass to papers tagged '{}' and recent related variants.",
                    tag
                ),
            });
        }

        Ok(topics)
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentAdapter, MockAgentAdapter};
    use nabla_contracts::{PaperId, PaperRecord, ProjectBrief, ScreeningLabel};

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
        let adapter = MockAgentAdapter;
        let brief = make_brief();
        let papers = vec![make_paper("a", "Unrelated topic about cooking", None)];
        let decisions = adapter.screen(&brief, &papers).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].label, ScreeningLabel::Exclude);
    }

    #[test]
    fn screen_maybe_when_one_keyword_matches() {
        let adapter = MockAgentAdapter;
        let brief = make_brief();
        let papers = vec![make_paper("b", "Neural networks for images", None)];
        let decisions = adapter.screen(&brief, &papers).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].label, ScreeningLabel::Maybe);
    }

    #[test]
    fn screen_includes_when_multiple_keywords_match() {
        let adapter = MockAgentAdapter;
        let brief = make_brief();
        let papers = vec![make_paper(
            "c",
            "Neural operator for PDE solving",
            Some("A neural operator approach"),
        )];
        let decisions = adapter.screen(&brief, &papers).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].label, ScreeningLabel::Include);
    }

    #[test]
    fn propose_falls_back_when_all_excluded() {
        let adapter = MockAgentAdapter;
        let brief = make_brief();
        let papers = vec![make_paper("d", "Cooking recipes", None)];
        let decisions = adapter.screen(&brief, &papers).unwrap();
        assert_eq!(decisions[0].label, ScreeningLabel::Exclude);
        let topics = adapter.propose(&brief, &papers, &decisions).unwrap();
        assert!(!topics.is_empty(), "should produce fallback topics even when all excluded");
    }
}

