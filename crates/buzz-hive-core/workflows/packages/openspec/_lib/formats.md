# OpenSpec formats and rules

This file is the one place the OpenSpec workflow's document formats and rules are written. Each skill reads it by path. The wording follows OpenSpec's `spec-driven` schema.

## Layout in the repository

```
openspec/
  config.yaml                 project settings, optional
  specs/<capability-path>/spec.md      what the system does today
  changes/<change-name>/
    .openspec.yaml
    proposal.md
    specs/<capability-path>/spec.md    delta specs for this change
    design.md                          only when the change needs one
    tasks.md
  changes/archive/YYYY-MM-DD-<change-name>/    finished changes
```

`<capability-path>` is the spec folder relative to `specs/`, such as `user-auth` or `identity/user-auth`. Keep its full path. Use kebab-case for every segment you introduce. Follow the layout the project already uses.

A change name is kebab-case: lowercase letters, digits and single hyphens, starting with a letter.

## How Kiln passes the documents

During planning, each document is a Kiln artifact. Save it with the Kiln tool `save_artifact` and read it with `read_artifact`. Nothing is written to `openspec/changes/` until the Apply stage does it.

| Artifact | Holds |
|---|---|
| `proposal` | proposal.md, behind a short header (below) |
| `specs` | every delta spec file of the change, in one document (below) |
| `design` | design.md, or a one line note that no design is needed |
| `tasks` | tasks.md |
| `verify-report` | the Verify stage's report |
| `pr_body` | the pull request description the Archive stage writes: the proposal without its header, then the specs updated |

### The proposal header

The `proposal` artifact starts with this header. Every later stage reads the change name from it.

```
---
change: issue-<issue number>-<kebab-case slug of the title, at most five words>
skip_specs: false
---
```

Set `skip_specs: true` only when the change alters no spec-level behavior (a pure refactor, tooling or docs). Specs describe behavior, so if behavior does not change, no spec changes.

### The specs bundle

The `specs` artifact holds one delta spec file per capability the proposal lists. Each file starts with a marker line, written exactly like this on a line of its own:

```
<!-- file: specs/<capability-path>/spec.md -->
```

The file's content follows, up to the next marker or the end of the artifact. When `skip_specs` is true, the artifact is the single line `skip_specs: true` and holds no marker.

## proposal.md

```
# Proposal

## Why
1 or 2 sentences, at least 50 characters: the problem or opportunity, and why now.

## What Changes
Bullets of specific changes. Mark breaking changes with **BREAKING**.

## Capabilities

### New Capabilities
- `<capability-path>`: what this capability covers

### Modified Capabilities
- `<existing-capability-path>`: which requirement changes

## Impact
Affected code, APIs, dependencies and systems.
```

Name a capability for a durable system behavior (`user-auth`), not for the work in the change (`add-login-endpoint`). Avoid broad catch-all capabilities. A modified capability uses the exact existing path under `openspec/specs/`. Reuse an existing capability's path instead of inventing a near duplicate. Leave a list empty when it has nothing. Keep the proposal to one or two pages: the why and the what, not the how.

## Delta spec file

```
# Spec Delta

## Purpose
(new capabilities only) One or two sentences, at least 50 characters, on what the capability is for.

## ADDED Requirements

### Requirement: <name>
The system SHALL <behavior>.

#### Scenario: <name>
- **WHEN** <condition>
- **THEN** <expected outcome>

## MODIFIED Requirements
(the whole updated requirement block, every scenario included)

## REMOVED Requirements

### Requirement: <name>
**Reason**: <why>
**Migration**: <what to use instead>

## RENAMED Requirements
- FROM: `### Requirement: <old name>`
- TO: `### Requirement: <new name>`
```

Rules:
- Each requirement is `### Requirement: <name>` followed by its text. The text uses SHALL or MUST, never should or may, and is at most 500 characters. Put examples and edge cases in scenarios. State one behavior per requirement.
- Each scenario is `#### Scenario: <name>` with exactly four hash marks, then `**WHEN**` and `**THEN**` bullets and optional `**AND**` bullets. Three hash marks or plain bullets fail silently.
- Every requirement has at least one scenario.
- A delta for an existing capability has no `## Purpose`. To change an existing Purpose, edit the main spec directly.
- A delta needs at least one operation. Use only the four section names above. A requirement outside them is ignored.
- MODIFIED holds the complete requirement block copied from the main spec and then edited. A partial block loses detail at merge. If you add a new concern without changing existing behavior, use ADDED.
- A header must match the main spec's header exactly, ignoring case and extra whitespace. When a rename exists, MODIFIED uses the new name.
- A requirement appears in only one section of a delta. A RENAMED entry has both FROM and TO. A TO name must not also appear under ADDED.
- A spec describes behavior a user or another system can observe: inputs, outputs, errors and external constraints. Leave out class names, library choices and step by step plans. If the implementation can change without changing visible behavior, it does not belong in a spec.

