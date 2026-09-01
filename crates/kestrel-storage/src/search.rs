//! `SearchService`: structured queries → Tantivy → hydrated hits
//! (architecture §4.3). Stateless-render: authoritative data is hydrated
//! from `SQLite`; the index provides ids + scores + snippets.

use std::sync::Arc;

use kestrel_core::{
    error::KestrelError,
    protocol::{SearchHit, SearchQuery},
};
use tantivy::{
    Searcher,
    collector::TopDocs,
    query::{BooleanQuery, FuzzyTermQuery, Occur, Query, TermQuery},
    schema::{IndexRecordOption, Value as _},
    snippet::SnippetGenerator,
};
use tracing::instrument;

use crate::{
    error::{StorageError, StorageResult},
    index::{SharedIndex, query_parser},
    store::StorageHandle,
};

/// Maximum hits per query (bounded by construction).
pub const MAX_HITS: usize = 200;

/// Cloneable search handle.
#[derive(Clone)]
pub struct SearchHandle {
    shared: Arc<SharedIndex>,
    storage: StorageHandle,
}

impl SearchHandle {
    /// Executes a structured query.
    ///
    /// # Errors
    /// [`KestrelError`] on index/storage failure.
    #[instrument(skip_all)]
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<SearchHit>, KestrelError> {
        let limit = usize::try_from(query.limit.unwrap_or(50))
            .unwrap_or(50)
            .min(MAX_HITS);
        let searcher = self.shared.reader.searcher();

        let parsed =
            build_query(&self.shared, &searcher, query).map_err(|e| KestrelError::StorageIo {
                detail: e.to_string(),
            })?;
        let top: Vec<tantivy::DocAddress> = if parsed_is_match_all(query) {
            searcher
                .search(
                    &parsed,
                    &TopDocs::with_limit(limit)
                        .order_by_fast_field::<i64>("date", tantivy::Order::Desc),
                )
                .map_err(|e| StorageError::Index(e.to_string()))?
                .into_iter()
                .map(|(_, addr)| addr)
                .collect()
        } else {
            searcher
                .search(&parsed, &TopDocs::with_limit(limit).order_by_score())
                .map_err(|e| StorageError::Index(e.to_string()))?
                .into_iter()
                .map(|(_, addr)| addr)
                .collect()
        };

        // Snippets from the stored subject field.
        let snippet_gen =
            SnippetGenerator::create(&searcher, parsed.as_ref(), self.shared.fields.subject).ok();

        let mut hits = Vec::with_capacity(top.len());
        for doc_addr in top {
            let Ok(doc) = searcher.doc::<tantivy::TantivyDocument>(doc_addr) else {
                continue;
            };
            let Some(id) = doc
                .get_first(self.shared.fields.msg_id)
                .and_then(|v| v.as_str())
                .and_then(kestrel_core::ids::MessageId::parse)
            else {
                continue;
            };
            // Authoritative row from SQLite (events-as-hints doctrine);
            // skip missing rows (deleted between index and search).
            let Ok(load) = self.storage.get_message(id).await else {
                continue;
            };
            let snippet = snippet_gen
                .as_ref()
                .map(|sg| sg.snippet_from_doc(&doc).to_html());
            hits.push(SearchHit {
                message: load.view.summary,
                snippet,
            });
        }
        Ok(hits)
    }
}

fn parsed_is_match_all(q: &SearchQuery) -> bool {
    q.is_empty()
}

/// Builds a fuzzy query for text terms across all text fields.
///
/// Each term is matched fuzzily (Levenshtein distance) against the text,
/// subject, from, to, cc, and `attachment_names` fields using `Should`
/// (OR) semantics — any field match suffices per term.
fn build_fuzzy_text_query(shared: &SharedIndex, text: &str) -> Box<dyn Query> {
    let fields = [
        shared.fields.subject,
        shared.fields.body,
        shared.fields.from,
        shared.fields.to,
        shared.fields.cc,
        shared.fields.attachment_names,
    ];
    let terms: Vec<&str> = text.split_whitespace().collect();
    if terms.is_empty() {
        return Box::new(tantivy::query::AllQuery);
    }
    let mut term_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    for term_text in terms {
        let clean = term_text.trim_matches(|c: char| c == '"' || c == '\'' || c == '~');
        if clean.is_empty() {
            continue;
        }
        let distance: u8 = if clean.len() < 5 { 1 } else { 2 };
        let mut field_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for &field in &fields {
            let term = tantivy::Term::from_field_text(field, clean);
            let fuzzy = FuzzyTermQuery::new(term, distance, true);
            field_clauses.push((Occur::Should, Box::new(fuzzy)));
        }
        if !field_clauses.is_empty() {
            term_clauses.push((Occur::Must, Box::new(BooleanQuery::from(field_clauses))));
        }
    }
    if term_clauses.is_empty() {
        return Box::new(tantivy::query::AllQuery);
    }
    Box::new(BooleanQuery::from(term_clauses))
}

