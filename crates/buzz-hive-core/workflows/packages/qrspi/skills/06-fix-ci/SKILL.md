---
name: 06-fix-ci
description: Only use when the user explicitly invokes this skill by name.
---

# Fix CI failures

CI failed on the pull request for this issue. The argument is the file that lists the failing checks.

1. Read the file. Each row names a failing check, its conclusion and a link.
2. Find the cause of each failing check. Open the link if the local repo does not show it.
3. Fix the cause in the code. Do not weaken or skip a check to make it pass.
4. Run the repo's own checks locally and confirm they pass.
5. Report passed when the fixes are in place. Report failed if you cannot find the cause.

Commit nothing outside the files you changed.