## design.md

Write one only when the change is cross-cutting, adds a dependency or changes a data model, has security, performance or migration concerns, or holds an ambiguity that technical decisions should settle before coding.

```
# Design

## Context
Only the current state and constraints that explain the approach. Point to the proposal for motivation.

## Goals / Non-Goals
**Goals:** ...
**Non-Goals:** ...

## Decisions
Each key choice with its reason and the alternatives considered.

## Risks / Trade-offs
[Risk] -> Mitigation

## Migration Plan
Steps to deploy and roll back, when it applies.

## Open Questions
Only unknowns that can wait without changing the specs, the approach or the tasks. Omit when none.
```

A question that would change the specs, the approach or the tasks is not an open question. Resolve it now.

## tasks.md

```
# Tasks

## 1. <group name>

- [ ] 1.1 <task, and how to verify it is done>
- [ ] 1.2 <task, and how to verify it is done>
```

- Group related tasks under numbered `##` headings. Each tracked task is a checkbox line `- [ ] X.Y <description>`. A box holding only `x` counts as done. Any other marker, or a line with no checkbox, is unfinished or untracked.
- Keep each task small enough for one session, ordered by dependency.
- Each task states how to verify it: a test, a command, an observable behavior or a delivered file.
- Each group lands the tests and documentation its own work calls for. Do not collect tests or docs into a final group. A final group is for integration checks only.
- Do not add archive as a tracked task. Kiln's Archive stage does it.

## Project settings

If `openspec/config.yaml` exists, read it before writing a document. Its `context` text and its `rules.<artifact>` entry (`proposal`, `specs`, `design` or `tasks`) are constraints on what you write. Never copy them into a document.

## Validation rules

A change is valid when:
- the proposal's Why section is 50 to 1000 characters and What Changes is not empty;
- `skip_specs` is true and the bundle holds no marker, or the bundle holds at least one delta file;
- every delta file has at least one operation;
- every requirement has SHALL or MUST and at least one scenario;
- no requirement is in two sections of one delta;
- a new capability's delta has a Purpose of at least 50 characters.

A change with more than 10 deltas is a warning: consider splitting it.

## Merging deltas into the main specs

For each delta file, the main spec is `openspec/specs/<capability-path>/spec.md`. Apply the sections in this order: RENAMED, REMOVED, MODIFIED, ADDED.

- RENAMED: find the FROM requirement and give it the TO name. If FROM is gone and TO exists, it is already done. If both are missing, stop and report.
- REMOVED: delete the whole requirement block. If it is already gone, skip it and note it.
- MODIFIED: replace the requirement with the delta's block. If the requirement is missing, stop and report. Keep every scenario the main spec has unless the delta's block removes it on purpose, and say so.
- ADDED: add the requirement. If it exists with different content, stop and report. If it exists with the same content, it is already done.
- New capability (no main spec yet): only ADDED is allowed. Create the spec from the delta's `## Purpose` and the ADDED requirements. If the delta has MODIFIED or RENAMED, stop and report.
- The delta's `## Purpose` is ignored when the main spec exists.
- Never leave an empty `## Requirements` section. Do not delete a capability's spec file unless the change's `.openspec.yaml` says `retire_capabilities: true` and the removal leaves no requirement.

A main spec looks like this:

```
# <Capability> Specification

## Purpose
...

## Requirements

### Requirement: <name>
The system SHALL ...

#### Scenario: <name>
- **WHEN** ...
- **THEN** ...
```
