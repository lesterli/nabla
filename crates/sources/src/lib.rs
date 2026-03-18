mod arxiv;
mod openalex;
mod pubmed;

use anyhow::{anyhow, Result};
use nabla_contracts::{PaperId, PaperRecord, ProjectBrief};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;

pub use arxiv::ArxivSource;
pub use openalex::OpenAlexSource;
pub use pubmed::PubMedSource;

pub trait PaperCollector: Send + Sync {
    fn collect(&self, brief: &ProjectBrief) -> Result<Vec<PaperRecord>>;
}

pub trait PaperSource: Send + Sync {
    fn fetch(&self, brief: &ProjectBrief) -> Result<Vec<PaperRecord>>;
}

pub struct CompositeCollector {
    sources: Vec<Box<dyn PaperSource>>,
}

impl CompositeCollector {
    pub fn new(sources: Vec<Box<dyn PaperSource>>) -> Self {
        Self { sources }
    }
}

impl PaperCollector for CompositeCollector {
    fn collect(&self, brief: &ProjectBrief) -> Result<Vec<PaperRecord>> {
        let mut papers = Vec::new();
        let mut errors = Vec::new();

        for source in &self.sources {
            match source.fetch(brief) {
                Ok(source_papers) => papers.extend(source_papers),
                Err(error) => errors.push(error.to_string()),
            }
        }

        if papers.is_empty() && !errors.is_empty() {
            return Err(anyhow!("all paper sources failed: {}", errors.join("; ")));
        }

        Ok(dedup_papers(papers))
    }
}

#[derive(Clone)]
pub struct StaticCollector {
    papers: Vec<PaperRecord>,
}

impl StaticCollector {
    pub fn new(papers: Vec<PaperRecord>) -> Self {
        Self { papers }
    }
}

impl PaperCollector for StaticCollector {
    fn collect(&self, _brief: &ProjectBrief) -> Result<Vec<PaperRecord>> {
        Ok(self.papers.clone())
    }
}

fn dedup_papers(papers: Vec<PaperRecord>) -> Vec<PaperRecord> {
    let mut deduped: BTreeMap<String, PaperRecord> = BTreeMap::new();

    for paper in papers {
        let key = canonical_paper_key(&paper.paper_id);
        if let Some(existing) = deduped.get_mut(&key) {
            merge_paper_records(existing, &paper);
        } else {
            deduped.insert(key, paper);
        }
    }

    deduped.into_values().collect()
}

fn merge_paper_records(existing: &mut PaperRecord, incoming: &PaperRecord) {
    if should_replace_text(
        existing.abstract_text.as_deref(),
        incoming.abstract_text.as_deref(),
    ) {
        existing.abstract_text = incoming.abstract_text.clone();
    }

    if existing.source_url.is_none() {
        existing.source_url = incoming.source_url.clone();
    }

    if existing.authors.is_empty() && !incoming.authors.is_empty() {
        existing.authors = incoming.authors.clone();
    }

    if existing.year.is_none() {
        existing.year = incoming.year;
    }

    if existing.title.trim().is_empty() && !incoming.title.trim().is_empty() {
        existing.title = incoming.title.clone();
    }

    existing.source_name = merge_source_names(&existing.source_name, &incoming.source_name);
}

fn should_replace_text(existing: Option<&str>, incoming: Option<&str>) -> bool {
    let existing_len = existing.map(str::trim).map(str::len).unwrap_or(0);
    let incoming_len = incoming.map(str::trim).map(str::len).unwrap_or(0);
    incoming_len > existing_len
}

fn merge_source_names(existing: &str, incoming: &str) -> String {
    let mut names = Vec::new();

    for value in [existing, incoming] {
        for name in value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if !names.iter().any(|seen: &String| seen == name) {
                names.push(name.to_string());
            }
        }
    }

    names.join(",")
}

pub(crate) fn canonical_paper_key(paper_id: &PaperId) -> String {
    match paper_id {
        PaperId::Doi(value) => {
            let doi = normalize_doi(value).unwrap_or_else(|| value.trim().to_lowercase());
            format!("doi:{doi}")
        }
        PaperId::Arxiv(value) => format!("arxiv:{}", value.trim()),
        PaperId::OpenAlex(value) => format!("openalex:{}", value.trim()),
        PaperId::PubMed(value) => format!("pubmed:{}", value.trim()),
        PaperId::DerivedHash(value) => format!("derived:{}", value.trim()),
    }
}

