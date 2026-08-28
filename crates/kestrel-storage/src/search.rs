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
    query::{BooleanQuery, Occur, Query, TermQuery},
    schema::{IndexRecordOption, Value as _},
    snippet::SnippetGenerator,
};

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
            // Authoritative row from SQLite (events-as-hints doctrine).
            let load = self.storage.get_message(id).await?;
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

/// Builds the Tantivy query for a structured search.
fn build_query(
    shared: &SharedIndex,
    _searcher: &Searcher,
    q: &SearchQuery,
) -> StorageResult<Box<dyn Query>> {
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

    if let Some(text) = q.text.as_deref().filter(|t| !t.trim().is_empty()) {
        let parser = query_parser(&shared.index, &shared.fields);
        let parsed = parser
            .parse_query(text)
            .or_else(|_| {
                // Fallback: tokenize defensively and parse per token.
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
    if let Some(from) = q.from.as_deref().filter(|f| !f.trim().is_empty()) {
        let exact = TermQuery::new(
            tantivy::Term::from_field_text(shared.fields.from_exact, &from.to_lowercase()),
            IndexRecordOption::Basic,
        );
        clauses.push((Occur::Must, Box::new(exact)));
    }
    if let Some(to) = q.to.as_deref().filter(|t| !t.trim().is_empty()) {
        let exact = TermQuery::new(
            tantivy::Term::from_field_text(shared.fields.to_exact, &to.to_lowercase()),
            IndexRecordOption::Basic,
        );
        clauses.push((Occur::Must, Box::new(exact)));
    }
    if let Some(subject) = q.subject.as_deref().filter(|s| !s.trim().is_empty()) {
        let parser = query_parser(&shared.index, &shared.fields);
        let parsed = parser
            .parse_query(&format!("subject:({subject})"))
            .or_else(|_| parser.parse_query(subject))
            .map_err(|e| StorageError::Index(format!("subject parse: {e}")))?;
        clauses.push((Occur::Must, Box::new(parsed)));
    }
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
            None => {
                // Unknown folder facet ⇒ no results can match.
                return Ok(Box::new(BooleanQuery::from(vec![])));
            }
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
            None => return Ok(Box::new(BooleanQuery::from(vec![]))),
        }
    }
    if q.has_attachment {
        let term = TermQuery::new(
            tantivy::Term::from_field_u64(shared.fields.has_attachment, 1),
            IndexRecordOption::Basic,
        );
        clauses.push((Occur::Must, Box::new(term)));
    }

    if clauses.is_empty() {
        return Ok(Box::new(tantivy::query::AllQuery));
    }
    Ok(Box::new(BooleanQuery::from(clauses)))
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
