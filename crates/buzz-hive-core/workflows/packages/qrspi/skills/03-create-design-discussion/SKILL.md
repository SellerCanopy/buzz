---
name: 03-create-design-discussion
description: Only use when the user explicitly invokes this skill by name.
model: claude-opus-5-5
effort: high
---

# Design Discussion Phase

You are now in the Design Discussion phase. Based on the research findings and the user's change request, work with them to make design decisions.

## Steps to follow after receiving the user's request

1. **Read the files in the task artifact directory and any mentioned files immediately and FULLY**:
   - `ls -La .claude/tasks/TASKSLUG` to find all related documents in the task directory. Do NOT use the Grep or Glob tools, or `ls -l` as the directory may be a symlink.\
   - Ticket files (e.g., `.claude/tasks/ENG-1234-description/ENG-1234-00-ticket.md`)
   - Research documents (e.g. `.claude/tasks/eng-1234-description/ENG-XXXX-02-research.md`)
   - **DO NOT read research questions documents** - research questions are inputs to the research phase only.
   - **IMPORTANT**: Use the Read tool WITHOUT limit/offset parameters to read entire files, never read files partially - if a file is mentioned, read it completely
   - do not spawn sub-tasks before reading these files yourself in the main context

2. **Check for related task content**:
   - if a path in `.claude/tasks/TASKNAME` is mentioned, use Bash(`ls -La .claude/tasks/TASKNAME`)
   - **IMPORTANT** DO NOT USE search or glob or grep, or `ls -l` (lowercase L), as .claude/tasks may be a symlink and those tools don't traverse symlinks
   - read all relevant files in the task directory to fully understand the work so far, excluding research questions documents

3. **Optional - Spawn parallel sub-tasks for comprehensive research**:
   - If not clear from the research, spawn sub-tasks to research different aspects concurrently
   - Create multiple Agent() calls to research different aspects concurrently
   - Use the right agent for each type of research:

   **For deeper investigation:**
   - **codebase-locator** - To find more specific files (e.g., "find all files that handle [specific component]")
   - **codebase-analyzer** - To understand implementation details (e.g., "analyze how [system] works")
   - **codebase-pattern-finder** - To find similar implementation patterns we can model after
   - **codebase-consumer-finder** - To find every caller of a function, endpoint, task payload, or column the change touches

   Each agent knows how to:
   - Find the right files and code patterns
   - Identify conventions and patterns to follow
   - Look for integration points and dependencies
   - Return specific file:line references
   - Find tests and examples

<important if="the user asks you to find how things work or add detail about existing functionality">
  prefer to use an initial pass with one or more Agent() calls before reading files yourself
  <else if="the user gives straightforward feedback that doesn't require loading more codebase context">
      skip Agent() calls if you already have the context
  </else>
</important>

4. **Read the design discussion template and visual guidance**

`Read({SKILLBASE}/references/design_discussion_template.md)`

`Read({SKILLBASE}/references/show-me.md)`

- Follow the `show-me` guidance throughout the design discussion.
- Use the smallest set of visual views needed to make the current state, proposed end state, and key design choices clear.
- Place each visual next to the short text it supports. Do not add visuals that do not help the user make a design decision.

5. **Write the design discussion** to `.claude/tasks/ENG-XXXX-description/ENG-XXXX-03-design.md`
   - First, check the task directory in your system prompt. If you don't see it, find the task directory: `ls -La .claude/tasks | grep -i "eng-XXXX"`
   - If the directory doesn't exist, create: `.claude/tasks/ENG-XXXX-description/`
   - Format: `ENG-XXXX-03-design.md`, where `ENG-XXXX` is the ticket ID (`GH-<N>` for a ticket opened locally; ask the user for an ID if there is none) and the slug is 2-4 words in kebab-case. The step number (`03`) is fixed, not a running index.
   - Example: `.claude/tasks/ENG-1478-parent-child-tracking/ENG-1478-03-design.md`

<content_guidance>
**Outline High-level product spec** - Current state, desired end state, what we're not doing in terms of user experience or functionality

**Outline the proposed end state architecture** - Use focused before-and-after diagrams, trees, pseudocode, or other views from the `show-me` guidance. Prefer `diff` blocks when the key point is what changes, and show the full target shape when it makes ownership or order clearer.

**Discuss design decisions**
   - For each major design choice, present options with pros/cons
   - Make recommendations based on codebase conventions
   - If the research surfaced testing patterns for the components being changed, include a brief testing approach (e.g. "follow the existing unit test pattern in `__tests__/foo.test.ts`")

**IMPORTANT: All design questions must be written in the OPEN state**
   - Put ALL questions under "Design Questions", NOT under "Resolved Design Questions"
   - You may recommend an option, but you must NOT resolve or close a question yourself
   - Only the user can resolve a design question — through explicit feedback or approval
   - The "Resolved Design Questions" section should be empty in the initial document
   - Do NOT auto-resolve questions even if the answer seems obvious from the research

**Resolving questions after user feedback**
   - When the user provides a clear answer (e.g. "for X we should do Y"), move the question to "Resolved Design Questions"
   - They don't need to say "resolve" explicitly — any clear indication of a decision counts
   - Record the final decision with rationale
   - Record the options that were considered but not chosen, with brief rationale for why they were discarded

**Present patterns to follow** 
   - Identify existing patterns in the codebase that should be followed
   - Include file locations and multiline code snippets showing the pattern

</content_guidance>


**Ask the open design questions in plain chat, never with `AskUserQuestion`**
   - After writing the document, ask the open design questions one per message. The tool's pop-up hides the context the user needs to decide.
   - Put the full context in the same message: the question, its options with tradeoffs, and your recommendation marked "(Recommended)". Quote the relevant part of the document instead of pointing at it.
   - A clear typed answer resolves the question. Move it to "Resolved Design Questions" with the decision, the rationale, and the options not chosen.
   - If the reply is a question or pushback, answer it, then restate the open decision. The question stays open.
   - Keep going until every question is resolved or the user stops, then update the document and republish it.

6. **Read the final output template**

`Read({SKILLBASE}/references/design_discussion_final_answer.md)`

If the document's Design Questions section has no open questions left, read the resolved template instead:

`Read({SKILLBASE}/references/design_discussion_final_answer_resolved.md)`

7. Respond following the selected template exactly. Do not include a summary or other information. Give the path of the saved file.

<guidance>
## Markdown Formatting

When writing markdown files that contain code blocks showing other markdown (like README examples or SKILL.md templates), use 4 backticks (````) for the outer fence so inner 3-backtick code blocks don't prematurely close it:

````markdown
# Example README
## Installation
```bash
npm install example
```
````

## Bugs Found While Planning

If reading the code shows a bug in code the work touches or depends on, make the fix part of the design.
- Record it as a design decision with the recommended fix, so the outline and plan carry it.
- Keep the question open until the user answers, like any other design question.
- Fix unrelated bugs too. Record each as its own design decision with the recommended fix, marked as outside the ticket's scope.

## Document Precedence

When documents conflict, the most recent document wins:
**design discussion > research > ticket**

Decisions made during the design discussion supersede the original ticket description.
The ticket provides the initial request; the design discussion refines and finalizes the approach.
</guidance>
