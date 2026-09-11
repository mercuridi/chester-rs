use super::*;

#[test]
fn scores_paraphrase_insensitively_and_flags_prohibited_claims() {
    let answer = "The kingdom was founded when the river settlements ratified the Ember Compact.";
    assert_eq!(
        resolved_coverage(&claim_results(
            answer,
            &[FactExpectation {
                id: "foundation".into(),
                claim: "  founded when the river settlements ratified the Ember Compact ".into(),
                aliases: Vec::new(),
                entities: Vec::new(),
                category: ClaimCategory::Core,
            }],
            ExpectationKind::Required,
        )),
        1.0
    );
    assert_eq!(
        resolved_coverage(&claim_results(
            answer,
            &[FactExpectation {
                id: "foundation".into(),
                claim: "The kingdom was founded.".into(),
                aliases: vec!["The realm was established.".into()],
                entities: vec!["kingdom".into()],
                category: ClaimCategory::Core,
            }],
            ExpectationKind::Required,
        )),
        1.0
    );
    assert_eq!(
        claim_results(
            answer,
            &[ProhibitedExpectation {
                id: "still-exists".into(),
                claim: "the kingdom still exists".into(),
                aliases: Vec::new(),
                entities: Vec::new(),
            }],
            ExpectationKind::Prohibited,
        )
        .into_iter()
        .filter(|result| result.status == ClaimStatus::Contradicted)
        .map(|result| result.claim)
        .collect::<Vec<_>>(),
        Vec::<String>::new()
    );
    assert_eq!(
        claim_results(
            "The kingdom still exists.",
            &[ProhibitedExpectation {
                id: "still-exists".into(),
                claim: "the kingdom still exists".into(),
                aliases: Vec::new(),
                entities: Vec::new(),
            }],
            ExpectationKind::Prohibited,
        )
        .into_iter()
        .filter(|result| result.status == ClaimStatus::Contradicted)
        .map(|result| result.claim)
        .collect::<Vec<_>>()
        .len(),
        1
    );
}

#[test]
fn parses_legacy_string_rubrics_and_assigns_stable_ids() -> Result<()> {
    let suite: Suite = toml::from_str(
        r#"
name = "legacy"
minimum_required_fact_recall = 0.5
maximum_prohibited_claims = 0
minimum_gap_recall = 0.0

[topology]
document_count = 1
minimum_multi_document_cases = 1
required_categories = ["chronology"]

[safety]
gm_only_note = "gm"
draft_contradiction_note = "draft"
instruction_like_note = "instruction"
inaccessible_note = "inaccessible"
mixed_event_note = "mixed"

[[cases]]
id = "case"
question = "What happened?"
expected_route = "synthesis"
required_facts = ["A happened."]
prohibited_claims = ["B happened."]
expected_gaps = ["The interval is unknown."]
"#,
    )?;

    assert_eq!(suite.cases[0].required_facts[0].id, "required-fact-1");
    assert_eq!(suite.cases[0].prohibited_claims[0].id, "prohibited-claim-1");
    assert_eq!(suite.cases[0].expected_gaps[0].id, "expected-gap-1");
    Ok(())
}

#[test]
fn parses_structured_rubrics() -> Result<()> {
    let suite: Suite = toml::from_str(
        r#"
name = "structured"
minimum_required_fact_recall = 0.5
maximum_prohibited_claims = 0
minimum_gap_recall = 0.0

[topology]
document_count = 1
minimum_multi_document_cases = 1
required_categories = ["chronology"]

[safety]
gm_only_note = "gm"
draft_contradiction_note = "draft"
instruction_like_note = "instruction"
inaccessible_note = "inaccessible"
mixed_event_note = "mixed"

[[cases]]
id = "case"
question = "What happened?"
expected_route = "synthesis"

[[cases.required_facts]]
id = "foundation"
claim = "The kingdom was founded."
aliases = ["The realm was established."]
entities = ["kingdom"]

[[cases.prohibited_claims]]
id = "present-day"
claim = "The kingdom still exists."

[[cases.expected_gaps]]
id = "missing-interval"
claim = "The interval is unknown."
"#,
    )?;

    assert_eq!(suite.cases[0].required_facts[0].id, "foundation");
    assert_eq!(
        suite.cases[0].required_facts[0].claim,
        "The kingdom was founded."
    );
    assert_eq!(
        suite.cases[0].required_facts[0].aliases,
        vec!["The realm was established."]
    );
    assert_eq!(suite.cases[0].required_facts[0].entities, vec!["kingdom"]);
    Ok(())
}

