//! What earlier sessions on a stage left for the next one. Ported from the Go engine's
//! `rules/brief.go`.
//!
//! Beehive has no `report_blocker` tool, so a brief has three kinds of entry: decisions, finished
//! work, and notes.

use std::collections::BTreeSet;

/// How many characters a brief may hold before parts are cut.
pub const BRIEF_CAP: usize = 8000;

const HEAD: &str = "Earlier sessions worked on this stage. This is what they left. Call read_history for every entry.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// Work a session finished.
    Progress,
    /// A choice and why.
    Decision {
        why: String,
        /// Options turned down, each with its reason.
        rejected: Vec<(String, String)>,
        /// The seq of an earlier decision this one replaces, 0 for none.
        supersedes: i64,
    },
    /// A person's note.
    Note,
}

/// One entry of a card's activity, as the brief reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub seq: i64,
    pub kind: EntryKind,
    pub summary: String,
    pub detail: String,
}

/// Lays out what earlier sessions left, in three parts: decisions, finished work, and notes.
/// Over `limit` characters it drops details, then replaced decisions, then the oldest finished
/// work, which one line counts. Live decisions and notes are never cut. It returns an empty
/// string when nothing carries over.
pub fn brief(entries: &[Entry], limit: usize) -> String {
    let parts = Parts::split(entries);
    if parts.is_empty() {
        return String::new();
    }
    let mut opts = Opts {
        details: true,
        replaced: true,
        skip: 0,
    };
    let over = |text: &str| text.chars().count() > limit;
    let mut text = parts.render(opts);
    if over(&text) {
        opts.details = false;
        text = parts.render(opts);
    }
    if over(&text) {
        opts.replaced = false;
        text = parts.render(opts);
    }
    while over(&text) && opts.skip < parts.progress.len() {
        opts.skip += 1;
        text = parts.render(opts);
    }
    text
}

struct Parts<'a> {
    /// In seq order, live and replaced.
    decisions: Vec<&'a Entry>,
    /// Decision seqs a later decision supersedes.
    replaced: BTreeSet<i64>,
    progress: Vec<&'a Entry>,
    notes: Vec<&'a Entry>,
}

#[derive(Clone, Copy)]
struct Opts {
    details: bool,
    replaced: bool,
    /// The oldest progress lines left out.
    skip: usize,
}

impl<'a> Parts<'a> {
    fn split(entries: &'a [Entry]) -> Self {
        let mut parts = Parts {
            decisions: Vec::new(),
            replaced: BTreeSet::new(),
            progress: Vec::new(),
            notes: Vec::new(),
        };
        for entry in entries {
            match &entry.kind {
                EntryKind::Decision { supersedes, .. } => {
                    if *supersedes != 0 {
                        parts.replaced.insert(*supersedes);
                    }
                    parts.decisions.push(entry);
                }
                EntryKind::Progress => parts.progress.push(entry),
                EntryKind::Note => parts.notes.push(entry),
            }
        }
        parts
    }

    fn is_empty(&self) -> bool {
        self.decisions.is_empty() && self.progress.is_empty() && self.notes.is_empty()
    }

    fn render(&self, opts: Opts) -> String {
        let mut out = String::from(HEAD);
        let section = |out: &mut String, title: &str, lines: Vec<String>| {
            if !lines.is_empty() {
                out.push_str(&format!("\n\n{title}\n{}", lines.join("\n")));
            }
        };
        let line = |mut text: String, entry: &Entry| {
            if opts.details && !entry.detail.is_empty() {
                text.push_str("\n  ");
                text.push_str(&entry.detail.replace('\n', "\n  "));
            }
            text
        };

        let mut lines = Vec::new();
        for entry in &self.decisions {
            let EntryKind::Decision { why, rejected, .. } = &entry.kind else {
                continue;
            };
            if self.replaced.contains(&entry.seq) {
                if opts.replaced {
                    lines.push(line(format!("- Replaced: {}", entry.summary), entry));
                }
                continue;
            }
            let mut text = format!("- {} Why: {}", entry.summary, sentence(why));
            if !rejected.is_empty() {
                let turned: Vec<String> = rejected
                    .iter()
                    .map(|(option, reason)| format!("{option} ({reason})"))
                    .collect();
                text.push_str(&format!(" Turned down: {}", sentence(&turned.join("; "))));
            }
            lines.push(line(text, entry));
        }
        section(&mut out, "Decisions so far:", lines);

        let mut lines = Vec::new();
        match opts.skip {
            0 => {}
            1 => lines.push("- 1 earlier step left out.".to_owned()),
            n => lines.push(format!("- {n} earlier steps left out.")),
        }
        for entry in &self.progress[opts.skip..] {
            lines.push(line(format!("- {}", entry.summary), entry));
        }
        section(&mut out, "Work finished so far:", lines);

        let lines: Vec<String> = self
            .notes
            .iter()
            .map(|entry| format!("- Your note: {}", entry.summary))
            .collect();
        section(&mut out, "Answers and notes:", lines);
        out
    }
}

