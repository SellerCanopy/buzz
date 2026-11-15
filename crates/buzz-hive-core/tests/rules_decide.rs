//! The routing and repeat rules, ported from `rules/decide_test.go`.
//!
//! The Go tests for `DecidePermission` and `LevelAsks` are not ported: Buzz decides what an
//! agent may do, so the engine has no permission levels (see `PORTING.md`).

use std::collections::BTreeMap;
use std::time::Duration;

use buzz_hive_core::domain::workflow::{
    Repeat, Route as RouteDef, Stage, RESULT_FAILED, RESULT_NEEDS_INPUT, RESULT_NEEDS_WORK,
    RESULT_PASSED,
};
use buzz_hive_core::rules::conditions::labels_met;
use buzz_hive_core::rules::decide::{
    loop_count, next_route, over_loop_limit, resolve_route, retry_decision, routes_backward, Route,
};
use buzz_hive_core::rules::repeat::{
    agent_done_decision, repeat_decision, RepeatState, RepeatStep, DEFAULT_MAX_ROUNDS,
    DEFAULT_STALL_AFTER,
};

fn stage(id: &str, on: &[(&str, &str)]) -> Stage {
    Stage {
        id: id.into(),
        on: on
            .iter()
            .map(|(result, to)| {
                (
                    (*result).to_owned(),
                    RouteDef {
                        to: (*to).to_owned(),
                        ..RouteDef::default()
                    },
                )
            })
            .collect(),
        ..Stage::default()
    }
}

/// `next_route` for the stage at `idx`, as the reducer calls it.
fn route(stages: &[Stage], idx: usize, result: &str) -> Route {
    let next = stages.get(idx + 1).map(|s| s.id.as_str());
    next_route(&stages[idx], next, result)
}

fn go(id: &str) -> Route {
    Route::Stage(id.into())
}

#[test]
fn next_route_follows_on_entries_then_the_file_order() {
    let stages = [
        stage("a", &[("blocked", "stop"), (RESULT_NEEDS_WORK, "a")]),
        stage("b", &[]),
        stage("c", &[(RESULT_PASSED, "a")]),
    ];
    let cases = [
        // (stage, result, route, backward)
        (0, RESULT_PASSED, go("b"), false), // default: the next stage
        (1, RESULT_PASSED, go("c"), false),
        (2, RESULT_PASSED, go("a"), true), // an on entry wins, and points back
        (0, RESULT_NEEDS_WORK, go("a"), true), // a route to itself is backward
        (0, "blocked", Route::Stop, false), // a custom result's route
        (1, RESULT_FAILED, Route::Ask, false), // default for the other results
        (1, RESULT_NEEDS_INPUT, Route::Ask, false),
    ];
    for (idx, result, want, back) in cases {
        let got = route(&stages, idx, result);
        assert_eq!(got, want, "stage {}, {result}", stages[idx].id);
        if let Route::Stage(to) = &got {
            assert_eq!(routes_backward(&stages, idx, to), back, "{to}");
        }
    }
    let only = [stage("only", &[])];
    assert_eq!(route(&only, 0, RESULT_PASSED), Route::Done);
}

#[test]
fn next_route_keeps_a_call() {
    let call = RouteDef {
        to: "b".into(),
        give: BTreeMap::from([("plan".to_owned(), "review".to_owned())]),
        then: "c".into(),
    };
    let mut a = stage("a", &[]);
    a.on.insert(RESULT_NEEDS_WORK.into(), call);
    let stages = [a, stage("b", &[]), stage("c", &[])];
    assert_eq!(
        route(&stages, 0, RESULT_NEEDS_WORK),
        Route::Call {
            to: "b".into(),
            give: BTreeMap::from([("plan".to_owned(), "review".to_owned())]),
            then: Some("c".into()),
        }
    );
    // The default route is a plain stage, not a call.
    assert_eq!(route(&stages, 0, RESULT_PASSED), go("b"));
}

