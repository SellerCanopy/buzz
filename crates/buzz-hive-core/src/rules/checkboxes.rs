//! Task list items in a plan document. Ported from the Go engine's `rules/checkboxes.go`.
//!
//! An item is a line that starts with a bullet (`-`, `*`, `+`) or an ordered marker (`1.` or
//! `1)`), a space, then `[ ]` or `[x]`. Lines inside a fenced code block are not items.

/// One task list item found outside code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkbox {
    /// Index into the split lines.
    pub line: usize,
    pub checked: bool,
    pub text: String,
    /// Byte offset of the box character in its line.
    box_at: usize,
}

/// The item a line holds, as (box byte offset, checked, text), or `None`.
fn parse_item(line: &str) -> Option<(usize, bool, String)> {
    let b = line.as_bytes();
    let blank = |c: u8| c == b' ' || c == b'\t';
    let mut i = 0;
    while i < b.len() && blank(b[i]) {
        i += 1;
    }
    match b.get(i)? {
        b'-' | b'*' | b'+' => i += 1,
        b'0'..=b'9' => {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i - start > 9 || !matches!(b.get(i), Some(b'.' | b')')) {
                return None;
            }
            i += 1;
        }
        _ => return None,
    }
    let spaces = i;
    while i < b.len() && blank(b[i]) {
        i += 1;
    }
    if i == spaces || b.get(i) != Some(&b'[') {
        return None;
    }
    let box_at = i + 1;
    let mark = *b.get(box_at)?;
    if !matches!(mark, b' ' | b'x' | b'X') || b.get(box_at + 1) != Some(&b']') {
        return None;
    }
    let rest = &line[box_at + 2..];
    let text = if rest.bytes().all(blank) {
        ""
    } else {
        // The text follows at least one space or tab, and loses the trailing ones.
        let trimmed = rest.trim_start_matches([' ', '\t']);
        if trimmed.len() == rest.len() {
            return None;
        }
        trimmed.trim_end_matches([' ', '\t'])
    };
    Some((box_at, mark != b' ', text.to_owned()))
}

/// The fence character, its length and what follows it, when `line` opens or closes a
/// fenced code block.
fn fence_of(line: &str) -> Option<(u8, usize, &str)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let first = *t.as_bytes().first()?;
    if first != b'`' && first != b'~' {
        return None;
    }
    let n = t.bytes().take_while(|c| *c == first).count();
    if n < 3 {
        return None;
    }
    Some((first, n, &t[n..]))
}

