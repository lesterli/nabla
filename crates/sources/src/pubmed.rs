use crate::{normalize_date, normalize_doi, parse_year, search_text, PaperSource};
use anyhow::{anyhow, Context, Result};
use nabla_contracts::{PaperId, PaperRecord, ProjectBrief};
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::blocking::Client;
use serde::Deserialize;

#[derive(Clone)]
pub struct PubMedSource {
    client: Client,
    limit: usize,
}

impl PubMedSource {
    pub fn new(limit: usize) -> Self {
        Self {
            client: Client::new(),
            limit,
        }
    }
}

impl PaperSource for PubMedSource {
    fn fetch(&self, brief: &ProjectBrief) -> Result<Vec<PaperRecord>> {
        let query = search_text(brief);
        let search_url = format!(
            "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi?db=pubmed&term={}&retmax={}&retmode=json{}",
            urlencoding::encode(&query),
            self.limit,
            date_filter(brief)
        );

        let search_response = self
            .client
            .get(search_url)
            .send()
            .context("send PubMed esearch request")?
            .error_for_status()
            .context("PubMed esearch returned error status")?;
        let payload: ESearchResponse = search_response
            .json()
            .context("parse PubMed esearch response")?;

        if payload.esearchresult.idlist.is_empty() {
            return Ok(Vec::new());
        }

        let ids = payload.esearchresult.idlist.join(",");
        let fetch_url = format!(
            "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/efetch.fcgi?db=pubmed&id={ids}&retmode=xml"
        );

        let xml = self
            .client
            .get(fetch_url)
            .send()
            .context("send PubMed efetch request")?
            .error_for_status()
            .context("PubMed efetch returned error status")?
            .text()
            .context("read PubMed efetch body")?;

        parse_pubmed_xml(&xml)
    }
}

fn date_filter(brief: &ProjectBrief) -> String {
    let Some(range) = brief.date_range.as_ref() else {
        return String::new();
    };

    let mut params = String::new();
    let mut has_bound = false;

    if let Some(start) = range.start.as_deref().map(normalize_date) {
        params.push_str("&mindate=");
        params.push_str(&start);
        has_bound = true;
    }

    if let Some(end) = range.end.as_deref().map(normalize_date) {
        params.push_str("&maxdate=");
        params.push_str(&end);
        has_bound = true;
    }

    if has_bound {
        params.push_str("&datetype=pdat");
    }

    params
}

fn parse_pubmed_xml(xml: &str) -> Result<Vec<PaperRecord>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut path = Vec::new();
    let mut articles = Vec::new();
    let mut article = PubMedArticle::default();
    let mut current_author = Option::<PubMedAuthor>::None;
    let mut current_article_id_type = Option::<String>::None;
    let mut in_article = false;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "PubmedArticle" {
                    in_article = true;
                    article = PubMedArticle::default();
                } else if in_article && tag == "Author" {
                    current_author = Some(PubMedAuthor::default());
                } else if in_article && tag == "ArticleId" {
                    current_article_id_type = e
                        .attributes()
                        .flatten()
                        .find(|attr| attr.key.as_ref() == b"IdType")
                        .and_then(|attr| String::from_utf8(attr.value.into_owned()).ok());
                }
                path.push(tag);
            }
            Ok(Event::End(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if in_article && tag == "Author" {
                    if let Some(author) = current_author
                        .take()
                        .and_then(PubMedAuthor::into_display_name)
                    {
                        article.authors.push(author);
                    }
                } else if in_article && tag == "ArticleId" {
                    current_article_id_type = None;
                } else if tag == "PubmedArticle" && in_article {
                    if let Some(record) = std::mem::take(&mut article).to_record() {
                        articles.push(record);
                    }
                    in_article = false;
                }
                path.pop();
            }
            Ok(Event::Text(e)) if in_article => {
                let text = e
                    .unescape()
                    .context("unescape PubMed xml text")?
                    .into_owned();
                apply_text(
                    &mut article,
                    &mut current_author,
                    &current_article_id_type,
                    &path,
                    &text,
                );
            }
            Ok(Event::CData(e)) if in_article => {
                let text = String::from_utf8_lossy(e.as_ref()).into_owned();
                apply_text(
                    &mut article,
                    &mut current_author,
                    &current_article_id_type,
                    &path,
                    &text,
                );
            }
            Ok(Event::Eof) => break,
            Err(err) => return Err(anyhow!("parse PubMed xml: {err}")),
            _ => {}
        }

        buffer.clear();
    }

    Ok(articles)
}

fn append_text(target: &mut String, value: &str) {
    if !target.is_empty() {
        target.push(' ');
    }
    target.push_str(value);
}

fn apply_text(
    article: &mut PubMedArticle,
    current_author: &mut Option<PubMedAuthor>,
    current_article_id_type: &Option<String>,
    path: &[String],
    text: &str,
) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }

    match path.last().map(String::as_str) {
        Some("PMID") if article.pmid.is_none() => article.pmid = Some(text.to_string()),
        Some("ArticleTitle") => append_text(&mut article.title, text),
        Some("AbstractText") => article.abstract_parts.push(text.to_string()),
        Some("Year") if article.year.is_none() && is_primary_publication_year(path) => {
            article.year = parse_year(text)
        }
        Some("ArticleId") if current_article_id_type.as_deref() == Some("doi") => {
            if article.doi.is_none() {
                article.doi = normalize_doi(text);
            }
        }
        Some("ForeName") => {
            if let Some(author) = current_author.as_mut() {
                author.fore_name = Some(text.to_string());
            }
        }
        Some("LastName") => {
            if let Some(author) = current_author.as_mut() {
                author.last_name = Some(text.to_string());
            }
        }
        Some("CollectiveName") => {
            if let Some(author) = current_author.as_mut() {
                author.collective_name = Some(text.to_string());
            }
        }
        _ => {}
    }
}

