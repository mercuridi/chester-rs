# Chronicle retrieval evaluation

This fixed fictional corpus contains 12 notes (11 canon and one excluded draft)
and 12 questions: exact names, aliases, paraphrases, factual lookups, distractors,
and two questions without an answer in the indexed corpus. It is deliberately
small: a reproducible smoke baseline, not proof of quality on a full vault.

From the repository root:

```sh
cargo run -- --chronicle-eval
```

The runner writes a new `logs/evaluation/chronicle-report-YYYYMMDD-HHMMSS.json` file in the
repository automatically. If a report already exists for that timestamp,
it adds a numeric suffix. The suite defaults to
`tests/fixtures/chronicle/suite.toml`, but you can provide a different suite
path followed by an explicit report path when needed; existing files are never
overwritten.
It uses a temporary SQLite database, the production scanner/chunker/indexer,
the real BGE embedding model on CPU, and the production retrieval selection code.
It does not load application configuration, connect to Discord, open live databases,
or load an answer LLM. The embedding model must be cached or downloadable.
The temporary database is discarded after the run.

The suite's `[retrieval]` settings are grouped by responsibility:
`[retrieval.limits]` controls shortlist and context sizes,
`[retrieval.candidate_pool]` controls raw eligibility,
`[retrieval.fusion]` controls ranking weights, and
`[retrieval.selection]` controls duplicate handling and document diversity.

## Reading the report

The JSON report includes the model ID and resolved snapshot revision, a checksum
of the suite and indexed input notes, chunking/retrieval settings, per-case results,
and aggregate comparisons for lexical, vector, and hybrid retrieval. Compare runs
with the same input checksum and model revision when assessing code changes.
For parameter experiments, retain both reports and compare their recorded settings.

- `notes`: selected note IDs in chunk order; repeats mean multiple chunks from a note.
- `recall`: fraction of annotated relevant notes found within the selected chunks.
- `precision`: fraction of distinct selected notes annotated as relevant.
- `reciprocal_rank`: reciprocal of the first relevant **chunk** position, or zero.
- `evidence_coverage`: fraction of literal evidence snippets present in selected
  passages. This is a lightweight passage check, not a semantic answer score.
- `returned_for_unanswerable`: whether any candidates were returned for a question
  with no annotated answer. It does not claim an LLM hallucinated or abstained.
- `forbidden_evidence_returned`: whether a player/GM scope returned a passage
  containing text explicitly annotated as unavailable to that scope.
- `retrieval_ms`: query embedding plus both database searches, excluding indexing,
  model loading, selection, and generation.

Answerable cases contribute to mean recall, precision, and reciprocal rank.
Unanswerable cases are reported separately and never counted as zero-recall cases.
All three modes share the same result budget, deduplication and document cap.
Vector-only and hybrid apply the configured distance threshold to vector candidates.
Lexical-only and hybrid do not apply it to lexical candidates.

The process writes the report then exits unsuccessfully if hybrid mean recall is
below `minimum_hybrid_recall` or a `forbidden_evidence` annotation appears in any
mode. The shipped recall gate is 0.8. This does not check answer correctness or
unanswerable-question abstention.

## Diagnostics

Each candidate records its note ID, chunk index, raw vector rank/distance, lexical
rank, vector-threshold outcome, fused rank, RRF score and selection decision:
`selected`, `vector_threshold`, `exact_duplicate`, `near_duplicate`,
`document_cap`, or `result_limit`. Missing ranks mean the candidate was not in that
search's shortlist. Fused ranks are one-based; RRF uses the threshold-filtered vector
ranking. Diagnostic candidate arrays are sorted by identity; use rank fields to
inspect retrieval order. Only candidates fetched within `candidate_limit` appear.

These are retrieval-stage decisions before LLM prompt token budgeting. No scores,
rankings, or selection bookkeeping are inserted into model context. Normal chat
retrieval also emits this diagnostic structure at debug level, with document paths
instead of fixture IDs. Enable it with:

```sh
RUST_LOG=info,chester_rs::chronicle::indexer::retriever=debug cargo run
```

## Extending the suite

Add Markdown notes under `corpus/` and cases in `suite.toml`. Cases reference stable
frontmatter IDs, not filenames. `access` is `player` (the default) or `gm`.
`relevant_notes` lists all notes judged relevant; `evidence` contains exact
substrings from those notes. Use empty lists for unknown answers. Use
`forbidden_evidence` for text that must never appear in retrieved passages for the
case’s access scope. Duplicate case IDs, missing relevant IDs, or invalid evidence
annotations are errors. Keep category names descriptive for per-case comparisons.

Do not weaken annotations or thresholds merely to make a regression pass. Add new
cases for failures observed in real usage, using fictional equivalents of live data.

Fast tests run without embedding weights:

```sh
cargo test --bin chester-rs chronicle::
```

These cover scoring, fixture validation, lexical fixture retrieval, draft exclusion,
and diagnostic decisions. They do not replace the real-model command above.