/// Every task list item in `lines` that is not inside a fenced code block.
pub fn checkboxes(lines: &[&str]) -> Vec<Checkbox> {
    let mut out = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some((ch, n, rest)) = fence_of(line) {
            match fence {
                None => fence = Some((ch, n)),
                Some((open_ch, open_n))
                    if ch == open_ch && n >= open_n && rest.trim().is_empty() =>
                {
                    fence = None
                }
                _ => {}
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if let Some((box_at, checked, text)) = parse_item(line) {
            out.push(Checkbox {
                line: i,
                checked,
                text,
                box_at,
            });
        }
    }
    out
}

/// The checked and total task list items in `md`, skipping code.
pub fn count_checkboxes(md: &str) -> (u32, u32) {
    let lines: Vec<&str> = md.split('\n').collect();
    let boxes = checkboxes(&lines);
    let checked = boxes.iter().filter(|b| b.checked).count() as u32;
    (checked, boxes.len() as u32)
}

/// The most items an unknown-item error lists.
const MAX_LISTED_ITEMS: usize = 20;

fn list_items(boxes: &[Checkbox]) -> String {
    if boxes.is_empty() {
        return "none".to_owned();
    }
    let mut names = Vec::new();
    for (i, b) in boxes.iter().enumerate() {
        if i == MAX_LISTED_ITEMS {
            names.push(format!("and {} more", boxes.len() - i));
            break;
        }
        names.push(format!("{:?}", b.text.trim()));
    }
    names.join(", ")
}

/// Checks or unchecks the task list item whose text is `item` and returns the new text. An
/// item already in that state leaves `md` as it is. The text must match exactly, ignoring
/// surrounding space, or else ignoring case. With no match the error lists the items there
/// are.
pub fn set_checkbox(md: &str, item: &str, done: bool) -> Result<String, String> {
    let mut lines: Vec<String> = md.split('\n').map(str::to_owned).collect();
    let boxes = {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        checkboxes(&refs)
    };
    let want = item.trim();
    // An item that is not yet in the wanted state wins over one that is, so a repeated item
    // text is worked through in order.
    let pick = |matches: &dyn Fn(&str) -> bool| -> Option<&Checkbox> {
        let mut found = None;
        for b in &boxes {
            if !matches(b.text.trim()) {
                continue;
            }
            if b.checked != done {
                return Some(b);
            }
            found.get_or_insert(b);
        }
        found
    };
    let chosen = pick(&|t| t == want).or_else(|| pick(&|t| t.eq_ignore_ascii_case(want)));
    let Some(b) = chosen else {
        return Err(format!(
            "the plan has no checkbox item {item:?}; its items are: {}",
            list_items(&boxes)
        ));
    };
    if b.checked == done {
        return Ok(md.to_owned());
    }
    let line = &mut lines[b.line];
    line.replace_range(b.box_at..b.box_at + 1, if done { "x" } else { " " });
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_items_of_every_marker() {
        let md = "- [ ] a\n* [x] b\n+ [X] c\n1. [ ] d\n2) [x] e\nnot - [ ] an item\n-[ ] no space\n- [] empty\n";
        assert_eq!(count_checkboxes(md), (3, 5));
    }

    #[test]
    fn an_item_may_have_no_text_and_nested_items_count() {
        assert_eq!(
            count_checkboxes("- [ ]\n  - [x] inner\n\t- [ ] tabbed"),
            (1, 3)
        );
        assert_eq!(count_checkboxes("- [ ]x\n- [x]y"), (0, 0));
    }

    #[test]
    fn code_fences_hide_items() {
        let md = "- [ ] a\n```\n- [ ] hidden\n- [x] hidden\n```\n- [x] b\n~~~~\n- [ ] hidden\n~~~\n- [ ] still hidden\n~~~~\n- [ ] c";
        assert_eq!(count_checkboxes(md), (1, 3));
    }

    #[test]
    fn a_fence_opened_and_never_closed_hides_the_rest() {
        assert_eq!(count_checkboxes("- [x] a\n```\n- [ ] b"), (1, 1));
    }

    #[test]
    fn crlf_lines_count() {
        assert_eq!(count_checkboxes("- [x] a\r\n- [ ] b\r\n"), (1, 2));
    }

    #[test]
    fn set_checks_an_item_by_exact_text() {
        let md = "- [ ] write\n- [ ] test\n";
        assert_eq!(
            set_checkbox(md, "test", true).unwrap(),
            "- [ ] write\n- [x] test\n"
        );
        assert_eq!(
            set_checkbox("- [x] write", "write", false).unwrap(),
            "- [ ] write"
        );
    }

    #[test]
    fn set_falls_back_to_case_insensitive_and_trims() {
        assert_eq!(
            set_checkbox("- [ ] Write Tests", "  write tests ", true).unwrap(),
            "- [x] Write Tests"
        );
    }

    #[test]
    fn set_on_an_item_already_in_state_changes_nothing() {
        let md = "- [x] a\n";
        assert_eq!(set_checkbox(md, "a", true).unwrap(), md);
    }

    #[test]
    fn set_works_through_repeated_items_in_order() {
        let md = "- [x] a\n- [ ] a\n";
        assert_eq!(set_checkbox(md, "a", true).unwrap(), "- [x] a\n- [x] a\n");
    }

    #[test]
    fn set_does_not_touch_code() {
        let md = "```\n- [ ] a\n```\n- [ ] a";
        assert_eq!(
            set_checkbox(md, "a", true).unwrap(),
            "```\n- [ ] a\n```\n- [x] a"
        );
    }

    #[test]
    fn an_unknown_item_lists_the_items() {
        let err = set_checkbox("- [ ] a\n- [x] b", "c", true).unwrap_err();
        assert!(err.contains(r#"no checkbox item "c""#), "{err}");
        assert!(err.contains(r#""a", "b""#), "{err}");
        let none = set_checkbox("nothing", "c", true).unwrap_err();
        assert!(none.ends_with("none"), "{none}");
    }

    #[test]
    fn a_long_item_list_is_cut() {
        let md: String = (0..25).map(|i| format!("- [ ] item{i}\n")).collect();
        let err = set_checkbox(&md, "x", true).unwrap_err();
        assert!(err.ends_with("and 5 more"), "{err}");
    }
}
