//! Full-text index and entity dictionary for the Memory memory daemon (SPEC.md §4, design doc 5.2).
//!
//! * [`FullText`]: one Tantivy index per tenant holding both turns and memories (`kind` = `turn` |
//!   `memory`), body analysed with the `memory` analyzer ([`analyze`]), BM25 scoring, exact-term filters.
//! * [`EntityDict`]: in-memory alias dictionary with normalised exact lookup and leftmost-longest matching.

mod analyzer;
mod entity;

use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use anyhow::{bail, Context, Result};
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::{BooleanQuery, ConstScoreQuery, Occur, Query, RangeQuery, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, FAST, INDEXED, STORED,
    STRING,
};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

pub use analyzer::{
    analyze, analyze_stream, analyze_with_offsets, is_stop_word, warm_up, MemoryTokenizer,
    ANALYZER_NAME, ANALYZER_VERSION,
};
pub use entity::{normalize_name, EntityDict};

/// Bump when the Tantivy schema changes (stored in the index directory next to the analyzer version).
pub const SCHEMA_VERSION: &str = "ft-schema-1";

/// Writer memory budget (single indexing thread).
const WRITER_MEMORY_BYTES: usize = 50_000_000;

/// Marker file in the index directory: analyzer + schema version the index was built with.
const VERSION_FILE: &str = "memory_fulltext.version";

#[derive(Debug, Clone, Default)]
pub struct FtDoc {
    pub id: String,
    /// `turn` | `memory`
    pub kind: String,
    pub mtype: String,
    pub status: String,
    pub scope_kind: String,
    pub project: String,
    pub platform: String,
    pub session: String,
    /// unix seconds
    pub effective_at: i64,
    pub body: String,
}

/// Search filter. Every non-empty constraint must hold (AND); values inside one constraint are OR-ed.
#[derive(Debug, Clone, Default)]
pub struct FtFilter {
    /// any-of on `kind`
    pub kinds: Vec<String>,
    /// any-of on `mtype`
    pub types: Vec<String>,
    /// any-of on `status`
    pub statuses: Vec<String>,
    /// OR-ed `(field, value)` pairs, e.g. `("scope_kind","global"), ("project","p1"), ("platform","codex")`.
    /// Fields: `id`, `kind`, `mtype`/`type`, `status`, `scope_kind`, `project`, `platform`, `session`.
    /// Empty = no visibility filter.
    pub visible: Vec<(String, String)>,
    /// inclusive `[from, to]` on `effective_at` (unix seconds)
    pub time_range: Option<(i64, i64)>,
}

#[derive(Clone, Copy)]
struct Fields {
    id: Field,
    kind: Field,
    mtype: Field,
    status: Field,
    scope_kind: Field,
    project: Field,
    platform: Field,
    session: Field,
    effective_at: Field,
    body: Field,
}

fn build_schema() -> (Schema, Fields) {
    let mut sb = Schema::builder();
    let id = sb.add_text_field("id", STRING | STORED);
    let kind = sb.add_text_field("kind", STRING);
    let mtype = sb.add_text_field("mtype", STRING);
    let status = sb.add_text_field("status", STRING);
    let scope_kind = sb.add_text_field("scope_kind", STRING);
    let project = sb.add_text_field("project", STRING);
    let platform = sb.add_text_field("platform", STRING);
    let session = sb.add_text_field("session", STRING);
    let effective_at = sb.add_i64_field("effective_at", INDEXED | FAST);
    let body_opts = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(ANALYZER_NAME)
            .set_fieldnorms(true)
            .set_index_option(IndexRecordOption::WithFreqs),
    );
    let body = sb.add_text_field("body", body_opts);
    let f = Fields {
        id,
        kind,
        mtype,
        status,
        scope_kind,
        project,
        platform,
        session,
        effective_at,
        body,
    };
    (sb.build(), f)
}

struct WriterState {
    writer: IndexWriter,
    /// uncommitted changes exist
    dirty: bool,
}

/// Tantivy-backed BM25 index shared by turns and memories. `Send + Sync`; all methods take `&self`.
pub struct FullText {
    dir: PathBuf,
    index: Index,
    reader: IndexReader,
    writer: Mutex<WriterState>,
    f: Fields,
    needs_rebuild: bool,
}

fn version_stamp() -> String {
    format!("{ANALYZER_VERSION}\n{SCHEMA_VERSION}\n")
}

/// Removes everything inside `dir` (keeps the directory itself).
fn wipe_dir(dir: &Path) -> Result<()> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

