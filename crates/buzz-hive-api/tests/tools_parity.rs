//! Every served route is a tool or has a reason it is not one, and a tool asks for the level
//! its route asks for.
//!
//! `buzz-hive-mcp` lists `TOOLS`, so a route that is neither a tool nor listed in `NOT_TOOLS`
//! is a capability an agent silently lacks. The check is a function over the three tables so
//! the last test can prove it fails on a gap.

use buzz_hive_api::routes::ROUTES;
use buzz_hive_api::tools::{Tool, NOT_TOOLS, TOOLS};
use buzz_hive_core::access::Level;

fn problems(routes: &[(&str, Level)], tools: &[Tool], not_tools: &[(&str, &str)]) -> Vec<String> {
    let mut out = Vec::new();
    for (method, level) in routes {
        let tool = tools.iter().find(|t| t.route == *method);
        let reason = not_tools.iter().find(|(m, _)| m == method);
        match (tool, reason) {
            (None, None) => out.push(format!("{method} is neither a tool nor in NOT_TOOLS")),
            (Some(_), Some(_)) => out.push(format!("{method} is both a tool and in NOT_TOOLS")),
            (Some(tool), None) if tool.needs != *level => out.push(format!(
                "{} needs {:?} but its route {method} needs {level:?}",
                tool.name, tool.needs
            )),
            (None, Some((_, why))) if why.trim().is_empty() => {
                out.push(format!("{method} is in NOT_TOOLS with no reason"))
            }
            _ => {}
        }
    }
    for tool in tools {
        if !routes.iter().any(|(m, _)| *m == tool.route) {
            out.push(format!(
                "{} calls {}, which is not served",
                tool.name, tool.route
            ));
        }
    }
    for (method, _) in not_tools {
        if !routes.iter().any(|(m, _)| m == method) {
            out.push(format!("NOT_TOOLS lists {method}, which is not served"));
        }
    }
    out
}

#[test]
fn every_route_is_a_tool_or_has_a_reason() {
    let found = problems(ROUTES, TOOLS, NOT_TOOLS);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn the_check_fails_on_a_route_with_no_tool_and_no_reason() {
    let mut routes = ROUTES.to_vec();
    routes.push(("Brand", Level::Run));
    let found = problems(&routes, TOOLS, NOT_TOOLS);
    assert_eq!(found, ["Brand is neither a tool nor in NOT_TOOLS"]);
}

#[test]
fn the_check_fails_on_a_reasonless_entry_a_double_entry_and_a_level_mismatch() {
    let routes = [("A", Level::Run), ("B", Level::Read), ("C", Level::Read)];
    let tools = [
        Tool {
            name: "b",
            route: "B",
            needs: Level::Read,
            description: "",
            schema: || serde_json::json!({}),
        },
        Tool {
            name: "c",
            route: "C",
            needs: Level::Run,
            description: "",
            schema: || serde_json::json!({}),
        },
    ];
    let not_tools = [("A", " "), ("B", "also listed")];
    let found = problems(&routes, &tools, &not_tools);
    assert_eq!(found.len(), 3, "{found:#?}");
    assert!(found
        .iter()
        .any(|f| f.contains("A is in NOT_TOOLS with no reason")));
    assert!(found.iter().any(|f| f.contains("B is both")));
    assert!(found.iter().any(|f| f.contains("c needs Run")));
}