#[test]
fn a_then_applies_only_when_the_stage_passed() {
    let mut a = stage("a", &[]);
    a.on.insert(
        RESULT_NEEDS_WORK.into(),
        RouteDef {
            to: "b".into(),
            give: BTreeMap::from([("plan".to_owned(), "review".to_owned())]),
            then: String::new(),
        },
    );
    let stages = [a, stage("b", &[]), stage("describe_pr", &[])];
    let resolve = |idx: usize, result: &str, then: &str| {
        let next = stages.get(idx + 1).map(|s| s.id.as_str());
        resolve_route(&stages[idx], next, result, then)
    };
    // then wins on passed.
    assert_eq!(resolve(1, RESULT_PASSED, "describe_pr"), go("describe_pr"));
    // then is ignored on needs_work and failed, which route as they would without it.
    assert_eq!(resolve(1, RESULT_NEEDS_WORK, "describe_pr"), Route::Ask);
    assert_eq!(resolve(1, RESULT_FAILED, "describe_pr"), Route::Ask);
    // No then keeps the next stage.
    assert_eq!(resolve(1, RESULT_PASSED, ""), go("describe_pr"));
    assert_eq!(resolve(0, RESULT_PASSED, ""), go("b"));
    // A then is ignored on a result the stage routes itself, and that route keeps its call.
    assert_eq!(
        resolve(0, RESULT_NEEDS_WORK, "describe_pr"),
        Route::Call {
            to: "b".into(),
            give: BTreeMap::from([("plan".to_owned(), "review".to_owned())]),
            then: None,
        }
    );
    // A then is a plain route: it leaves no call behind for the next stage.
    assert!(!matches!(
        resolve(1, RESULT_PASSED, "describe_pr"),
        Route::Call { .. }
    ));
}

#[test]
fn a_then_can_end_the_workflow_or_stop_it() {
    let stages = [stage("a", &[]), stage("b", &[])];
    let resolve = |then: &str| resolve_route(&stages[0], Some("b"), RESULT_PASSED, then);
    assert_eq!(resolve("done"), Route::Done);
    assert_eq!(resolve("stop"), Route::Stop);
}

#[test]
fn the_loop_count_walks_back_while_each_entry_routed_backward() {
    for (earlier, want) in [
        (vec![], 0),
        (vec![true], 1),
        (vec![true, true], 2),
        (vec![true, false, true], 1), // a forward route ends the count
        (vec![true, true, false], 0),
    ] {
        assert_eq!(loop_count(&earlier), want, "{earlier:?}");
    }
    for (limit, earlier, want) in [
        (0, 50, false), // no limit
        (-1, 50, false),
        (2, 0, false),
        (2, 1, false),
        (2, 2, true), // the third backward route in a row passes a limit of 2
    ] {
        assert_eq!(over_loop_limit(limit, earlier), want, "{limit}, {earlier}");
    }
}

#[test]
fn a_retry_waits_longer_each_attempt_up_to_fifteen_minutes() {
    let minute = Duration::from_secs(60);
    let ms = Duration::from_millis;
    for (attempt, max, backoff, want) in [
        (1, 0, minute, None),
        (1, 2, minute, Some(minute)),
        (2, 2, minute, Some(2 * minute)),
        (3, 2, minute, None),
        (3, 5, ms(20), Some(ms(80))),
        (6, 9, 2 * minute, Some(15 * minute)), // 64 minutes, capped
        (2, 3, Duration::ZERO, Some(Duration::ZERO)),
        // An attempt count that would overflow the doubling is capped, not a panic.
        (60, 100, minute, Some(15 * minute)),
    ] {
        assert_eq!(
            retry_decision(attempt, max, backoff),
            want,
            "attempt {attempt} of {max}"
        );
    }
}

#[test]
fn labels_are_met_when_the_card_has_every_one_wanted() {
    let labels = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    assert!(labels_met(&labels(&["a", "b"]), &labels(&["b"])));
    assert!(labels_met(&[], &[]));
    assert!(!labels_met(&labels(&["a"]), &labels(&["a", "b"])));
}

