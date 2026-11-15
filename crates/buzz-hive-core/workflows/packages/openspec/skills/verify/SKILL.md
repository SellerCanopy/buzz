---
name: verify
description: The Verify stage of the openspec workflow
disable-model-invocation: true
arguments: [branch, issue_number]
---
Verify that the implementation on branch $branch matches the OpenSpec change for issue $issue_number. You report. You do not change any file, implement a task or archive the change.

First read ${CLAUDE_PLUGIN_ROOT}/_lib/formats.md. It holds every format and rule used here.

1. Read the proposal with the Kiln tool read_artifact to get the change name and skip_specs. Then read the change record in openspec/changes/<change name>/: tasks.md, every delta spec under specs/, and design.md if it exists. See what the branch changed with git diff, git log and git status.
2. Completeness. Count the ticked and unticked tasks in tasks.md. Each unticked task is a CRITICAL issue ("Complete task: ..."). For each ADDED or MODIFIED requirement, search the code for evidence it is implemented. If none, CRITICAL: "Requirement not found: <name>".
3. Removals and renames. For each REMOVED requirement, read its baseline in the main spec at openspec/specs/<capability-path>/spec.md, or in git history if the spec no longer has it, and search the code for the behavior. Finding it still present is CRITICAL: "Removed requirement still implemented: <name>". For a RENAMED entry, check the behavior is kept under the new name. An entry whose baseline you cannot find is not verified, never passing.
4. Correctness. For each ADDED or MODIFIED requirement, note the file and lines that implement it and whether they match its intent (WARNING when they may diverge). For each of its scenarios, check the code handles it and a test covers it (WARNING when not).
5. Coherence. If design.md exists, check the code follows its decisions (WARNING when it does not). Check new code follows the project's patterns (SUGGESTION).
6. Skip checks that do not apply: spec checks when skip_specs is true, design checks when there is no design. Do not invent optional artifacts to get a clean report.
7. When unsure, prefer SUGGESTION over WARNING and WARNING over CRITICAL. Give every issue a specific recommendation with a file and line.
8. Save the report with the Kiln tool save_artifact under the name verify-report: a table with the rows Completeness, Correctness and Coherence, then the issues grouped as CRITICAL (must fix before archive), WARNING and SUGGESTION, then a final assessment. List every check you could not run and why. Never score a skipped check as passing.

Report passed when there is no CRITICAL issue. Report needs_work when there is at least one, and name the issues in your final message.
