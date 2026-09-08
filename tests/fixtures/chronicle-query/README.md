# Structured count/list MVP evaluation

The fixture contains 10 canon characters, two locations, one organisation, one
deity, one monster, and an excluded draft character. Character records cover NPC,
PC, ex-PC, alive, dead, missing, explicitly unknown, and omitted metadata. It also
covers generic scalar equality, wikilink-list membership, string-list membership,
and conjunctions across the declared frontmatter taxonomy. The retrieval baseline
corpus is unchanged and remains a separate evaluation.

The suite covers 39 questions: legacy and generic supported counts/lists, zero
results, missing values, ordinary retrieval questions, unsupported restrictions, and
ambiguous follow-ups.

From the repository root, run the deterministic executor evaluation with defaults:

```sh
cargo run -- --chronicle-query-eval
```

This uses the fixture suite and creates a timestamped `chronicle-query-report-*.json`
in the current directory. You can override either path:

```sh
cargo run -- --chronicle-query-eval SUITE.toml REPORT.json
```

This uses a temporary database and requires no model, embeddings, application
configuration, Discord connection, or live notes. It validates expected plans and
checks exact totals and list IDs. Empty/missing properties do not satisfy a positive
filter. An explicit `character_status: unknown` is distinct from an omitted field.

To also measure the configured local LLM's interpretation:

```sh
cargo run -- --chronicle-query-eval --planner
```

The planner report also uses the default suite and a timestamped report path. Explicit
paths remain available with `--planner` last.

This additionally reads `.chronicle/config.toml` for the model configuration and
loads the model using the normal runtime/device path. It does not open the live
Chronicle database, index the live vault, or connect to Discord. It uses the same
planning prompt, JSON parser, and validation as `/chronicle ask`. Each generated
plan is compared to its expected plan; executor results are checked separately
using the expected plans. No final-answer generation is evaluated.

Reports record the input checksum, expected results, raw planner responses,
validated plans, parse/validation failures, and planner accuracy (null in
executor-only runs).
Use a new output filename each time. Failures in case results still write a report
and exit unsuccessfully; model-loading failures stop before evaluation.

The gate requires every executor result to match. With `--planner`, it additionally
requires at least 95% exact plan agreement and rejects any non-structured question
incorrectly accepted for structured execution, regardless of aggregate accuracy.
A recorded model identifier using `main` is not an immutable model snapshot;
compare the actual local model revision when comparing planner reports.

Run fast tests with:

```sh
cargo test --bin chester-rs chronicle::
```

These also cover metadata-only updates without embedding work, schema upgrades,
strict plan validation, dispatch/fallback, distinct counts, list caps, and response
lengths. They do not measure natural-language planner accuracy.

Planner reports include accuracy grouped by the `intent_family` field, so
regressions in synthesis paraphrases are visible even when aggregate accuracy
remains above the suite threshold.
