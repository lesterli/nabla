use crate::{
    derived_paper_id, normalize_doi, rebuild_inverted_index, search_text, start_year, PaperSource,
};
use anyhow::{Context, Result};
use nabla_contracts::{PaperId, PaperRecord, ProjectBrief};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone)]
pub struct OpenAlexSource {
    client: Client,
    limit: usize,
}

impl OpenAlexSource {
    pub fn new(limit: usize) -> Self {
        Self {
            client: Client::new(),
            limit,
        }
    }
}

impl PaperSource for OpenAlexSource {
    fn fetch(&self, brief: &ProjectBrief) -> Result<Vec<PaperRecord>> {
        let query = search_text(brief);
        let mut url = format!(
            "https://api.openalex.org/works?search={}&sort=relevance_score:desc&per-page={}",
            urlencoding::encode(&query),
            self.limit
        );

        if let Some(year) = start_year(brief) {
            let filter_year = year.saturating_sub(1);
            url.push_str(&format!("&filter=publication_year:>{filter_year}"));
        }

        let response = self
            .client
            .get(url)
            .send()
            .context("send OpenAlex request")?
            .error_for_status()
            .context("OpenAlex returned error status")?;
        let payload: OpenAlexResponse = response.json().context("parse OpenAlex response")?;

        Ok(payload
            .results
            .into_iter()
            .map(|work| {
                let year = work
                    .publication_year
                    .and_then(|year| u16::try_from(year).ok());
                let abstract_text = work
                    .abstract_inverted_index
                    .map(rebuild_inverted_index)
                    .filter(|text| !text.is_empty());

                PaperRecord {
                    paper_id: work
                        .doi
                        .as_deref()
                        .and_then(normalize_doi)
                        .map(PaperId::Doi)
                        .or_else(|| {
                            work.id
                                .rsplit('/')
                                .next()
                                .map(|id| PaperId::OpenAlex(id.to_string()))
                        })
                        .unwrap_or_else(|| derived_paper_id(&work.display_name, year)),
                    title: work.display_name,
                    authors: work
                        .authorships
                        .into_iter()
                        .filter_map(|auth| auth.author.map(|author| author.display_name))
                        .collect(),
                    year,
                    abstract_text,
                    source_url: work
                        .primary_location
                        .and_then(|location| location.landing_page_url),
                    source_name: "openalex".to_string(),
                }
            })
            .collect())
    }
}

#[derive(Debug, Deserialize)]
struct OpenAlexResponse {
    results: Vec<OpenAlexWork>,
}

#[derive(Debug, Deserialize)]
struct OpenAlexWork {
    id: String,
    doi: Option<String>,
    display_name: String,
    publication_year: Option<i64>,
    authorships: Vec<OpenAlexAuthorship>,
    abstract_inverted_index: Option<BTreeMap<String, Vec<usize>>>,
    primary_location: Option<OpenAlexLocation>,
}

#[derive(Debug, Deserialize)]
struct OpenAlexAuthorship {
    author: Option<OpenAlexAuthor>,
}

#[derive(Debug, Deserialize)]
struct OpenAlexAuthor {
    display_name: String,
}

#[derive(Debug, Deserialize)]
struct OpenAlexLocation {
    landing_page_url: Option<String>,
}