/// Determines whether the text contains tilde-suffixed terms.
fn has_tilde_terms(text: &str) -> bool {
    text.split_whitespace().any(|t| t.ends_with('~'))
}

/// Builds the Tantivy query for a structured search.
fn build_query(
    shared: &SharedIndex,
    _searcher: &Searcher,
    q: &SearchQuery,
) -> StorageResult<Box<dyn Query>> {
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

    push_text_clause(shared, q, &mut clauses)?;
    push_address_clauses(shared, q, &mut clauses);
    push_subject_clause(shared, q, &mut clauses)?;
    push_date_clause(shared, q, &mut clauses);
    if !push_facet_clauses(shared, q, &mut clauses) {
        return Ok(Box::new(BooleanQuery::from(vec![])));
    }
    push_attachment_clause(shared, q, &mut clauses);

    if clauses.is_empty() {
        return Ok(Box::new(tantivy::query::AllQuery));
    }
    Ok(Box::new(BooleanQuery::from(clauses)))
}

fn push_text_clause(
    shared: &SharedIndex,
    q: &SearchQuery,
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
) -> StorageResult<()> {
    let Some(text) = q.text.as_deref().filter(|t| !t.trim().is_empty()) else {
        return Ok(());
    };
    if q.fuzzy || has_tilde_terms(text) {
        clauses.push((Occur::Must, build_fuzzy_text_query(shared, text)));
    } else {
        let parser = query_parser(&shared.index, &shared.fields);
        let parsed = parser
            .parse_query(text)
            .or_else(|_| {
                let joined = text
                    .split_whitespace()
                    .map(|t| t.replace('"', ""))
                    .collect::<Vec<_>>()
                    .join(" ");
                parser.parse_query(&joined)
            })
            .map_err(|e| StorageError::Index(format!("query parse: {e}")))?;
        clauses.push((Occur::Must, Box::new(parsed)));
    }
    Ok(())
}

fn push_address_clauses(
    shared: &SharedIndex,
    q: &SearchQuery,
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
) {
    for (val, field) in [
        (q.from.as_deref(), shared.fields.from_exact),
        (q.to.as_deref(), shared.fields.to_exact),
    ] {
        if let Some(addr) = val.filter(|v| !v.trim().is_empty()) {
            let exact = TermQuery::new(
                tantivy::Term::from_field_text(field, &addr.to_lowercase()),
                IndexRecordOption::Basic,
            );
            clauses.push((Occur::Must, Box::new(exact)));
        }
    }
}

fn push_subject_clause(
    shared: &SharedIndex,
    q: &SearchQuery,
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
) -> StorageResult<()> {
    let Some(subject) = q.subject.as_deref().filter(|s| !s.trim().is_empty()) else {
        return Ok(());
    };
    let parser = query_parser(&shared.index, &shared.fields);
    let parsed = parser
        .parse_query(&format!("subject:({subject})"))
        .or_else(|_| parser.parse_query(subject))
        .map_err(|e| StorageError::Index(format!("subject parse: {e}")))?;
    clauses.push((Occur::Must, Box::new(parsed)));
    Ok(())
}

fn push_date_clause(
    shared: &SharedIndex,
    q: &SearchQuery,
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
) {
    if q.since.is_some() || q.until.is_some() {
        use tantivy::query::RangeQuery;
        let lower = q.since.map_or(std::ops::Bound::Unbounded, |ms| {
            std::ops::Bound::Included(tantivy::Term::from_field_i64(shared.fields.date, ms))
        });
        let upper = q.until.map_or(std::ops::Bound::Unbounded, |ms| {
            std::ops::Bound::Included(tantivy::Term::from_field_i64(shared.fields.date, ms))
        });
        clauses.push((Occur::Must, Box::new(RangeQuery::new(lower, upper))));
    }
}

fn push_facet_clauses(
    shared: &SharedIndex,
    q: &SearchQuery,
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
) -> bool {
    if let Some(folder) = q.folder {
        let facet = shared
            .map
            .try_read()
            .ok()
            .and_then(|map| map.folders.get(&folder.to_string()).copied());
        match facet {
            Some(v) => {
                let term = TermQuery::new(
                    tantivy::Term::from_field_u64(shared.fields.folder, v),
                    IndexRecordOption::Basic,
                );
                clauses.push((Occur::Must, Box::new(term)));
            }
            None => return false,
        }
    }
    if let Some(account) = q.account {
        let facet = shared
            .map
            .try_read()
            .ok()
            .and_then(|map| map.accounts.get(&account.to_string()).copied());
        match facet {
            Some(v) => {
                let term = TermQuery::new(
                    tantivy::Term::from_field_u64(shared.fields.account, v),
                    IndexRecordOption::Basic,
                );
                clauses.push((Occur::Must, Box::new(term)));
            }
            None => return false,
        }
    }
    true
}