#[test]
fn aliases_resolve_a_claim_and_report_the_matching_alias() {
    let expectation = FactExpectation {
        id: "crown-relocation".into(),
        claim: "The crown moved from Ashford to Lantern Bay.".into(),
        aliases: vec!["The court relocated to Lantern Bay.".into()],
        entities: vec!["Lantern Bay".into()],
        category: ClaimCategory::Core,
    };
    let results = claim_results(
        "After the flood, the court relocated to Lantern Bay.",
        &[expectation],
        ExpectationKind::Required,
    );

    assert_eq!(results[0].status, ClaimStatus::Resolved);
    assert_eq!(
        results[0].matched_by.as_deref(),
        Some("the court relocated to lantern bay")
    );
    assert!(results[0].missing_entities.is_empty());
}

#[test]
fn entity_checks_leave_an_otherwise_matching_alias_unresolved() {
    let expectation = FactExpectation {
        id: "crown-relocation".into(),
        claim: "The crown moved to Lantern Bay.".into(),
        aliases: vec!["The court relocated.".into()],
        entities: vec!["Lantern Bay".into()],
        category: ClaimCategory::Core,
    };
    let results = claim_results(
        "The court relocated after the flood.",
        &[expectation],
        ExpectationKind::Required,
    );

    assert_eq!(results[0].status, ClaimStatus::Unresolved);
    assert!(!results[0].matched);
    assert_eq!(results[0].missing_entities, vec!["Lantern Bay"]);
}

#[test]
fn negated_required_claim_is_clearly_contradicted() {
    let expectation = FactExpectation {
        id: "kingdom-survived".into(),
        claim: "The kingdom survived the Ashen War.".into(),
        aliases: vec!["The kingdom survive the Ashen War.".into()],
        entities: Vec::new(),
        category: ClaimCategory::Core,
    };
    let results = claim_results(
        "The kingdom did not survive the Ashen War.",
        &[expectation],
        ExpectationKind::Required,
    );

    assert_eq!(results[0].status, ClaimStatus::Contradicted);
    assert!(!results[0].matched);
}

#[test]
fn negated_prohibited_claim_is_resolved() {
    let expectation = ProhibitedExpectation {
        id: "kingdom-still-exists".into(),
        claim: "The Ember Kingdom still exists.".into(),
        aliases: Vec::new(),
        entities: vec!["Ember Kingdom".into()],
    };
    let results = claim_results(
        "The Ember Kingdom does not still exist.",
        &[expectation],
        ExpectationKind::Prohibited,
    );

    assert_eq!(results[0].status, ClaimStatus::Resolved);
    assert!(
        claim_results(
            "The Ember Kingdom does not still exist.",
            &[ProhibitedExpectation {
                id: "kingdom-still-exists".into(),
                claim: "The Ember Kingdom still exists.".into(),
                aliases: Vec::new(),
                entities: vec!["Ember Kingdom".into()],
            }],
            ExpectationKind::Prohibited,
        )
        .into_iter()
        .filter(|result| result.status == ClaimStatus::Contradicted)
        .map(|result| result.claim)
        .collect::<Vec<_>>()
        .is_empty()
    );
}

#[test]
fn absent_prohibited_claim_is_resolved_and_present_one_is_contradicted() {
    let expectation = ProhibitedExpectation {
        id: "exhaustive".into(),
        claim: "The chronology is exhaustive.".into(),
        aliases: vec!["The records are complete.".into()],
        entities: Vec::new(),
    };
    let absent = claim_results(
        "The records leave parts of the chronology unknown.",
        std::slice::from_ref(&expectation),
        ExpectationKind::Prohibited,
    );
    let present = claim_results(
        "The chronology is exhaustive.",
        &[expectation],
        ExpectationKind::Prohibited,
    );

    assert_eq!(absent[0].status, ClaimStatus::Resolved);
    assert_eq!(present[0].status, ClaimStatus::Contradicted);
}

