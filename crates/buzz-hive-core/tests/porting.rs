//! The port ledger is closed: every `PORTING.md` row starts with one of the three statuses
//! (`ported`, `changed`, `dropped`), no API procedure is still pending, and the Spikes section
//! holds one closed row for each of the 16 TDD spikes. A row that says `pending`, `deferred`,
//! `kept` or anything else is not closed.
//!
//! The parser reads `PORTING.md` tables by their header: a table with a `decision` column is a
//! ledger table, and each of its rows is checked. Prose and other tables are not read.

const LEDGER: &str = include_str!("../PORTING.md");
const PROCEDURES: &str = include_str!("../../buzz-hive-api/tests/procedures.rs");

/// The number of TDD spikes the Spikes section must cover.
const SPIKES: usize = 16;

/// The only decisions that close a row.
const STATUSES: [&str; 3] = ["ported", "changed", "dropped"];

/// One ledger row: its first cell and its decision cell.
#[derive(Debug, PartialEq)]
struct Row {
    item: String,
    decision: String,
}

/// The cells of a table line, without the outer pipes, trimmed.
fn cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

/// Every row of every table that has a `decision` column, in document order. A row with a
/// different number of cells than its header is an error, since the decision would be misread.
fn ledger_rows(markdown: &str) -> Result<Vec<Row>, String> {
    let mut rows = Vec::new();
    let mut header: Option<(usize, usize)> = None; // (decision column, cell count)
    let mut expect_separator = false;
    for line in markdown.lines() {
        if !line.trim_start().starts_with('|') {
            header = None;
            expect_separator = false;
            continue;
        }
        let row = cells(line);
        if header.is_none() {
            header = row
                .iter()
                .position(|c| c.eq_ignore_ascii_case("decision"))
                .map(|col| (col, row.len()));
            expect_separator = header.is_some();
            continue;
        }
        if expect_separator {
            expect_separator = false;
            continue;
        }
        let (col, count) = header.expect("checked above");
        if row.len() != count {
            return Err(format!(
                "a ledger row has {} cells, its header has {count}: {line}",
                row.len()
            ));
        }
        rows.push(Row {
            item: row[0].clone(),
            decision: row[col].clone(),
        });
    }
    Ok(rows)
}

/// The text of the `## <title>` section, up to the next `## ` heading.
fn section<'a>(markdown: &'a str, title: &str) -> Option<&'a str> {
    let heading = format!("## {title}");
    let mut start = None;
    let mut offset = 0;
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_end();
        match start {
            None if trimmed == heading => start = Some(offset + line.len()),
            Some(from) if trimmed.starts_with("## ") => return Some(&markdown[from..offset]),
            _ => {}
        }
        offset += line.len();
    }
    start.map(|from| &markdown[from..])
}

/// True when the decision cell starts with a status. Its leading word is compared, so a
/// note after the status (`changed (Phase 18: ...)`) does not matter, and neither does a
/// status named later in the note.
fn is_closed(decision: &str) -> bool {
    let word: String = decision
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    STATUSES
        .iter()
        .any(|status| word.eq_ignore_ascii_case(status))
}

/// The items whose decision does not start with a status.
fn pending_items(markdown: &str) -> Result<Vec<String>, String> {
    Ok(ledger_rows(markdown)?
        .into_iter()
        .filter(|r| !is_closed(&r.decision))
        .map(|r| r.item)
        .collect())
}

/// The spike numbers named by the Spikes section, and the spikes that are still pending.
/// Returns an error for a missing section, a missing or repeated number, or a pending row.
fn check_spikes(markdown: &str) -> Result<(), String> {
    let body = section(markdown, "Spikes").ok_or("PORTING.md has no `## Spikes` section")?;
    let rows = ledger_rows(body)?;
    let mut seen = [false; SPIKES + 1];
    for row in &rows {
        let number: usize = row
            .item
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| format!("a spike row does not start with its number: {}", row.item))?;
        if !(1..=SPIKES).contains(&number) {
            return Err(format!("spike {number} is outside 1 to {SPIKES}"));
        }
        if seen[number] {
            return Err(format!("spike {number} has more than one row"));
        }
        seen[number] = true;
        if !is_closed(&row.decision) {
            return Err(format!(
                "spike {number} has no status (ported, changed or dropped): {}",
                row.item
            ));
        }
    }
    let missing: Vec<usize> = (1..=SPIKES).filter(|n| !seen[*n]).collect();
    if !missing.is_empty() {
        return Err(format!(
            "the Spikes section has no row for spikes {missing:?}"
        ));
    }
    Ok(())
}

