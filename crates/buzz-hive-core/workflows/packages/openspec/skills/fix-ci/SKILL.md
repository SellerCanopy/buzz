---
name: fix-ci
description: The Fix CI stage of the openspec workflow
disable-model-invocation: true
arguments: [artifacts_ci_failure_path]
---
CI failed on the pull request for this issue. The file $artifacts_ci_failure_path lists the failing checks.

1. Read the file. Each row names a failing check, its conclusion and a link.
2. Find the cause of each failing check. Open the link if the local repo does not show it.
3. Fix the cause in the code. Do not weaken or skip a check to make it pass. Do not edit the archived change record under openspec/changes/archive/.
4. Run the repository's own checks locally and confirm they pass.
5. Commit your fixes to the branch. Commit nothing outside the files you changed.

Report passed when the fixes are in place. Report failed if you cannot find the cause.
