use std::collections::BTreeSet;

use regex::Regex;

use crate::retrieval::RetrievedChunk;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub document_id: String,
    pub page: usize,
}

#[derive(Debug, Clone, Default)]
pub struct CitationReport {
    pub valid: bool,
    pub citations: Vec<Citation>,
    pub invalid: Vec<String>,
    pub had_citations: bool,
}

pub fn validate_citations(answer: &str, chunks: &[RetrievedChunk]) -> CitationReport {
    let pattern = Regex::new(r"(?i)\[doc:([A-Za-z0-9._:-]{1,128})\s+p\.?\s*(\d{1,6})\]")
        .expect("static citation regex");
    let allowed: BTreeSet<(String, usize)> = chunks
        .iter()
        .map(|chunk| (chunk.document_id.clone(), chunk.page))
        .collect();
    let mut citations = Vec::new();
    let mut invalid = Vec::new();
    for capture in pattern.captures_iter(answer) {
        let document_id = capture[1].to_owned();
        let page = capture[2].parse::<usize>().unwrap_or(0);
        if allowed.contains(&(document_id.clone(), page)) {
            citations.push(Citation { document_id, page });
        } else {
            invalid.push(capture[0].to_owned());
        }
    }
    CitationReport {
        valid: invalid.is_empty() && !citations.is_empty(),
        had_citations: !citations.is_empty() || !invalid.is_empty(),
        citations,
        invalid,
    }
}

pub fn evidence_prompt(question: &str, chunks: &[RetrievedChunk]) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "Answer only from the evidence below. Cite every factual claim with the exact tag \
         [doc:<document_id> p.<page>]. If the evidence is insufficient, say you cannot find \
         it in the indexed documents. Never invent document ids or pages.\n\nEVIDENCE\n",
    );
    for chunk in chunks {
        prompt.push_str(&format!(
            "[doc:{} p.{}] {}\n{}\n\n",
            chunk.document_id, chunk.page, chunk.document_title, chunk.text
        ));
    }
    prompt.push_str("QUESTION\n");
    prompt.push_str(question);
    prompt
}

pub const ABSTENTION: &str = "I could not verify that answer against the indexed documents, so I am not providing a citation.";

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: &str, page: usize) -> RetrievedChunk {
        RetrievedChunk {
            chunk_id: format!("{id}-{page}"),
            document_id: id.into(),
            document_title: "Synthetic".into(),
            page,
            text: "evidence".into(),
            score: 1.0,
        }
    }

    #[test]
    fn accepts_only_citations_from_retrieved_evidence() {
        let chunks = vec![chunk("doc-a", 3)];
        let report = validate_citations("Claim [doc:doc-a p.3].", &chunks);
        assert!(report.valid);
        assert_eq!(report.citations.len(), 1);
    }

    #[test]
    fn rejects_invented_page_and_document() {
        let chunks = vec![chunk("doc-a", 3)];
        assert!(!validate_citations("Claim [doc:doc-a p.4].", &chunks).valid);
        assert!(!validate_citations("Claim [doc:missing p.3].", &chunks).valid);
    }

    #[test]
    fn rejects_uncited_answers() {
        let chunks = vec![chunk("doc-a", 3)];
        assert!(!validate_citations("An uncited claim.", &chunks).valid);
    }

    #[test]
    fn evidence_prompt_marks_page_provenance() {
        let prompt = evidence_prompt("What?", &[chunk("doc-a", 7)]);
        assert!(prompt.contains("[doc:doc-a p.7]"));
        assert!(!prompt.contains("p.8"));
    }
}
