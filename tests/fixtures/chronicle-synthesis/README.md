# Chronicle bounded synthesis fixture

This fictional corpus is for synthesis evaluation, not exact answer matching. It
spreads the Ember Kingdom's founding, war, relocation, and decline across eight
notes. The corpus topology intentionally includes multi-document evidence,
lexical distractors, duplicate evidence, and a chronological gap. The Ashen War
records a material disagreement, while the River Flood note records the gap.

`suite.toml` declares expected planner routes plus required facts, prohibited
claims, and gaps that a future model-backed evaluator should check semantically.
It intentionally does not prescribe answer wording or visible citations.

Run the model-backed evaluation with:

```text
cargo run -- --chronicle-synthesis-eval tests/fixtures/chronicle-synthesis/suite.toml /tmp/chronicle-synthesis-report.json
```

The evaluator runs the real planner and bounded map/reduce pipeline. Its rubric
scoring is deterministic: required facts and expected gaps are whitespace- and
case-insensitive containment checks, while prohibited claims are reported as
failures. This makes regressions reproducible without allowing the model to
grade its own answer.
