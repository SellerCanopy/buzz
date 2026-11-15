---
name: 06-describe-pr
description: Only use when the user explicitly invokes this skill by name.
---

# Describe the pull request

The code is finished and reviewed. Write the pull request description for this issue and save it as a file. The next stage opens the pull request with that file as its body. The argument is the issue's task folder.

1. Find the template. Look in the repo for `.github/PULL_REQUEST_TEMPLATE.md`, `.github/pull_request_template.md`, `PULL_REQUEST_TEMPLATE.md`, `pull_request_template.md`, `docs/PULL_REQUEST_TEMPLATE.md`, then a single file in `.github/PULL_REQUEST_TEMPLATE/`. If that folder holds several files, pick the one whose name best fits the change. If the repo has a template, follow it: keep every heading, its order and its wording, tick only the boxes the diff supports, and write `N/A` or `None.` in a section with nothing to say. Add no sections of your own. If the repo has none, read `${CLAUDE_PLUGIN_ROOT}/skills/06-describe-pr/references/pr_description_template.md` and use that.
2. Read the facts. The pull request is about the code, so start from the commits and the diff against the base branch, with `git log` and `git diff --no-ext-diff --no-textconv --ignore-submodules=all`. Use `git symbolic-ref refs/remotes/origin/HEAD` to find the base branch. Open a document in the task folder only when the diff leaves the reason for the change unclear, and then open only the one that answers it, usually the requirements. The task documents are working papers for you. They are not part of the pull request.
3. Write the description. With the built-in template: keep Why the change to one sentence, keep Special things to note to one to three bullets, and make Change outline a short structural view, not prose and not a file-by-file list. Start with a link to the issue when there is one.
4. Keep it short. The body must stay under 6,000 characters, and GitHub rejects anything over 65,536. Never paste, quote, summarize at length, or link a task document, and never name a path under `.claude/tasks/`. Leave out the plan, the review findings, a checklist of what you ran, and a changelog. Describe the change as the code shows it.
5. Save it to `GH-<number>-08-pr-body.md` in the task folder, next to the plan. Do not stage or commit it, and do not run `gh`, push, or open the pull request.

Report passed when the file is saved.
