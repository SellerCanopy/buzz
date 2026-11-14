<!-- /07-code-review reviewer contract. The workflow's reviewer subagents return findings through
     the StructuredOutput schema in code-review.workflow.js. The severity guide is mirrored in the
     workflow's reviewer prompt; the domain map keeps the fourteen reviewers non-overlapping and
     marks the eight that run by default plus the one that is opt-in. -->

## Severity guide (passed to reviewers)

Tag by **overall engineering impact at scale** (a system serving millions of customers): correctness, security, production blast radius, AND maintainability/structure. Not by how "real" the bug is:

- **Critical** - at scale, causes data loss / corruption, a security breach (auth bypass, cross-tenant exposure, injection, secret leak), an outage, money/billing incorrectness, or a compliance violation. Exploitable / triggerable now.
- **High** - one of the above under realistic load or a motivated attacker, or a latent version that detonates as traffic/data grows (e.g. an N+1 fine at 100 rows and fatal at 10M); OR a structural defect that actively causes bugs or blocks change across the codebase.
- **Medium** - a real defect with limited blast radius, OR a maintainability/structure problem that measurably raises the cost or risk of change: a god function/class, a layering or boundary violation, duplication that will drift, misleading naming, dead code that hides intent, needless complexity with a demonstrably simpler equivalent, a missing test on a real path.
- **Low** - a cosmetic nit or opportunistic cleanup with no correctness, blast-radius, or real maintainability cost.

**Report every tier - do not self-filter.** The orchestrator applies a **severity gate** over your findings (Medium and above is scheduled for fixing, Low is logged) and decides what gets fixed vs. logged; trimming your list to "only the important ones" lowers recall and is the one thing you must not do. Pure style/formatting is the only safe drop. If two subagents disagree on severity for the same finding, the orchestrator takes the higher.

## Scope: the committed diff, not the files

Every reviewer audits `git diff <base>...HEAD`, which the workflow passes verbatim in the prompt. A defect counts only when this diff introduces it: in a line it adds or changes, or in existing code that the change now breaks, mis-uses, or leaves inconsistent. Surrounding files are read to judge whether a candidate is real and to find the other side of a contract the diff touches, never to hunt for new material. A pre-existing defect in a touched file is not a finding, and the workflow drops any finding whose path is outside the changed-file list before the severity gate. Files read from disk are the working tree and may carry uncommitted edits that are not under review; the diff in the prompt is.

## Reviewer domain map

Each subagent owns a non-overlapping domain - do not duplicate categories across calls. The eight marked *core* run by default; the rest join with the `fullRoster` flag, except **brand-reviewer**, which is in neither roster and runs only with the `brand` flag.

- **code-quality-reviewer** (core) - control flow, nil safety, API contracts, dead code, magic numbers, naming.
- **error-handling-reviewer** (core) - try/except hygiene, async rejections, boundary input validation, generic error responses, errors that do not say what failed, missing retries / fallbacks / circuit breakers.
- **concurrency-hazard-reviewer** (core) - races, mutation-while-iter, deadlocks, lock ordering, atomicity, cancellation, closure capture, out-of-order async.
- **security-reviewer** (core) - authn/authz, injection, SSRF/deserialization, secrets, crypto, transport, abuse, deps.
- **architecture-reviewer** (core) - misplaced responsibilities, layering violations, poor cohesion, boundary breaches, excessive coupling, circular dependencies, wrong abstraction level.
- **test-coverage-reviewer** (core) - untested new paths, vacuous tests, missing edge cases, mock-induced false positives, weakened coverage, missing regression test.
- **ssot-reviewer** (core) - one fact declared authoritatively in two or more artifacts, layers, or formats with nothing deriving one from the other: restated schemas (migration vs ORM vs type vs spec), config with two owners (code default vs compose vs chart vs CI), API contracts, error/permission/flag/event catalogs, version and identity drift, hand-edited or stale generated files, a business rule reimplemented per layer. Every finding names the canonical source and the missing binding.
- **simplification-reviewer** (core) - unnecessary complexity: single-use abstractions, speculative options and extension points, pass-through layers, deep nesting and tangled conditionals, clever code a plain version would replace, reinvented standard facilities, needless data indirection, over-built control flow, control flow or state hidden where a debugger cannot follow it. Every finding carries the simpler version and why it behaves the same for every caller. Severity is capped at Medium, because a simplification rewrites working code; the workflow enforces the cap after the gate.
- **resource-lifecycle-reviewer** - timers, listeners, handles, streams, transactions, unbounded caches, captured closures (covers memory leaks).
- **performance-reviewer** - N+1, blocking I/O, complexity, indexes, pagination, allocation, contention, pool exhaustion, missing caching.
- **observability-reviewer** - missing logs at critical paths, wrong log levels, ad-hoc print/console.log, sensitive data in logs, missing metrics/tracing/correlation ids.
- **dry-enforcer** - copy-paste ≥6 lines, parallel switch/enum drift, duplicated constants, god functions/classes, forked types.
- **documentation-accuracy-reviewer** - stale comments, missing public-API/config docs, inaccurate examples, CHANGELOG/README/schema drift.
- **brand-reviewer** (opt-in, `brand` flag) - user-facing copy and brand values only: product/feature/partner naming, terminology drift, voice and tone, banned phrases, required legal and attribution notices, copy mechanics, hardcoded colors/fonts/logos bypassing design tokens, i18n bypass. Every finding cites the guide line or the dominant in-repo convention it violates, and carries the replacement string.

**Boundary between the three duplication lenses.** dry-enforcer owns duplication *inside one layer or language*, where the fix is extracting a helper. ssot-reviewer owns *one fact with two owners across artifacts or formats*, where the fix is designating a canonical source and deriving the rest. documentation-accuracy-reviewer owns prose that *describes* code wrongly with no second declaration behind it.

**Boundary between the three structure lenses.** dry-enforcer owns code doing *too many jobs* (god functions and classes) and code repeated. architecture-reviewer owns code *in the wrong place*, where the fix is moving it. simplification-reviewer owns code with *too much structure for one job*, where the fix is deleting a layer or collapsing structure in place. code-quality-reviewer keeps code that is *wrong*; simplification-reviewer takes code that is correct but longer than it needs to be.