fn push_attachment_clause(
    shared: &SharedIndex,
    q: &SearchQuery,
    clauses: &mut Vec<(Occur, Box<dyn Query>)>,
) {
    if q.has_attachment {
        let term = TermQuery::new(
            tantivy::Term::from_field_u64(shared.fields.has_attachment, 1),
            IndexRecordOption::Basic,
        );
        clauses.push((Occur::Must, Box::new(term)));
    }
}

/// `SearchService`: builds the search handle from the index + storage handles.
pub struct SearchService;

impl SearchService {
    /// Creates the search handle bound to a live index service + storage.
    #[must_use]
    pub fn from_index(index: &crate::index::IndexHandle, storage: StorageHandle) -> SearchHandle {
        SearchHandle {
            shared: crate::index::shared_of(index),
            storage,
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn tilde_terms_detected() {
        assert!(has_tilde_terms("budget~"));
        assert!(has_tilde_terms("hello world~"));
        assert!(has_tilde_terms("foo bar~ baz"));
        assert!(!has_tilde_terms("budget"));
        assert!(!has_tilde_terms(""));
        assert!(!has_tilde_terms("  "));
    }

    #[test]
    fn fuzzy_text_query_uses_correct_edit_distance() {
        let dir = tempfile::tempdir().unwrap();
        let shared = Arc::new(SharedIndex::open(dir.path()).unwrap());

        // Short term (< 5 chars) → distance 1
        let q = build_fuzzy_text_query(&shared, "foo");
        let searcher = shared.reader.searcher();
        let count = searcher.search(&q, &tantivy::collector::Count).unwrap();
        assert_eq!(count, 0, "empty index returns zero hits");

        // Long term (>= 5 chars) → distance 2
        let q = build_fuzzy_text_query(&shared, "budget");
        let count = searcher.search(&q, &tantivy::collector::Count).unwrap();
        assert_eq!(count, 0, "empty index returns zero hits");
    }

    #[test]
    fn search_query_fuzzy_field_defaults_false() {
        let q = SearchQuery::default();
        assert!(!q.fuzzy);
    }

    #[test]
    fn search_query_fuzzy_field_serde_roundtrip() {
        let q = SearchQuery {
            text: Some("budget~".into()),
            fuzzy: true,
            ..SearchQuery::default()
        };
        let json = serde_json::to_string(&q).unwrap();
        let deser: SearchQuery = serde_json::from_str(&json).unwrap();
        assert!(deser.fuzzy);
        assert_eq!(deser.text.as_deref(), Some("budget~"));
    }

    #[test]
    fn search_query_fuzzy_field_backward_compatible() {
        // Old JSON without "fuzzy" field must deserialize with fuzzy=false.
        let json = r#"{"text":"hello"}"#;
        let q: SearchQuery = serde_json::from_str(json).unwrap();
        assert!(!q.fuzzy);
        assert_eq!(q.text.as_deref(), Some("hello"));
    }

    #[test]
    fn search_query_empty_with_fuzzy_is_not_empty() {
        let q = SearchQuery {
            fuzzy: true,
            ..SearchQuery::default()
        };
        assert!(!q.is_empty());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(kestrel_core::testkit::proptest_cases()))]

        #[test]
        fn search_query_json_roundtrip(
            text in proptest::option::of("[a-z ]{0,50}"),
            from in proptest::option::of("[a-z@.]{0,30}"),
            to in proptest::option::of("[a-z@.]{0,30}"),
            subject in proptest::option::of("[a-z ]{0,40}"),
            since in proptest::option::of(-1_000_000_000_000i64..1_000_000_000_000),
            until in proptest::option::of(-1_000_000_000_000i64..1_000_000_000_000),
            has_attachment in proptest::prelude::any::<bool>(),
            fuzzy in proptest::prelude::any::<bool>(),
            limit in proptest::option::of(1u64..500),
        ) {
            let q = SearchQuery {
                text, from, to, subject, since, until,
                folder: None, account: None,
                has_attachment, limit, fuzzy,
            };
            let json = serde_json::to_string(&q).unwrap();
            let deser: SearchQuery = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(&q, &deser);
            // is_empty consistency
            let is_actually_empty = q.text.is_none() && q.from.is_none() && q.to.is_none()
                && q.subject.is_none() && q.since.is_none() && q.until.is_none()
                && q.folder.is_none() && q.account.is_none() && !q.has_attachment && !q.fuzzy;
            prop_assert_eq!(q.is_empty(), is_actually_empty);
        }
    }
}