#[test]
fn a_plan_repeat_measures_each_round_by_its_plan() {
    let spec = Repeat {
        until: "plan_complete".into(),
        plan: "plan".into(),
        max_rounds: 4,
        stall_after: 2,
        fresh_session: false,
    };
    // (name, checked at the start, rounds as (result, checked, total, want))
    use RepeatStep::{MaxRounds, Next, NoCheckboxes, Pass, Route, Stalled};
    type Round<'a> = (&'a str, u32, u32, RepeatStep);
    let cases: Vec<(&str, u32, Vec<Round>)> = vec![
        (
            "progress then complete",
            0,
            vec![
                ("passed", 1, 3, Next),
                ("needs_work", 2, 3, Next),
                ("passed", 3, 3, Pass),
            ],
        ),
        (
            "no result is measured by the plan",
            0,
            vec![("", 1, 2, Next), ("", 2, 2, Pass)],
        ),
        ("failed routes at once", 0, vec![("failed", 3, 3, Route)]),
        (
            "needs input routes at once",
            0,
            vec![("needs_input", 0, 3, Route)],
        ),
        (
            "custom result routes at once",
            0,
            vec![("blocked", 1, 3, Route)],
        ),
        ("no checkboxes", 0, vec![("passed", 0, 0, NoCheckboxes)]),
        (
            "stalled after two unchanged rounds",
            0,
            vec![
                ("passed", 1, 3, Next),
                ("passed", 1, 3, Next),
                ("passed", 1, 3, Stalled),
            ],
        ),
        (
            "unchanged from the start",
            1,
            vec![("passed", 1, 3, Next), ("passed", 1, 3, Stalled)],
        ),
        (
            "max rounds",
            0,
            vec![
                ("passed", 1, 9, Next),
                ("passed", 2, 9, Next),
                ("passed", 3, 9, Next),
                ("passed", 4, 9, MaxRounds),
            ],
        ),
    ];
    for (name, start, rounds) in cases {
        let mut state = RepeatState {
            last_checked: start,
            unchanged: 0,
        };
        for (i, (result, checked, total, want)) in rounds.into_iter().enumerate() {
            let (got, next) = repeat_decision(state, i as u32 + 1, result, checked, total, &spec);
            state = next;
            assert_eq!(got, want, "{name}: round {}", i + 1);
        }
    }

    // Zero limits take the defaults.
    let plain = Repeat {
        plan: "plan".into(),
        ..Repeat::default()
    };
    let mut state = RepeatState::default();
    for round in 1..=DEFAULT_STALL_AFTER as u32 {
        let (got, next) = repeat_decision(state, round, "passed", 0, 1, &plain);
        state = next;
        let want = if i64::from(round) == DEFAULT_STALL_AFTER {
            Stalled
        } else {
            Next
        };
        assert_eq!(got, want, "default stall_after: round {round}");
    }
}

#[test]
fn the_default_max_rounds_is_twenty() {
    // Each round checks one more item than the last, so nothing stalls. The default is
    // written out, so a change to it fails here.
    assert_eq!(DEFAULT_MAX_ROUNDS, 20);
    let spec = Repeat {
        plan: "plan".into(),
        ..Repeat::default()
    };
    for (round, want) in [(19, RepeatStep::Next), (20, RepeatStep::MaxRounds)] {
        let (got, _) = repeat_decision(RepeatState::default(), round, RESULT_PASSED, 1, 100, &spec);
        assert_eq!(got, want, "round {round}");
    }
}

#[test]
fn an_agent_done_repeat_ends_on_passed_and_runs_rounds_on_needs_work() {
    let three = Repeat {
        until: "agent_done".into(),
        max_rounds: 3,
        ..Repeat::default()
    };
    let default = Repeat {
        until: "agent_done".into(),
        ..Repeat::default()
    };
    let max = DEFAULT_MAX_ROUNDS as u32;
    for (round, result, spec, want) in [
        (1, "passed", &three, RepeatStep::Pass),
        (3, "passed", &three, RepeatStep::Pass),
        (1, "needs_work", &three, RepeatStep::Next),
        (1, "", &three, RepeatStep::Next),
        (3, "needs_work", &three, RepeatStep::MaxRounds),
        (3, "", &three, RepeatStep::MaxRounds),
        (1, "failed", &three, RepeatStep::Route),
        (1, "needs_input", &three, RepeatStep::Route),
        (1, "blocked", &three, RepeatStep::Route),
        (max - 1, "needs_work", &default, RepeatStep::Next),
        (max, "needs_work", &default, RepeatStep::MaxRounds),
    ] {
        assert_eq!(
            agent_done_decision(round, result, spec),
            want,
            "round {round}, result {result:?}"
        );
    }
}
