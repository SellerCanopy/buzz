---
name: archive
description: The Archive stage of the openspec workflow
disable-model-invocation: true
arguments: [branch, issue_number]
---
Archive the verified OpenSpec change for issue $issue_number in your working folder, which is a git worktree on the branch $branch. Archiving merges the change's delta specs into the main specs and moves the change record to the archive, so the pull request carries both.

First read ${CLAUDE_PLUGIN_ROOT}/_lib/formats.md. It holds every format and rule used here, including the merge order.

1. Read the proposal with the Kiln tool read_artifact to get the change name and skip_specs. The change record is openspec/changes/<change name>/.
2. Check tasks.md: every tracked task is ticked. If not, report failed and name the open tasks. Do not archive an unfinished change.
3. If skip_specs is not true, merge each delta spec file under the change's specs/ into the main spec openspec/specs/<capability-path>/spec.md, following the merge rules in formats.md exactly: RENAMED, then REMOVED, then MODIFIED, then ADDED. Do the merge yourself, in this session. Stop and report failed, changing nothing further, if a rule in formats.md says to stop. Copy a new capability's Purpose from its delta.
4. Re-read each main spec you wrote. Check it has a Purpose and at least one requirement, that every requirement has SHALL or MUST and a four hash scenario, and that no requirement appears twice. Fix what fails.
5. Move openspec/changes/<change name>/ to openspec/changes/archive/YYYY-MM-DD-<change name>/ with today's date. Use git mv so the history follows. If the name already starts with a date, do not add a second date. If the target exists, report failed.
6. Commit the merged specs and the move to the branch in one commit. Do not push and do not open a pull request; later stages do that.
7. Save the pull request description with the Kiln tool save_artifact under the name pr_body. It is the proposal artifact without its header (drop the lines from the first --- to the second ---, so the change name and skip_specs never reach the pull request), then a Specs updated section that lists each capability you merged, or says no specs changed when skip_specs is true.

Report passed when the merge, the checks, the move and the description are done. In your final message, list each capability updated and what changed.