fn is_primary_publication_year(path: &[String]) -> bool {
    let in_pub_date = path.iter().any(|tag| tag == "PubDate");
    let in_article_date = path.iter().any(|tag| tag == "ArticleDate");
    let in_pubmed_history = path.iter().any(|tag| tag == "PubMedPubDate");

    (in_pub_date || in_article_date) && !in_pubmed_history
}

#[derive(Debug, Deserialize)]
struct ESearchResponse {
    esearchresult: ESearchResult,
}

#[derive(Debug, Deserialize)]
struct ESearchResult {
    idlist: Vec<String>,
}

#[derive(Default)]
struct PubMedArticle {
    pmid: Option<String>,
    doi: Option<String>,
    title: String,
    abstract_parts: Vec<String>,
    authors: Vec<String>,
    year: Option<u16>,
}

impl PubMedArticle {
    fn to_record(self) -> Option<PaperRecord> {
        let pmid = self.pmid?;
        if self.title.trim().is_empty() {
            return None;
        }

        let abstract_text = self
            .abstract_parts
            .into_iter()
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let abstract_text = (!abstract_text.is_empty()).then_some(abstract_text);

        Some(PaperRecord {
            paper_id: self
                .doi
                .map(PaperId::Doi)
                .unwrap_or_else(|| PaperId::PubMed(pmid.clone())),
            title: self.title.trim().to_string(),
            authors: self.authors,
            year: self.year,
            abstract_text,
            source_url: Some(format!("https://pubmed.ncbi.nlm.nih.gov/{pmid}/")),
            source_name: "pubmed".to_string(),
        })
    }
}

#[derive(Default)]
struct PubMedAuthor {
    fore_name: Option<String>,
    last_name: Option<String>,
    collective_name: Option<String>,
}

impl PubMedAuthor {
    fn into_display_name(self) -> Option<String> {
        if let Some(name) = self.collective_name {
            return Some(name);
        }

        match (self.fore_name, self.last_name) {
            (Some(fore_name), Some(last_name)) => Some(format!("{fore_name} {last_name}")),
            (None, Some(last_name)) => Some(last_name),
            (Some(fore_name), None) => Some(fore_name),
            (None, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{date_filter, parse_pubmed_xml};
    use nabla_contracts::{DateRange, PaperId, ProjectBrief};

    #[test]
    fn builds_pubmed_date_filter() {
        let brief = ProjectBrief {
            id: "p1".into(),
            goal: "find papers".into(),
            constraints: vec![],
            keywords: vec!["crispr".into()],
            date_range: Some(DateRange {
                start: Some("2020-01-01".into()),
                end: Some("2024-12-31".into()),
            }),
        };

        assert_eq!(
            date_filter(&brief),
            "&mindate=2020/01/01&maxdate=2024/12/31&datetype=pdat"
        );
    }

    #[test]
    fn parses_pubmed_xml_and_prefers_doi_id() {
        let xml = r#"
        <PubmedArticleSet>
          <PubmedArticle>
            <MedlineCitation>
              <PMID>12345</PMID>
              <Article>
                <ArticleTitle>CRISPR delivery paper</ArticleTitle>
                <Abstract>
                  <AbstractText>First abstract paragraph.</AbstractText>
                  <AbstractText>Second abstract paragraph.</AbstractText>
                </Abstract>
                <AuthorList>
                  <Author>
                    <ForeName>Alice</ForeName>
                    <LastName>Smith</LastName>
                  </Author>
                  <Author>
                    <CollectiveName>Genome Lab</CollectiveName>
                  </Author>
                </AuthorList>
                <Journal>
                  <JournalIssue>
                    <PubDate>
                      <Year>2024</Year>
                    </PubDate>
                  </JournalIssue>
                </Journal>
              </Article>
            </MedlineCitation>
            <PubmedData>
              <ArticleIdList>
                <ArticleId IdType="doi">10.1000/ABC</ArticleId>
              </ArticleIdList>
            </PubmedData>
          </PubmedArticle>
        </PubmedArticleSet>
        "#;

        let papers = parse_pubmed_xml(xml).unwrap();
        assert_eq!(papers.len(), 1);
        assert_eq!(papers[0].paper_id, PaperId::Doi("10.1000/abc".into()));
        assert_eq!(papers[0].authors, vec!["Alice Smith", "Genome Lab"]);
        assert_eq!(
            papers[0].abstract_text.as_deref(),
            Some("First abstract paragraph.\nSecond abstract paragraph.")
        );
    }

    #[test]
    fn falls_back_to_pmid_when_doi_missing() {
        let xml = r#"
        <PubmedArticleSet>
          <PubmedArticle>
            <MedlineCitation>
              <PMID>999</PMID>
              <Article>
                <ArticleTitle>Only PMID</ArticleTitle>
                <Journal>
                  <JournalIssue>
                    <PubDate>
                      <Year>2023</Year>
                    </PubDate>
                  </JournalIssue>
                </Journal>
              </Article>
            </MedlineCitation>
          </PubmedArticle>
        </PubmedArticleSet>
        "#;

        let papers = parse_pubmed_xml(xml).unwrap();
        assert_eq!(papers.len(), 1);
        assert_eq!(papers[0].paper_id, PaperId::PubMed("999".into()));
    }
}