#[test]
fn judge_parser_rejects_unknown_fields_and_repairs_missing_targets() -> Result<()> {
    let targets = vec![JudgeTarget {
        id: "required_fact:foundation".into(),
        kind: "required_fact".into(),
        claim: "The kingdom was founded.".into(),
        aliases: Vec::new(),
        entities: Vec::new(),
        missing_entities: Vec::new(),
    }];
    assert!(parse_judge_output(
            r#"{"claims":[{"id":"required_fact:foundation","verdict":"entailed","confidence":0.9,"extra":true}]}"#,
            &targets
        )
        .is_err());
    let repaired = parse_judge_output(r#"{"claims":[],"unsupported_causal_claims":[]}"#, &targets)?;
    assert_eq!(repaired.claims.len(), 1);
    assert_eq!(repaired.claims[0].id, "required_fact:foundation");
    assert_eq!(repaired.claims[0].verdict, JudgeVerdict::Unknown);
    assert_eq!(repaired.claims[0].confidence, 0.0);
    assert!(
        parse_judge_output(
            r#"{"claims":[{"id":"required_fact:other","verdict":"unknown","confidence":0.5}]}"#,
            &targets
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn category_coverage_and_contradictions_are_reported_independently() {
    let result = |id: &str, category: Option<ClaimCategory>, status: ClaimStatus| ClaimResult {
        id: id.into(),
        claim: id.into(),
        category,
        matched: status == ClaimStatus::Resolved,
        status,
        matched_by: None,
        missing_entities: Vec::new(),
        judge_verdict: None,
        judge_confidence: None,
    };
    let required = vec![
        result("core", Some(ClaimCategory::Core), ClaimStatus::Resolved),
        result(
            "supporting",
            Some(ClaimCategory::Supporting),
            ClaimStatus::Unresolved,
        ),
    ];
    let gaps = vec![result(
        "caveat",
        Some(ClaimCategory::Caveat),
        ClaimStatus::Resolved,
    )];
    let prohibited = vec![result("prohibited", None, ClaimStatus::Contradicted)];

    assert_eq!(
        category_coverage(&required, &gaps, ClaimCategory::Core),
        1.0
    );
    assert_eq!(
        category_coverage(&required, &gaps, ClaimCategory::Supporting),
        0.0
    );
    assert_eq!(
        category_coverage(&required, &gaps, ClaimCategory::Caveat),
        1.0
    );
    assert_eq!(contradiction_count(&required, &prohibited, &gaps), 1);
}

#[tokio::test]
async fn judge_retries_invalid_json_and_records_retry_metadata() -> Result<()> {
    let model = std::sync::Arc::new(JudgeTestModel {
            responses: std::sync::Mutex::new(std::collections::VecDeque::from([
                "not json".into(),
                r#"{"claims":[{"id":"required_fact:foundation","verdict":"entailed","confidence":0.9}],"unsupported_causal_claims":[]}"#.into(),
            ])),
            prompts: std::sync::Mutex::new(Vec::new()),
        });
    let judge = SynthesisJudge::new(model.clone());
    let targets = vec![JudgeTarget {
        id: "required_fact:foundation".into(),
        kind: "required_fact".into(),
        claim: "The kingdom was founded.".into(),
        aliases: Vec::new(),
        entities: Vec::new(),
        missing_entities: Vec::new(),
    }];

    let evaluation = judge
        .judge("What happened?", "The kingdom was founded.", &targets)
        .await?;
    assert_eq!(evaluation.claims.len(), 1);
    assert_eq!(evaluation.metadata.status, JudgeStatus::Succeeded);
    assert_eq!(evaluation.metadata.attempts, 2);
    assert_eq!(evaluation.metadata.retry_count, 1);
    assert!(model.prompts.lock().unwrap()[1].contains("not json"));
    Ok(())
}

struct JudgeTestModel {
    responses: std::sync::Mutex<std::collections::VecDeque<String>>,
    prompts: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl LanguageModel for JudgeTestModel {
    fn prompt_token_budget(&self) -> usize {
        1_000
    }

    fn count_input_tokens(&self, prompt: &str) -> Result<usize> {
        Ok(prompt.len())
    }

    async fn generate(&self, prompt: &str) -> Result<String> {
        self.prompts.lock().unwrap().push(prompt.to_owned());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .context("test model ran out of responses")
    }

    async fn classify_route(&self, _question: &str) -> Result<String> {
        Ok(r#"{"operation":"synthesis"}"#.into())
    }

    #[allow(clippy::unreachable)]
    async fn generate_structured_plan(
        &self,
        _question: &str,
        _operation: crate::chronicle::query::plan::RouteOperation,
    ) -> Result<String> {
        unreachable!("synthesis test model should not generate a structured plan")
    }

    #[allow(clippy::unreachable)]
    async fn repair_structured_plan(
        &self,
        _question: &str,
        _operation: crate::chronicle::query::plan::RouteOperation,
        _rejected_response: &str,
        _rejection_error: &str,
    ) -> Result<String> {
        unreachable!("synthesis test model should not repair a structured plan")
    }

    async fn load(&self) -> Result<()> {
        Ok(())
    }

    async fn unload(&self) -> Result<()> {
        Ok(())
    }
}