pub(crate) fn normalize_doi(value: &str) -> Option<String> {
    let normalized = value
        .trim()
        .trim_start_matches("https://doi.org/")
        .trim_start_matches("http://doi.org/")
        .trim_start_matches("doi:")
        .trim()
        .to_lowercase();

    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

pub(crate) fn derived_paper_id(title: &str, year: Option<u16>) -> PaperId {
    let mut hasher = Sha1::new();
    hasher.update(title.trim().to_lowercase().as_bytes());
    if let Some(year) = year {
        hasher.update(year.to_string().as_bytes());
    }
    PaperId::DerivedHash(format!("{:x}", hasher.finalize()))
}

pub(crate) fn rebuild_inverted_index(index: BTreeMap<String, Vec<usize>>) -> String {
    let mut words = vec![String::new(); index.values().flatten().max().map(|v| v + 1).unwrap_or(0)];
    for (token, positions) in index {
        for position in positions {
            if let Some(slot) = words.get_mut(position) {
                *slot = token.clone();
            }
        }
    }
    words.join(" ").trim().to_string()
}

pub(crate) fn search_terms(brief: &ProjectBrief) -> Vec<String> {
    if brief.keywords.is_empty() {
        vec![brief.goal.clone()]
    } else {
        brief.keywords.clone()
    }
}

pub(crate) fn search_text(brief: &ProjectBrief) -> String {
    search_terms(brief).join(" ")
}

pub(crate) fn start_year(brief: &ProjectBrief) -> Option<u16> {
    brief
        .date_range
        .as_ref()
        .and_then(|range| range.start.as_deref())
        .and_then(parse_year)
}

pub(crate) fn parse_year(value: &str) -> Option<u16> {
    value.trim().get(0..4)?.parse().ok()
}

pub(crate) fn normalize_date(value: &str) -> String {
    value.trim().replace('-', "/")
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_paper_key, dedup_papers, CompositeCollector, PaperCollector, PaperSource,
    };
    use anyhow::{anyhow, Result};
    use nabla_contracts::{PaperId, PaperRecord, ProjectBrief};

    struct FailingSource;

    impl PaperSource for FailingSource {
        fn fetch(&self, _brief: &ProjectBrief) -> Result<Vec<PaperRecord>> {
            Err(anyhow!("boom"))
        }
    }

    struct StaticSource(Vec<PaperRecord>);

    impl PaperSource for StaticSource {
        fn fetch(&self, _brief: &ProjectBrief) -> Result<Vec<PaperRecord>> {
            Ok(self.0.clone())
        }
    }

    fn sample_brief() -> ProjectBrief {
        ProjectBrief {
            id: "p1".into(),
            goal: "find papers".into(),
            constraints: vec![],
            keywords: vec!["crispr".into()],
            date_range: None,
        }
    }

    #[test]
    fn dedups_by_normalized_doi() {
        let deduped = dedup_papers(vec![
            PaperRecord {
                paper_id: PaperId::Doi("https://doi.org/10.1000/ABC".into()),
                title: "A".into(),
                authors: vec![],
                year: Some(2024),
                abstract_text: None,
                source_url: None,
                source_name: "openalex".into(),
            },
            PaperRecord {
                paper_id: PaperId::Doi("doi:10.1000/abc".into()),
                title: "A".into(),
                authors: vec![],
                year: Some(2024),
                abstract_text: Some("text".into()),
                source_url: Some("url".into()),
                source_name: "pubmed".into(),
            },
        ]);

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].source_name, "openalex,pubmed");
        assert_eq!(deduped[0].abstract_text.as_deref(), Some("text"));
    }

    #[test]
    fn canonicalizes_pubmed_keys() {
        assert_eq!(
            canonical_paper_key(&PaperId::PubMed("12345".into())),
            "pubmed:12345"
        );
    }

    #[test]
    fn collector_tolerates_single_source_failure() {
        let collector = CompositeCollector::new(vec![
            Box::new(FailingSource),
            Box::new(StaticSource(vec![PaperRecord {
                paper_id: PaperId::Arxiv("1234.5678".into()),
                title: "Paper".into(),
                authors: vec![],
                year: Some(2024),
                abstract_text: None,
                source_url: None,
                source_name: "arxiv".into(),
            }])),
        ]);

        let papers = collector.collect(&sample_brief()).unwrap();
        assert_eq!(papers.len(), 1);
    }
}
