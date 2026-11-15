---
name: 05-create-plan
description: Only use when the user explicitly invokes this skill by name.
model: claude-opus-5-5
effort: high
---

# Create Plan

You are in the final Plan Writing phase. Turn the research and the issue into a complete, detailed implementation plan. This workflow has no design or outline step, so you split the work into phases yourself.

## Steps

1. **Read all input files FULLY**:
   - Use Read tool WITHOUT limit/offset to read all provided file paths
   - `ls -La .claude/tasks/TASKNAME` to find all related documents in the task directory. Do NOT use the Grep or Glob tools, or `ls -l` (lower case L) as the directory may be a symlink.
   - Read everything in the task directory to build full context. The research document is your main input.

2. **Read relevant code files**:
   - Read any source files mentioned in the research
   - Build context for writing specific code examples

3. **Read the plan template**:

`Read({SKILLBASE}/references/plan_template.md)`

4. **Write the implementation plan**:
   - Write to `.claude/tasks/ENG-XXXX-description/ENG-XXXX-05-plan.md`
   - The step number (`05`) is fixed, not a running index. Example: `ENG-1478-05-plan.md`. `ENG-XXXX` is the ticket ID (`GH-<N>` for a ticket opened locally).
   - Split the work into small phases, each independently testable, and write detailed implementation steps for each
   - Include specific code examples for each change
   - Add both automated and manual success criteria

## Plan Writing Guidelines

- Each phase should be independently testable
- Include specific code examples, not just descriptions
- Automated verification should be runnable commands
- Manual verification should be specific, actionable steps
- Pause for human confirmation between phases
- If the research documented testing patterns for the components being changed, include test code in the plan (new test files or additions to existing test files). Follow the existing test patterns found in the research.

## Bugs Found While Planning

If reading the code shows a bug in code the work touches or depends on, put the fix in the plan. Do not leave it as a note.
- Give the fix its own numbered change in the phase that touches the file, with a diff.
- Add an automated check that fails without the fix.
- If the fix needs a decision the documents do not settle, such as a contract change with other callers, ask the user before writing the plan. Then put the chosen fix in the plan.
- Fix unrelated bugs too. Put them in a final phase named "Bugs found while planning", each with a diff and a check that fails without the fix.

## Document Precedence

When documents conflict, the most recent document wins:
**plan > research > ticket**

The plan is the final authority. Follow the research over the original ticket when they differ.

## Output

1. **Check if worktree setup should be skipped**:

```
Read(.claude/workspace.json)
Read(.claude/workspace.local.json)
Bash(git rev-parse --git-dir)
```

2. **Read the appropriate final output template**:

<condition if=" OR git-dir output contains '.git/worktrees/'">

`Read({SKILLBASE}/references/plan_final_answer_in_worktree.md)`

<else if="disabled is true (workspace.local.json takes precedence over workspace.json)">

Check out the task branch for the user, defaulting to the task slug at .claude/tasks/TASKSLUG

`Bash(git checkout -b [BRANCHNAME])`
`Read({SKILLBASE}/references/plan_final_answer_disabled.md)`

<else>

`Read({SKILLBASE}/references/plan_final_answer.md)`

</condition>

3. Respond following the selected template exactly. Do not include a summary or other information. Give the path of the saved file.

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

## Validation Design

Not every phase requires manual validation, don't put steps for manual validation just to have them. 
</guidance>
