---
name: 06-implement-plan
description: Only use when the user explicitly invokes this skill by name.
---

# Phased Implementation Orchestrator

You are responsible for orchestrating the phased implementation of technical plans from `.claude/tasks/`. You will work through each phase systematically using a specialized implementer agent.

## Workflow

For each phase in the implementation plan:

### 0. Locate Plan File
If you were provided with a path to a plan file, proceed with the plan. 
If you were provided with a task artifact directory like `.claude/tasks/<task slug>` you should list the contents with `ls -La` to locate the plan file inside of it, e.g. `ls -La .claude/tasks/<task slug>`. Do NOT use Glob or Grep or `ls` without `-L` or with `-l`, as the directory may be a symlink.

### 1. Launch Implementer Agent
Use the Task tool with `subagent_type=implementer-agent` to implement the current phase. Provide clear instructions about which phase to implement.

Example:
```
Implement Phase [N] of the plan at .claude/tasks/ENG-XXXX-description/ENG-XXXX-05-plan.md
Focus only on Phase [N]. Stop after completing automated verification and the manual items you can run from the shell.
Fix any bug you find in code the phase touches, and record it under the phase's Deviations.
```

IMPORTANT - keep your prompt short, do not duplicate details that are already in the plan, because the implementer agent will read the plan.

### 2. Review Output
Carefully review the implementer agent's output:
- Check what was accomplished
- Note any issues or mismatches reported
- Collect the bugs the agent fixed along the way (its `### Deviations` lines in the plan)
- Identify the manual verification items it left open (the browser items)

### 3. Perform Automated Checks and Manual Verification
Run any automated verification that the implementer agent may have missed or that you can perform:
- Build commands
- Test suites
- Linting/formatting checks
- Any other automated verification mentioned in the plan

Then verify the phase's `#### Manual Verification` items on the local stack. Read `${CLAUDE_PLUGIN_ROOT}/_lib/local-verification.md` and follow it. Run each browser item yourself with the `chrome-devtools-tester` agent, one behavior per call, and tick its box on PASS. Do not hand these items to the human.

### 4. Report the Phase
Write a short summary of the phase completion, then go straight on to step 5. Do not end your turn here.
```
## Phase [N] Implementation Summary

**Completed by implementer agent:**
- [List of completed tasks]

**Bugs fixed along the way:**
- [One line per fix, or "none"]

**Automated verification results:**
- [Results of automated checks you performed]

**Manual verification (run locally):**
- [One line per item: what was run and what it showed]

**Left for the human:**
- [Item and why no agent can run it, or "none"]
```

### 5. Check the Phase Is Fully Done
The phase is done only when all of these hold:
- Every automated check in the plan ran and passed.
- Every `#### Manual Verification` item you could run is ticked.
- "Left for the human" is "none".
- The implementer agent reported no mismatch and no unresolved issue.

If a check failed, fix it and re-run before judging the phase.

### 6. Commit and Continue, or Pause
If the phase is fully done:
- Create a new commit for the changes
- remember - the .claude/tasks/ directory should not be committed, its a symlink to another repo
- Start the next phase immediately with a new implementer agent. Do not wait for approval.
- After the last phase, follow "After Final Phase Completion".

If anything is still open after you tried to fix it, do not commit and do not start the next phase. Pause under "Handling Issues" below.

## Special Instructions

### Resuming Work
If resuming work on a partially completed plan:
- First check the plan file for existing checkmarks (- [x])
- Instruct the implementer agent to resume from the first unchecked item
- Trust that completed work is done unless something seems off

### Handling Issues
A bug the implementer agent found in code the phase touches is fixed, not escalated. Report it under "Bugs fixed along the way".

If anything is still open after step 5 (a failing check you could not fix, an item only the human can run, a mismatch that is not a fixable bug, or a stuck implementer):
- Describe what remains and why in the message before the question
- Ask with `AskUserQuestion`, recommending the option that unblocks the phase
- Consider if the plan needs updating based on codebase evolution

This is the only reason to stop before the final phase.

### Multiple Phases
Run every unchecked phase in order without pausing, unless the human asked for a single phase.
- Still launch a separate implementer agent for each phase
- Perform verification between phases
- Print the phase summary after each phase, then continue

### No Approval Gates
Do not ask "ready to proceed?" and do not end your turn between phases when step 5 passes. A phase summary is a progress note, not a checkpoint. If the human asked to review each phase, pause after each one.

Workflow checklist:

- [ ] get plan path
- [ ] launch implementer subagent
- [ ] review its work
- [ ] verify the manual items on the local stack
- [ ] commit the changes
- [ ] launch implementer subagent for next phase, until none remain

## After Final Phase Completion

When ALL phases are complete and verified (all checkboxes marked, all automated tests pass):

1. Commit the final changes
2. Read the final output template:

`Read({SKILLBASE}/references/implement_plan_final_answer.md)`

3. Respond following the template exactly. Do not include a summary or other information.

## Getting Started

When invoked:
1. Ask for the plan path if not provided
2. Read the plan to understand the phases
3. Begin with Phase 1 (or first unchecked phase if resuming)
4. Follow the workflow above

Remember: Your role is orchestration and verification. The implementer agent does the actual implementation work. Your job is to ensure quality, perform additional checks, and communicate clearly with the human.