fn open_index(dir: &Path, schema: &Schema) -> Result<(Index, IndexReader)> {
    let mmap = MmapDirectory::open(dir).context("open index directory")?;
    let index =
        Index::open_or_create(mmap, schema.clone()).context("open or create tantivy index")?;
    index.tokenizers().register(
        ANALYZER_NAME,
        TextAnalyzer::from(MemoryTokenizer::default()),
    );
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()
        .context("open index reader")?;
    Ok((index, reader))
}

impl FullText {
    /// Opens (or creates) the index in `dir`. If the directory holds an index built with another analyzer
    /// or schema version, or one that cannot be opened, it is wiped and recreated empty; in that case, and
    /// when the index is new, [`FullText::needs_rebuild`] returns true so the caller can repopulate it
    /// from the primary records.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        analyzer::warm_up();
        let (schema, f) = build_schema();
        let stamp_path = dir.join(VERSION_FILE);
        let existed = dir.join("meta.json").exists();
        let mut reset = false;
        if existed {
            let stamp = std::fs::read_to_string(&stamp_path).unwrap_or_default();
            if stamp != version_stamp() {
                tracing::warn!(
                    dir = %dir.display(),
                    have = %stamp.trim().replace('\n', " / "),
                    want = %version_stamp().trim().replace('\n', " / "),
                    "fulltext index built with another analyzer/schema version: recreating"
                );
                wipe_dir(dir)?;
                reset = true;
            }
        }
        let (index, reader) = match open_index(dir, &schema) {
            Ok(x) => x,
            Err(e) if existed && !reset => {
                tracing::warn!(dir = %dir.display(), error = %format!("{e:#}"), "fulltext index unreadable: recreating");
                wipe_dir(dir)?;
                reset = true;
                open_index(dir, &schema)?
            }
            Err(e) => return Err(e),
        };
        let tmp = dir.join(format!("{VERSION_FILE}.tmp"));
        std::fs::write(&tmp, version_stamp())?;
        std::fs::rename(&tmp, &stamp_path)?;
        let writer: IndexWriter = index
            .writer_with_num_threads(1, WRITER_MEMORY_BYTES)
            .context("create index writer (is another process using this index?)")?;
        Ok(Self {
            dir: dir.to_path_buf(),
            index,
            reader,
            writer: Mutex::new(WriterState {
                writer,
                dirty: false,
            }),
            f,
            needs_rebuild: reset || !existed,
        })
    }

    /// True when `open` created the index empty (new directory, or an incompatible/corrupt index was
    /// discarded): the caller should re-index from the primary records.
    pub fn needs_rebuild(&self) -> bool {
        self.needs_rebuild
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn lock(&self) -> MutexGuard<'_, WriterState> {
        self.writer.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn to_doc(&self, d: &FtDoc) -> TantivyDocument {
        let f = &self.f;
        let mut doc = TantivyDocument::default();
        doc.add_text(f.id, &d.id);
        doc.add_text(f.kind, &d.kind);
        doc.add_text(f.mtype, &d.mtype);
        doc.add_text(f.status, &d.status);
        doc.add_text(f.scope_kind, &d.scope_kind);
        doc.add_text(f.project, &d.project);
        doc.add_text(f.platform, &d.platform);
        doc.add_text(f.session, &d.session);
        doc.add_i64(f.effective_at, d.effective_at);
        doc.add_text(f.body, &d.body);
        doc
    }

    /// Idempotent: deletes any document with the same `id`, then adds. Visible after [`FullText::commit`].
    /// Within one call a later doc with a repeated id wins.
    pub fn upsert(&self, docs: Vec<FtDoc>) -> Result<()> {
        if docs.is_empty() {
            return Ok(());
        }
        let mut st = self.lock();
        for d in &docs {
            st.writer
                .delete_term(Term::from_field_text(self.f.id, &d.id));
            st.writer.add_document(self.to_doc(d))?;
        }
        st.dirty = true;
        Ok(())
    }

    /// Deletes by id (missing ids are ignored). Visible after [`FullText::commit`].
    pub fn delete(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut st = self.lock();
        for id in ids {
            st.writer.delete_term(Term::from_field_text(self.f.id, id));
        }
        st.dirty = true;
        Ok(())
    }

    /// Commits pending changes and reloads the reader: changes are searchable when this returns.
    /// A no-op when nothing changed since the last commit (cheap to call every second).
    pub fn commit(&self) -> Result<()> {
        let mut st = self.lock();
        if !st.dirty {
            return Ok(());
        }
        st.writer.commit().context("tantivy commit")?;
        st.dirty = false;
        self.reader.reload().context("tantivy reader reload")?;
        Ok(())
    }

    /// Deletes every document and commits.
    pub fn clear(&self) -> Result<()> {
        let mut st = self.lock();
        st.writer.delete_all_documents()?;
        st.writer.commit().context("tantivy commit")?;
        st.dirty = false;
        self.reader.reload()?;
        Ok(())
    }

    /// Number of searchable (committed, not deleted) documents.
    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// Distinct analysed terms of `text` (same analyzer as indexing and querying). See [`analyze`].
    pub fn analyze(&self, text: &str) -> Vec<String> {
        analyze(text)
    }

    fn filter_field(&self, name: &str) -> Result<Field> {
        let f = &self.f;
        Ok(match name {
            "id" => f.id,
            "kind" => f.kind,
            "mtype" | "type" => f.mtype,
            "status" => f.status,
            "scope_kind" | "scopeKind" => f.scope_kind,
            "project" => f.project,
            "platform" => f.platform,
            "session" => f.session,
            other => bail!("memory-fulltext: unknown filter field {other:?}"),
        })
    }

    /// The Tantivy query for `query` + `filter` (None when the query has no searchable terms).
    /// Built programmatically: user text never reaches a query parser.
    fn build_query(&self, query: &str, filter: &FtFilter) -> Result<Option<Box<dyn Query>>> {
        let terms = analyze(query);
        if terms.is_empty() {
            return Ok(None);
        }
        let text: Vec<(Occur, Box<dyn Query>)> = terms
            .iter()
            .map(|t| {
                let q: Box<dyn Query> = Box::new(TermQuery::new(
                    Term::from_field_text(self.f.body, t),
                    IndexRecordOption::WithFreqs,
                ));
                (Occur::Should, q)
            })
            .collect();
        let mut clauses: Vec<(Occur, Box<dyn Query>)> =
            vec![(Occur::Must, Box::new(BooleanQuery::new(text)))];

        let any_of = |pairs: Vec<(Field, &str)>| -> Box<dyn Query> {
            let should: Vec<(Occur, Box<dyn Query>)> = pairs
                .into_iter()
                .map(|(field, v)| {
                    let q: Box<dyn Query> = Box::new(TermQuery::new(
                        Term::from_field_text(field, v),
                        IndexRecordOption::Basic,
                    ));
                    (Occur::Should, q)
                })
                .collect();
            // filters must not change the BM25 score of the body match
            Box::new(ConstScoreQuery::new(
                Box::new(BooleanQuery::new(should)),
                0.0,
            ))
        };
        if !filter.kinds.is_empty() {
            clauses.push((
                Occur::Must,
                any_of(
                    filter
                        .kinds
                        .iter()
                        .map(|v| (self.f.kind, v.as_str()))
                        .collect(),
                ),
            ));
        }
        if !filter.types.is_empty() {
            clauses.push((
                Occur::Must,
                any_of(
                    filter
                        .types
                        .iter()
                        .map(|v| (self.f.mtype, v.as_str()))
                        .collect(),
                ),
            ));
        }
        if !filter.statuses.is_empty() {
            clauses.push((
                Occur::Must,
                any_of(
                    filter
                        .statuses
                        .iter()
                        .map(|v| (self.f.status, v.as_str()))
                        .collect(),
                ),
            ));
        }
        if !filter.visible.is_empty() {
            let mut pairs = Vec::with_capacity(filter.visible.len());
            for (k, v) in &filter.visible {
                pairs.push((self.filter_field(k)?, v.as_str()));
            }
            clauses.push((Occur::Must, any_of(pairs)));
        }
        if let Some((from, to)) = filter.time_range {
            let r = RangeQuery::new(
                Bound::Included(Term::from_field_i64(self.f.effective_at, from)),
                Bound::Included(Term::from_field_i64(self.f.effective_at, to)),
            );
            clauses.push((
                Occur::Must,
                Box::new(ConstScoreQuery::new(Box::new(r), 0.0)),
            ));
        }
        Ok(Some(Box::new(BooleanQuery::new(clauses))))
    }

    /// BM25 search: OR of the analysed query terms over `body`, restricted by `filter`.
    /// Returns `(id, score)` best first; empty for an empty / stop-word-only query or `limit == 0`.
    pub fn search(
        &self,
        query: &str,
        filter: &FtFilter,
        limit: usize,
    ) -> Result<Vec<(String, f32)>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        if let Some((from, to)) = filter.time_range {
            if from > to {
                return Ok(Vec::new());
            }
        }
        let Some(q) = self.build_query(query, filter)? else {
            return Ok(Vec::new());
        };
        let searcher = self.reader.searcher();
        let top = searcher.search(&*q, &TopDocs::with_limit(limit).order_by_score())?;
        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            if let Some(id) = doc.get_first(self.f.id).and_then(|v| v.as_str()) {
                out.push((id.to_string(), score));
            }
        }
        Ok(out)
    }

    /// The underlying Tantivy index (diagnostics).
    pub fn index(&self) -> &Index {
        &self.index
    }
}

#[cfg(test)]
mod tests;
