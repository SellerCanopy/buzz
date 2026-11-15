# Publishing HTML pages

**Never publish a Markdown (`.md`) file with the `Artifact` tool.** Task documents (research, design discussion, PRD, TDD, structure outline, plan) stay local files. Publishing them would fill the user's artifact list. Give only the local path for them.

Skills that write an HTML mockup, an HTML diagram, or a PR walkthrough also publish it as a private claude.ai page with the `Artifact` tool. The page renders the file, gives the user a link, and lets teammates leave comments that `/review-artifact-comments` reads and answers.

## Is publishing on?

Publishing is on unless the repository turns it off. Read `.claude/workspace.local.json`, then `.claude/workspace.json`. The first one that sets `"publishArtifacts"` decides. If neither sets it, publishing is on. Check once per session.

If publishing is off, skip everything below and give only the local path.

## Where the links live

Each task folder keeps `artifacts.json`, a map from a file's name inside the task folder to its page URL:

```json
{
  "ENG-1234-03-design.md": "https://claude.ai/code/artifact/...",
  "mockup-share-dialog.html": "https://claude.ai/code/artifact/..."
}
```

Read it before publishing. Create it with the first entry. A walkthrough with no task folder records its URL in `.claude/tasks/pr-{number}/artifacts.json`.

## Publish or update

- **First publish (no entry):** call `Artifact` with the file's path, a one-word `icon` (`doc` for documents, `design` for mockups, `diagram` for diagrams, `code` for a PR walkthrough), and a one-sentence `description`. Record the returned URL in `artifacts.json`.
- **HTML files:** follow the Artifact tool's own page rules (title, light and dark colors, allowed script sources) when writing the file, before publishing.
- **Update (entry exists):** if this session has not published or read that page yet, read it first with `Artifact` `action: "read"` and its URL. Then publish the local file with `url` set. The local file is the source of truth: if the read shows the page changed elsewhere, tell the user what differs before overwriting it.
- **When to republish:** after creating the file, before asking the user to review it, and before the skill's final message, whenever the file changed since the last publish. Do not republish after every small edit mid-conversation.

## If publishing fails

A failed or refused publish never blocks the skill. Finish the work, give the local path, and say in one line that publishing failed and why (for example, not logged in to claude.ai).

## Reporting

Whenever the skill names a published file, give both:

```
Saved at `.claude/tasks/ENG-1234-description/ENG-1234-03-design.md`
Page: https://claude.ai/code/artifact/...
```

After the first publish of a mockup or diagram that takes feedback, add one line: pages are private until shared from the page, teammates can comment there, and `/review-artifact-comments` works through their comments.
