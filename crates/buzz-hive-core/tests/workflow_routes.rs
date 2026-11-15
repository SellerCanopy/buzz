//! The results a stage may report.

use buzz_hive_core::domain::workflow::Stage;
use buzz_hive_core::workflow::decode::decode_stage;
use buzz_hive_core::workflow::routes::{allowed_results, reportable_results};

fn stage(text: &str) -> Stage {
    let (stage, _, findings) = decode_stage("s.yaml", text);
    assert_eq!(findings, vec![]);
    stage
}

#[test]
fn allowed_results_are_the_standard_ones_then_the_stages_own() {
    let s = Stage {
        results: vec!["blocked".into(), "passed".into()],
        ..Stage::default()
    };
    assert_eq!(
        allowed_results(&s),
        ["passed", "needs_work", "failed", "needs_input", "blocked"]
    );
}

#[test]
fn an_unrouted_needs_work_is_not_reportable() {
    assert_eq!(
        reportable_results(&Stage::default()),
        ["passed", "failed", "needs_input"]
    );
    let routed =
        stage("id: a\nkind: prompt\nagent: builder\nprompt: x\non: { needs_work: plan }\n");
    assert_eq!(
        reportable_results(&routed),
        ["passed", "needs_work", "failed", "needs_input"]
    );
}
