use crate::{search_terms, PaperSource};
use anyhow::{anyhow, Context, Result};
use nabla_contracts::{PaperId, PaperRecord, ProjectBrief};
use quick_xml::events::Event;
use quick_xml::Reader;
use reqwest::blocking::Client;

#[derive(Clone)]
pub struct ArxivSource {
    client: Client,
    limit: usize,
}

impl ArxivSource {
    pub fn new(limit: usize) -> Self {
        Self {
            client: Client::new(),
            limit,
        }
    }
}

impl PaperSource for ArxivSource {
    fn fetch(&self, brief: &ProjectBrief) -> Result<Vec<PaperRecord>> {
        let query = search_terms(brief)
            .into_iter()
            .map(|keyword| format!("all:\"{}\"", keyword))
            .collect::<Vec<_>>()
            .join("+AND+");
        let url = format!(
            "https://export.arxiv.org/api/query?search_query={query}&start=0&max_results={}",
            self.limit
        );
        let body = self
            .client
            .get(url)
            .send()
            .context("send arXiv request")?
            .error_for_status()
            .context("arXiv returned error status")?
            .text()
            .context("read arXiv response body")?;
        parse_arxiv_feed(&body)
    }
}

fn parse_arxiv_feed(xml: &str) -> Result<Vec<PaperRecord>> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut in_entry = false;
    let mut current_tag = String::new();
    let mut entries = Vec::new();
    let mut entry = ArxivEntry::default();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(e)) => {
                current_tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if current_tag == "entry" {
                    in_entry = true;
                    entry = ArxivEntry::default();
                }
            }
            Ok(Event::End(e)) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "entry" && in_entry {
                    entries.push(entry.clone());
                    in_entry = false;
                }
                current_tag.clear();
            }
            Ok(Event::Text(e)) if in_entry => {
                let text = e
                    .unescape()
                    .context("unescape arXiv xml text")?
                    .into_owned();
                match current_tag.as_str() {
                    "title" => entry.title = text,
                    "id" => entry.id = text,
                    "summary" => entry.summary = Some(text),
                    "published" => entry.published = Some(text),
                    "name" => entry.authors.push(text),
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(err) => return Err(anyhow!("parse arXiv feed: {err}")),
            _ => {}
        }
        buffer.clear();
    }

    Ok(entries
        .into_iter()
        .filter(|entry| !entry.title.is_empty())
        .map(|entry| {
            let arxiv_id = entry
                .id
                .rsplit('/')
                .next()
                .unwrap_or(entry.id.as_str())
                .to_string();
            let year = entry
                .published
                .as_deref()
                .and_then(|published| published.get(0..4))
                .and_then(|year| year.parse::<u16>().ok());

            PaperRecord {
                paper_id: PaperId::Arxiv(arxiv_id),
                title: entry.title.replace('\n', " ").trim().to_string(),
                authors: entry.authors,
                year,
                abstract_text: entry
                    .summary
                    .map(|summary| summary.replace('\n', " ").trim().to_string()),
                source_url: Some(entry.id),
                source_name: "arxiv".to_string(),
            }
        })
        .collect())
}

#[derive(Default, Clone)]
struct ArxivEntry {
    id: String,
    title: String,
    summary: Option<String>,
    published: Option<String>,
    authors: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::parse_arxiv_feed;

    #[test]
    fn parses_basic_arxiv_feed() {
        let xml = r#"
        <feed xmlns="http://www.w3.org/2005/Atom">
          <entry>
            <id>http://arxiv.org/abs/1234.5678v1</id>
            <title>Sample Paper</title>
            <summary>Sample summary</summary>
            <published>2024-01-01T00:00:00Z</published>
            <author><name>Alice</name></author>
          </entry>
        </feed>
        "#;
        let papers = parse_arxiv_feed(xml).unwrap();
        assert_eq!(papers.len(), 1);
        assert_eq!(papers[0].title, "Sample Paper");
    }
}