/// Ends `s` with a period unless it already ends a sentence.
fn sentence(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() || s.ends_with(['.', '!', '?']) {
        s.to_owned()
    } else {
        format!("{s}.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(seq: i64, summary: &str, detail: &str) -> Entry {
        Entry {
            seq,
            kind: EntryKind::Progress,
            summary: summary.into(),
            detail: detail.into(),
        }
    }

    fn decision(seq: i64, summary: &str, why: &str, supersedes: i64) -> Entry {
        Entry {
            seq,
            kind: EntryKind::Decision {
                why: why.into(),
                rejected: vec![("A".into(), "slow".into())],
                supersedes,
            },
            summary: summary.into(),
            detail: String::new(),
        }
    }

    #[test]
    fn nothing_carries_over_when_there_are_no_entries() {
        assert_eq!(brief(&[], BRIEF_CAP), "");
    }

    #[test]
    fn the_parts_read_in_order_with_details() {
        let entries = [
            decision(1, "Kept the limiter in the engine.", "One owner", 0),
            progress(2, "Added the limiter.", "It has tests.\nTwo of them."),
            Entry {
                seq: 3,
                kind: EntryKind::Note,
                summary: "Use tabs".into(),
                detail: String::new(),
            },
        ];
        let got = brief(&entries, BRIEF_CAP);
        let want = format!(
            "{HEAD}\n\nDecisions so far:\n- Kept the limiter in the engine. Why: One owner. Turned down: A (slow).\n\nWork finished so far:\n- Added the limiter.\n  It has tests.\n  Two of them.\n\nAnswers and notes:\n- Your note: Use tabs"
        );
        assert_eq!(got, want);
    }

    #[test]
    fn a_replaced_decision_shows_as_replaced_and_is_cut_first_after_details() {
        let entries = [
            decision(1, "Use A.", "fast", 0),
            decision(2, "Use B.", "safe", 1),
            progress(3, "Did it.", &"d".repeat(200)),
        ];
        let full = brief(&entries, BRIEF_CAP);
        assert!(full.contains("- Replaced: Use A."), "{full}");
        assert!(full.contains(&"d".repeat(200)));
        let no_details = brief(&entries, full.chars().count() - 1);
        assert!(!no_details.contains(&"d".repeat(200)), "{no_details}");
        assert!(no_details.contains("- Replaced: Use A."), "{no_details}");
        let tight = brief(&entries, no_details.chars().count() - 1);
        assert!(!tight.contains("Replaced"), "{tight}");
        assert!(tight.contains("Use B."), "{tight}");
    }

    #[test]
    fn the_oldest_work_is_left_out_last_and_counted() {
        let entries: Vec<Entry> = (1..=30)
            .map(|i| progress(i, &format!("Step {i} finished."), ""))
            .collect();
        let got = brief(&entries, 400);
        assert!(got.chars().count() <= 400, "{}", got.chars().count());
        assert!(got.contains("earlier steps left out."), "{got}");
        assert!(got.contains("Step 30 finished."), "{got}");
        assert!(!got.contains("Step 1 finished."), "{got}");
    }

    #[test]
    fn live_decisions_and_notes_are_never_cut() {
        let entries = [
            decision(1, "Use B.", "safe", 0),
            Entry {
                seq: 2,
                kind: EntryKind::Note,
                summary: "n".into(),
                detail: String::new(),
            },
        ];
        let got = brief(&entries, 10);
        assert!(
            got.contains("Use B.") && got.contains("Your note: n"),
            "{got}"
        );
    }
}