/// The method names in the `PENDING` list of `procedures.rs`, without comments.
fn pending_procedures(source: &str) -> Result<Vec<String>, String> {
    let start = source
        .find("const PENDING")
        .ok_or("procedures.rs has no `const PENDING`")?;
    let rest = &source[start..];
    let open = rest.find("= &[").ok_or("PENDING has no list")? + 4;
    let close = rest[open..]
        .find("];")
        .ok_or("PENDING list is not closed")?
        + open;
    let mut names = Vec::new();
    for line in rest[open..close].lines() {
        let code = line.split("//").next().unwrap_or("");
        let mut parts = code.split('"');
        while let (Some(_), Some(name)) = (parts.next(), parts.next()) {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

#[test]
fn no_ledger_row_is_pending() {
    let pending = pending_items(LEDGER).expect("PORTING.md tables parse");
    assert!(
        pending.is_empty(),
        "{} ledger rows have no status (ported, changed or dropped): {pending:?}",
        pending.len()
    );
}

#[test]
fn the_ledger_tables_are_found() {
    let rows = ledger_rows(LEDGER).expect("PORTING.md tables parse");
    // The Features, Go test directories, e2e specs and Spikes tables hold well over this.
    assert!(
        rows.len() > 100,
        "only {} ledger rows were read",
        rows.len()
    );
}

#[test]
fn every_spike_has_a_row_and_none_is_pending() {
    if let Err(e) = check_spikes(LEDGER) {
        panic!("{e}");
    }
}

#[test]
fn no_procedure_is_pending() {
    let pending = pending_procedures(PROCEDURES).expect("procedures.rs parses");
    assert!(
        pending.is_empty(),
        "procedures still pending in tests/procedures.rs: {pending:?}"
    );
}

// Parser tests, on fixture text.

const FIXTURE_HEADER: &str = "| item | decision | phase |\n|---|---|---|\n";

fn spikes_fixture(skip: Option<usize>, pending: Option<usize>) -> String {
    let mut text = format!("## Spikes\n\n{FIXTURE_HEADER}");
    for n in 1..=SPIKES {
        if Some(n) == skip {
            continue;
        }
        let decision = if Some(n) == pending {
            "pending (not run yet)"
        } else {
            "ported (a note)"
        };
        text.push_str(&format!("| {n} a spike | {decision} | 1 |\n"));
    }
    text.push_str("\n## Next section\n");
    text
}

#[test]
fn a_fixture_row_with_a_pending_decision_is_found() {
    let md = format!(
        "{FIXTURE_HEADER}| `a` | ported | 1 |\n| `b` | pending (later) | 2 |\n| `c` | pending | 3 |\n| `d` | dropped | 4 |\n"
    );
    assert_eq!(pending_items(&md).unwrap(), vec!["`b`", "`c`"]);
}

#[test]
fn a_decision_that_is_not_one_of_the_three_statuses_is_not_closed() {
    let md = format!(
        "{FIXTURE_HEADER}| `a` | deferred (later) | 1 |\n| `b` | kept, not dropped | 2 |\n| `c` | unassigned | 3 |\n| `d` | | 4 |\n| `e` | Changed (Phase 5) | 5 |\n| `f` | pending (this was changed later) | 6 |\n"
    );
    assert_eq!(
        pending_items(&md).unwrap(),
        vec!["`a`", "`b`", "`c`", "`d`", "`f`"]
    );
}

#[test]
fn a_fixture_with_only_decided_rows_has_none_pending() {
    let md = format!(
        "{FIXTURE_HEADER}| `a` | ported | 1 |\n| `b` | changed (pending items are listed in prose) | 2 |\n| `c` | dropped | 3 |\n"
    );
    assert!(pending_items(&md).unwrap().is_empty());
}

#[test]
fn the_decision_column_can_be_the_third_cell() {
    let md = "| item | test files | decision | phase |\n|---|---|---|---|\n| `x` | 3 | pending | 1 |\n| `y` | 4 | dropped | 1 |\n";
    assert_eq!(pending_items(md).unwrap(), vec!["`x`"]);
}

#[test]
fn a_table_without_a_decision_column_and_prose_are_not_read() {
    let md = "| name | status |\n|---|---|\n| a | pending |\n\nThe word pending in prose.\n";
    assert!(pending_items(md).unwrap().is_empty());
}

#[test]
fn a_row_with_the_wrong_cell_count_is_an_error() {
    let md = format!("{FIXTURE_HEADER}| `a` | ported | 1 | extra |\n");
    assert!(pending_items(&md).is_err());
}

#[test]
fn a_complete_spikes_section_passes() {
    assert_eq!(check_spikes(&spikes_fixture(None, None)), Ok(()));
}

#[test]
fn a_spikes_section_missing_a_row_fails() {
    let err = check_spikes(&spikes_fixture(Some(9), None)).unwrap_err();
    assert!(err.contains("[9]"), "{err}");
}

#[test]
fn a_pending_spike_fails() {
    let err = check_spikes(&spikes_fixture(None, Some(4))).unwrap_err();
    assert!(err.contains("spike 4 has no status"), "{err}");
}

#[test]
fn a_repeated_spike_fails() {
    let mut md = spikes_fixture(None, None);
    md = md.replace("| 2 a spike |", "| 1 a spike |");
    let err = check_spikes(&md).unwrap_err();
    assert!(err.contains("more than one row"), "{err}");
}

#[test]
fn a_ledger_with_no_spikes_section_fails() {
    assert!(check_spikes(&format!(
        "# Ledger\n\n{FIXTURE_HEADER}| `a` | ported | 1 |\n"
    ))
    .is_err());
}

#[test]
fn rows_after_the_spikes_section_are_not_counted_as_spikes() {
    let md = format!(
        "{}{FIXTURE_HEADER}| 17 stray | pending | 1 |\n",
        spikes_fixture(None, None)
    );
    assert_eq!(check_spikes(&md), Ok(()));
}

#[test]
fn pending_procedures_reads_names_and_skips_comments() {
    let src = "const PENDING: &[&str] = &[\n    // Phase 18\n    \"ListA\", // note\n    \"ListB\",\n];\n";
    assert_eq!(pending_procedures(src).unwrap(), vec!["ListA", "ListB"]);
    let empty = "const PENDING: &[&str] = &[\n    // Phase 18 \"Commented\"\n];\n";
    assert!(pending_procedures(empty).unwrap().is_empty());
    assert!(pending_procedures("const OTHER: &[&str] = &[];").is_err());
}
