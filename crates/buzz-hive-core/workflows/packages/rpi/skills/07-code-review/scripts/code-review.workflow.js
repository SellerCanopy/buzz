export const meta = {
  name: 'code-review',
  description: 'Audit a branch or pull request diff: eight core specialist reviewers by default (thirteen with args.fullRoster, plus an opt-in brand/voice lens with args.brand), high-recall, severity-gated (Medium and above scheduled for fixing, Low appendixed), each scheduled finding adversarially verified before it reaches the fix plan (one skeptic, two for Critical). Report-only: the fix plan ENG-XXXX-07-qa-plan.md is assembled in code in /05-create-plan format and returned as result.doc for the skill to save; no code is edited.',
  phases: [
    { title: 'Recall', detail: 'specialist reviewers (8 core, 13 with fullRoster, +1 with brand) + plan-fidelity audit the diff in parallel' },
    { title: 'Gate', detail: 'one gate agent normalizes severity; Medium+ is scheduled, Low is appendixed' },
    { title: 'Verify', detail: 'adversarial skeptics refute each scheduled finding (2 for Critical, 1 otherwise)' },
  ],
}

// ---------------------------------------------------------------------------
// args contract (passed by the 07-code-review skill):
//   diffBase:        string?  diff base ref (default 'main'). The skill resolves it: the PR's base in PR
//                             mode, else the epic integration branch or the repo default
//   headRef:         string?  the head of the reviewed range (default 'HEAD'; a PR's head SHA in PR mode)
//   branch:          string?  branch under review (display only)
//   pr:              string?  '#<N> <url>' when reviewing a pull request (display only)
//   reviewRoot:      string?  absolute path of the checkout under review, when it is not the session's
//                             own working tree (a PR reviewed in a temporary worktree). Every agent
//                             reads files there, and reported paths are made relative to it. Omit to
//                             review the session's own checkout
//   changedFiles:    string[] from `git diff <base>...<head> --name-only`. Doubles as the scope
//                             filter: a finding outside these paths is dropped before the gate
//   diffText:        string?  verbatim `git diff <base>...<head>`. The specialists have no Bash, so
//                             without this they read whole files off disk and review pre-existing
//                             code and uncommitted edits. Always pass it
//   maxDiffChars:    number?  default 120000. Diffs above this are cut at a file boundary and the
//                             omitted paths are named in the prompt
//   planPath:        string?  absolute ENG-*-05-plan.md, or the ENG-*-04-decompose.md the branch was
//                             built from when no plan exists (enables plan-fidelity)
//   outputPath:      string   absolute path to write ENG-*-07-qa-plan.md
//   fullRoster:      boolean? default false. true runs all thirteen roster specialists; off, the
//                             eight core lenses run (code-quality, error-handling, concurrency,
//                             security, architecture, test-coverage, ssot, simplification)
//   brand:           boolean? default false. true adds the brand-reviewer (user-facing copy, naming,
//                             voice, required notices, design-token bypass). Independent of
//                             fullRoster: it is in neither roster and only this flag runs it
//   model:           string?  force ALL agents to one tier; omit to use per-stage tiering
//                             (code-quality/error-handling/concurrency/security reviewers + gate
//                             + verify lenses = Sonnet 5.5; all other reviewers + plan-fidelity
//                             = sonnet)
//   effort:          string?  force ALL agents to one effort; omit to use per-stage tiering
//                             (deep reviewers + gate + Critical verify = high; other
//                             routine reviewers + plan-fidelity + Medium verify = medium; High verify = high),
//                             regardless of the parent session's own effort
//   featureName:     string?  the feature's name for the plan title (the source plan's title)
//   checkCommands:   string[]? the project's automated checks (unit tests, type check, lint), one
//                             command each, usually the source plan's Automated Verification lines
//   featureCheck:    string?  one line: what to observe locally to confirm the branch's feature still
//                             works end-to-end, and how to see it
//   references:      string[]? the upstream artifacts, one `- ` line body each (`Ticket: <path>`)
//   date:            string?  today's date for the header (scripts cannot read the clock)
//   task:            string?  the task directory name (ENG-1234-description) for the plan's frontmatter
//   repo:            string?  the repository name for the plan's frontmatter
//   sha:             string?  the reviewed head commit (`git rev-parse HEAD`, or the PR's head SHA)
//                             for the plan's frontmatter
// The plan's format lives in renderPlan() below; every slot that needs judgment comes in as an
// arg above, so no agent writes the plan. A missing arg gets a generic line, not a failure.
//
// Flow (four stages; plan-fidelity runs concurrently and is collected before Write):
//   [Recall]  specialists (8 core, 13 with fullRoster, +brand-reviewer with args.brand), parallel,
//        |    read-only barrier. plan-fidelity
//        |    is launched alongside but kept OUT of the barrier (its result feeds only the plan).
//        v
//   merge + dedupe (in-script, no agent)
//        |
//        v
//   [Gate]    gate agents normalize severity, one per batch of findings, in parallel;
//        |    Medium+ is scheduled, Low is appendixed
//        v    (no barrier: each batch's scheduled findings go straight on to Verify)
//   [Verify]  adversarial skeptics per scheduled finding (2 lenses for Critical, 1 otherwise);
//        |    a finding dies only if every lens refutes it
//        v
//   renderPlan() assembles the qa-plan in code (only when at least one finding survived)
//        |
//        v
//   return { plan_path, doc, scheduled, scheduled_severity_counts, scheduled_category_counts,
//            refuted, non_blocking, plan_fidelity, engine_breakdown }
//
// Why this shape: subagents cannot spawn subagents, so all fan-out lives here.
// Mirrors review-design.workflow.js: the args shim, parallel() finder fan-out with per-agent
// schema, in-script merge/dedupe, withRetry, and a structured return.
// ---------------------------------------------------------------------------

// args delivered as a JSON STRING by the harness - normalize before reading (same shim as review-design.workflow.js).
const A = (typeof args === 'string')
  ? (function () { try { return JSON.parse(args) } catch (e) { return {} } })()
  : (args || {})

const diffBase       = A.diffBase || 'main'
const headRef        = A.headRef || 'HEAD'
const RANGE          = diffBase + '...' + headRef
const branch         = A.branch || '(current)'
const prLabel        = A.pr || null
const reviewRoot     = A.reviewRoot ? String(A.reviewRoot).replace(/\/+$/, '') : null
if (reviewRoot && !reviewRoot.startsWith('/')) {
  throw new Error('code-review.workflow.js: args.reviewRoot must be an absolute path. Received: ' + reviewRoot)
}
// Every agent that reads code gets this when the reviewed checkout is not the session's working tree.
// Without it they would read the session's copy, which holds a different version of the code.
const ROOT_NOTE = reviewRoot
  ? 'The code under review is checked out at `' + reviewRoot + '`. Resolve every path in the diff against that directory and read files ONLY there (Read/Grep/Glob with absolute paths under it); the session\'s own working directory holds a different version of the code and is not under review. Report file paths relative to that directory.'
  : ''
const changedFiles   = A.changedFiles || []          // string[] from `git diff <base>...<head> --name-only`
const planPath       = A.planPath || null            // ENG-*-05-plan.md, for plan-fidelity (optional)
// The unified diff of the committed range, verbatim from `git diff <base>...HEAD`. The specialists
// run with Read/Glob/Grep and no Bash, so this is the ONLY way they can see what actually changed:
// without it they get a file list, read each file whole off disk, and review pre-existing code and
// uncommitted working-tree edits along with the branch's own changes.
const diffText       = typeof A.diffText === 'string' ? A.diffText : ''
const outputPath     = A.outputPath                  // absolute path to write ENG-*-07-qa-plan.md
// outputPath is the one required arg. Unvalidated, a missing value only surfaces after the whole
// fan-out is billed, when the plan is returned for a path of `undefined`. Fail up front and name the arg
// (fail fast on a missing required arg).
if (typeof outputPath !== 'string' || outputPath.trim() === '') {
  throw new Error(
    'code-review.workflow.js: args.outputPath is required and must be an absolute path to the ' +
    'ENG-*-07-qa-plan.md to write (see 07-code-review/SKILL.md). Received: ' +
    (outputPath === undefined ? 'undefined' : typeof outputPath !== 'string' ? typeof outputPath : 'an empty string') + '.'
  )
}
if (!outputPath.startsWith('/')) {
  throw new Error(
    'code-review.workflow.js: args.outputPath must be an absolute path (the parent expands `~` ' +
    'before calling; the skill saves the plan from a cwd this script cannot see). Received: ' + outputPath
  )
}
const FULL_ROSTER    = A.fullRoster === true          // default: the eight core lenses
const BRAND_REVIEW   = A.brand === true                // opt-in brand/voice lens, outside both rosters
const model = A.model || 'claude-sonnet-5-5'
// High and Critical verification, and the security and concurrency specialists, default to Opus 5.5.
const opusModel      = A.model || 'claude-opus-5-5'
// Light-reasoning agents (plan-fidelity diff narration) run a cheaper tier by
// default. The gate and substantive adversarial verification use high effort.
const cheapModel     = A.model || 'claude-sonnet-5-5'
// Effort tiering, same override convention as model. Explicit on every call so the mechanical
// plan-fidelity stage never silently rides whatever effort the parent session happens to be
// at, and the deep reviewers/gate/heavy-verify always get real reasoning (high) rather than an
// accidental max. Medium-tier verify drops to cheap, realizing "verify effort tracks blast radius"
// on the effort axis (it was already true for lens count, not for effort, until now).
const cheapEffort    = A.effort || 'medium'
const strongEffort   = A.effort || 'high'
const featureName    = A.featureName || ''
const checkCommands  = Array.isArray(A.checkCommands) ? A.checkCommands.filter(Boolean) : []
const featureCheck   = A.featureCheck || ''
const references     = Array.isArray(A.references) ? A.references.filter(Boolean) : []
const planDate       = A.date || ''
const taskName       = A.task || ''
const repoName       = A.repo || ''
const headSha        = A.sha || ''

// Fourteen non-overlapping domains, verbatim from reviewer-contract.md. `core: true` marks the
// eight that run by default; five more join with args.fullRoster. `optIn: 'brand'` marks the brand/voice
// lens, which is in neither roster and runs only with args.brand (it audits user-facing copy and
// brand values, which most branches do not touch, and it needs a brand guide to be worth billing).
// Every specialist defaults to Sonnet 5.5, except security and concurrency (Opus 5.5). Substantive reviews use high effort; routine
// documentation, duplication, observability, and brand checks use medium.
// args.model overrides every tier.
const ALL_REVIEWERS = [
  { key: 'code-quality-reviewer',          core: true,  tier: 'claude-sonnet-5-5', effort: strongEffort, focus: 'control flow, nil safety, API contracts, dead code, magic numbers, naming' },
  { key: 'error-handling-reviewer',        core: true,  tier: 'claude-sonnet-5-5', effort: strongEffort, focus: 'try/except hygiene, async rejections, boundary input validation, generic error responses, errors that do not say what failed or on which input (Medium at most), missing retries / fallbacks / circuit breakers' },
  { key: 'resource-lifecycle-reviewer',    core: false, tier: 'claude-sonnet-5-5', effort: strongEffort,  focus: 'timers, listeners, handles, streams, transactions, unbounded caches, captured closures (memory leaks)' },
  { key: 'concurrency-hazard-reviewer',    core: true,  tier: 'claude-opus-5-5', effort: strongEffort, focus: 'races, mutation-while-iter, deadlocks, lock ordering, atomicity, cancellation, closure capture, out-of-order async' },
  { key: 'security-reviewer',              core: true,  tier: 'claude-opus-5-5', effort: strongEffort, focus: 'authn/authz, injection, SSRF/deserialization, secrets, crypto, transport, abuse, deps' },
  { key: 'performance-reviewer',           core: false, tier: 'claude-sonnet-5-5', effort: strongEffort,  focus: 'N+1, blocking I/O, complexity, indexes, pagination, allocation, contention, pool exhaustion, missing caching' },
  { key: 'observability-reviewer',         core: false, tier: 'claude-sonnet-5-5', effort: cheapEffort,  focus: 'missing logs at critical paths, wrong log levels, ad-hoc print/console.log, sensitive data in logs, missing metrics/tracing/correlation ids' },
  { key: 'dry-enforcer',                   core: false, tier: 'claude-sonnet-5-5', effort: cheapEffort,  focus: 'copy-paste >=6 lines, parallel switch/enum drift, duplicated constants, god functions/classes, forked types' },
  { key: 'test-coverage-reviewer',         core: true,  tier: 'claude-sonnet-5-5', effort: strongEffort,  focus: 'untested new paths, vacuous tests, missing edge cases, mock-induced false positives, weakened coverage, missing regression test' },
  { key: 'documentation-accuracy-reviewer',core: false, tier: 'claude-sonnet-5-5', effort: cheapEffort,  focus: 'stale comments, missing public-API/config docs, inaccurate examples, CHANGELOG/README/schema drift' },
  { key: 'architecture-reviewer',          core: true,  tier: 'claude-sonnet-5-5', effort: strongEffort,  focus: 'misplaced responsibilities, layering violations, poor cohesion, boundary breaches, excessive coupling, circular dependencies, wrong abstraction level' },
  { key: 'ssot-reviewer',                  core: true,  tier: 'claude-sonnet-5-5', effort: strongEffort,  focus: 'one fact declared authoritatively in two or more artifacts/layers/formats with nothing deriving one from the other: restated schemas, config with two owners, API contracts, error/permission/flag catalogs, version and identity drift, hand-edited or stale generated files, business rules reimplemented per layer' },
  { key: 'simplification-reviewer',        core: true,  tier: 'claude-sonnet-5-5', effort: strongEffort,  focus: 'unnecessary complexity: single-use abstractions, speculative options/flags/extension points, pass-through layers, deep nesting and tangled conditionals, clever code a plain version would replace, reinvented standard facilities, needless data indirection, over-built control flow, control flow or state hidden where a debugger cannot follow it (decorators, callbacks registered elsewhere, framework magic, mutable globals, implicit caches). Every finding carries the simpler version in `fix` and says in `problem` why it behaves the same for every caller. Severity is capped at Medium' },
  { key: 'brand-reviewer',                 core: false, optIn: 'brand', tier: 'claude-sonnet-5-5', effort: cheapEffort,  focus: 'user-facing copy and brand values only: product/feature/partner naming, terminology drift, voice and tone, banned phrases and required legal notices, copy mechanics, hardcoded colors/fonts/logos bypassing design tokens, i18n bypass' },
]
const REVIEWERS = ALL_REVIEWERS.filter(function (r) {
  if (r.optIn === 'brand') return BRAND_REVIEW     // never in core or full; rides its own flag
  return FULL_ROSTER || r.core
})

const FINDING_SCHEMA = {
  type: 'object',
  required: ['findings'],
  properties: {
    findings: {
      type: 'array',
      description: 'Zero or more findings. Empty array if the diff is clean for your domain. Report EVERY defect, every tier - do not self-filter; the orchestrator gates.',
      items: {
        type: 'object',
        required: ['category', 'title', 'file', 'severity', 'confidence', 'problem', 'fix'],
        properties: {
          category:   { type: 'string', description: 'your reviewer domain, e.g. security, performance' },
          title:      { type: 'string' },
          file:       { type: 'string', description: '`path/to/file.ext:LINE`' },
          severity:   { enum: ['Critical', 'High', 'Medium', 'Low'] },
          confidence: { enum: ['HIGH', 'MED', 'LOW'] },
          problem:    { type: 'string', description: "what's wrong and why it matters in this codebase" },
          code:       { type: 'string', description: 'the exact offending snippet' },
          fix:        { type: 'string', description: 'concrete corrected approach or code' },
        },
      },
    },
  },
}

// Four-bucket plan-fidelity reconciliation (non-judgmental). This schema is the contract the
// plan-fidelity-reviewer returns; the agent body carries only the domain and a no-schema fallback.
const FIDELITY_SCHEMA = {
  type: 'object',
  required: ['implemented_as_planned', 'deviations', 'additions_not_in_plan', 'planned_but_not_implemented'],
  properties: {
    implemented_as_planned: {
      type: 'array',
      description: 'Plan items the diff implements substantially as written. One short line each.',
      items: { type: 'string' },
    },
    deviations: {
      type: 'array',
      description: 'Plan items the diff implements DIFFERENTLY than written. State what the plan said vs. what shipped, no judgment.',
      items: { type: 'string' },
    },
    additions_not_in_plan: {
      type: 'array',
      description: 'Changes present in the diff that the plan did not call for. One short line each.',
      items: { type: 'string' },
    },
    planned_but_not_implemented: {
      type: 'array',
      description: 'Plan items with NO covering change in the diff - unshipped work hiding behind a green branch. Flag any planned item or success criterion that never landed.',
      items: { type: 'string' },
    },
  },
}

// A very large diff cannot ride in every reviewer prompt. Cut at a file boundary rather than
// mid-hunk (a truncated hunk reads as deleted code and manufactures findings), and name the files
// that did not fit so the reviewer reads those from disk instead of assuming they are unchanged.
const MAX_DIFF_CHARS = typeof A.maxDiffChars === 'number' ? A.maxDiffChars : 120000

function diffBlock() {
  if (!diffText) return null
  if (diffText.length <= MAX_DIFF_CHARS) return { text: diffText, omitted: [] }
  const parts = diffText.split(/^(?=diff --git )/m)
  const kept = []
  const omitted = []
  let used = 0
  parts.forEach(function (chunk) {
    const m = /^diff --git a\/(\S+)/.exec(chunk)
    if (used + chunk.length <= MAX_DIFF_CHARS) { kept.push(chunk); used += chunk.length }
    else if (m) { omitted.push(m[1]) }
  })
  return { text: kept.join(''), omitted: omitted }
}

const DIFF = diffBlock()

function reviewerPrompt(r) {
  return [
    'You are the ' + r.key + ', an INDEPENDENT specialist reviewer auditing a branch diff. Edit NOTHING. Report findings only.',
    '',
    ROOT_NOTE,
    'Scope: the committed diff `git diff ' + RANGE + '`, below. Changed files:',
    changedFiles.map(function (f) { return '  - ' + f }).join('\n'),
    '',
    'THE DIFF IS THE SUBJECT; the files around it are only context. Report a defect only when THIS diff introduces it - in a line it adds or changes, or in existing code that this change now breaks, mis-uses, or leaves inconsistent. Read the surrounding files with Read/Grep/Glob to judge whether a candidate finding is real and to find the other side of a contract the diff touches, never to hunt for new material. A pre-existing defect in a file this diff happens to touch is NOT a finding, and neither is anything in a file outside the list above; the orchestrator drops findings outside these files. What you read from disk is the working tree, which may hold uncommitted edits that are not part of this review - the diff below is what is under review.',
    '',
    DIFF
      ? '```diff\n' + DIFF.text + '\n```' + (DIFF.omitted.length ? '\n(Diff truncated at a file boundary. Not shown, read these from disk: ' + DIFF.omitted.join(', ') + ')' : '')
      : '(The unified diff was not supplied. Reconstruct the change from the files above and hold to the scope rule: only defects this branch introduces.)',
    '',
    'YOUR DOMAIN (do not stray outside it): ' + r.focus,
    '',
    'RECALL IS THE ASSET. Report EVERY defect you find in your domain - including low-confidence and low-severity - each tagged with severity and confidence. Do NOT self-filter to "only the important ones": the orchestrator applies a severity gate (Medium+ is scheduled, Low is logged), that is not your job. Trimming your list is the one thing you must not do. Pure style/formatting is the only safe drop.',
    '',
    'Tag severity by overall engineering impact at scale (millions of customers) - correctness, security, production blast radius, AND maintainability/structure - not by how "real" the bug is: Critical = data loss / breach / outage / billing error / compliance now; High = one of those under realistic load or a latent-at-scale detonator (an N+1 fine at 100 rows, fatal at 10M), OR a structural defect that actively causes bugs or blocks change across the codebase; Medium = a real defect with limited blast radius, OR a maintainability/structure problem that measurably raises the cost or risk of change (god function, layering violation, drifting duplication, misleading naming, dead code, needless complexity with a demonstrably simpler equivalent, a missing test on a real path); Low = a cosmetic nit or opportunistic cleanup with no correctness or real maintainability cost.',
    '',
    'Return findings via the structured schema.',
  ].join('\n')
}

function fidelityPrompt() {
  return [
    'You are the plan-fidelity-reviewer. Reconcile an approved implementation plan against the branch diff that was supposed to implement it. You are a NON-JUDGMENTAL diff narrator: state what differs, NEVER whether it is good or bad. Edit NOTHING.',
    '',
    'Source plan (read in full): `' + planPath + '`',
    'Diff under review: `git diff ' + RANGE + '`. Changed files:',
    changedFiles.map(function (f) { return '  - ' + f }).join('\n'),
    ROOT_NOTE,
    'Use Read/Grep/Glob and read-only git' + (reviewRoot ? ' (`git -C ' + reviewRoot + ' ...`)' : '') + ' to compare the plan against what actually shipped.',
    '',
    'Sort every observation into exactly one of four buckets:',
    '  1. implemented_as_planned - plan items the diff implements substantially as written.',
    '  2. deviations - plan items implemented DIFFERENTLY than written (say plan-said vs. shipped).',
    '  3. additions_not_in_plan - changes in the diff the plan did not call for.',
    '  4. planned_but_not_implemented - plan items with NO covering change in the diff. Surface any planned item or success criterion that never landed; this is the bucket that catches unshipped work hiding behind a green branch.',
    '',
    'This is an audit output, not a gate input: do not rank, score, or recommend. Return the four buckets via the structured schema.',
  ].join('\n')
}

const GATE_SCHEMA = {
  type: 'object',
  required: ['verdicts'],
  properties: {
    verdicts: {
      type: 'array',
      items: {
        type: 'object',
        required: ['key', 'severity', 'category', 'reason'],
        properties: {
          key:      { type: 'string', description: 'the finding key you were given' },
          severity: { enum: ['Critical', 'High', 'Medium', 'Low'], description: 'normalized severity at target scale - OVERRIDES the reviewer tag; the orchestrator schedules Medium+ and appendixes Low' },
          category: { enum: ['data-loss', 'security', 'outage', 'billing', 'compliance', 'correctness', 'maintainability', 'test', 'docs', 'none'] },
          reason:   { type: 'string', description: 'one line: the impact at target scale that justifies this severity' },
        },
      },
    },
  },
}

function gatePrompt(findings) {
  return [
    'You are the SEVERITY GATE for a set of code-review findings. Assign each finding a NORMALIZED severity, sizing its impact at the scale the system is BUILT for (millions of customers, years of data), not today\'s dataset. Your severity OVERRIDES the reviewer\'s tag - the tier is an input signal, not the verdict. The orchestrator then SCHEDULES every finding you rate Medium or above and appendixes the Low ones, so your calibration decides what gets fixed.',
    '',
    'Severity rubric (judge from the Problem text, at target scale):',
    '- Critical: data loss / silent corruption, a security breach (auth bypass, IDOR/cross-tenant, injection, secret leak, privesc), an outage, money/billing incorrectness, or a compliance/privacy violation - triggerable now.',
    '- High: one of the above under realistic load or a motivated attacker, or a latent-at-scale detonator (N+1/unbounded query, missing index on a hot path, unbounded memory/cache growth, O(n^2) hot path, deadlock, reachable crash on a live path); OR a structural defect that actively causes bugs or blocks change across the codebase.',
    '- Medium: a real defect with limited blast radius, OR a maintainability/structure problem that measurably raises the cost or risk of change - a god function/class, a layering or boundary violation, duplication that will drift, misleading or incorrect naming, dead code that hides intent, needless complexity with a demonstrably simpler equivalent, a missing test on a real path.',
    '- Low: a cosmetic nit or opportunistic cleanup with no correctness, blast-radius, or real maintainability cost (pure formatting, an obvious one-off magic number, a trivially reworded comment).',
    '',
    'FLOOR RULE (never demote a real breaker): if a finding can plausibly cause data loss, a security breach, an outage under load, a billing error, or a compliance violation, rate it at LEAST High - resolve any scale/triggerability doubt UPWARD for those five categories. Latent-at-scale defects are High, not Low: "no incident yet" is exactly when they fire.',
    '',
    'Do NOT inflate the other direction: pure style, formatting, and cosmetic-only items are Low. A maintainability finding that is genuinely trivial (no drift risk, no confusion, no coupling) is Low.',
    '',
    'SIMPLIFICATION CAP: a finding from simplification-reviewer is a rewrite of working code. Rate it Medium at most (category maintainability), never High or Critical.',
    '',
    'Findings (judge each by its `key`):',
    findings.map(function (f) {
      return '- key=' + f.key + ' [' + f.source + ' said ' + f.severity + '/' + f.confidence + '] ' + f.category + ' @ ' + f.file + '\n    problem: ' + f.problem
    }).join('\n'),
    '',
    'Return one verdict per key (normalized severity + category + reason) via the schema.',
  ].join('\n')
}

const VERDICT_SCHEMA = {
  type: 'object',
  required: ['refuted', 'reasoning'],
  properties: {
    refuted:   { type: 'boolean', description: 'true if you could NOT confirm this is a real, triggerable defect' },
    reasoning: { type: 'string', description: 'the specific code evidence for your verdict' },
  },
}

function verifyPrompt(f, lens) {
  return [
    'You are an adversarial verifier. Your job is to REFUTE the finding below, not to agree with it. Read the cited file and surrounding code with Read/Grep and decide whether this is a REAL problem at the claimed severity - a triggerable defect, or a genuine maintainability/structure issue - or a false positive (guarded elsewhere, unreachable, misread, already handled or refactored away, or a trivial non-issue).',
    'Default to refuted=true when the evidence is not clear-cut. A finding survives only on concrete code evidence that it genuinely holds at the claimed severity.',
    'Apply this lens specifically: ' + lens + '.',
    ROOT_NOTE,
    '',
    'Finding [' + (f.effective_severity || f.severity) + '] ' + (f.gate_category || f.category) + ' @ ' + f.file,
    'Problem: ' + f.problem,
    'Gate reason: ' + (f.gate_reason || 'n/a'),
    f.code ? ('Cited code:\n' + f.code) : '',
    f.source === 'simplification-reviewer' && f.fix ? ('Proposed simpler version:\n' + f.fix) : '',
    '',
    'Return refuted + reasoning via the schema.',
  ].join('\n')
}

// Two adversarial lenses for the Critical tier. Lens 1 folds reachability + scale (the two
// cheapest ways a gate false-positive dies); lens 2 covers "already handled". A Critical
// candidate is killed only if BOTH refute, so recall of real breakers is preserved - see the
// survives calc below.
const LENSES = [
  'does the cited code path actually reach this with attacker/edge input, and does it genuinely detonate at target scale rather than only in theory',
  'is it already guarded, validated, or handled nearby',
]
// Single lens for High and Medium: a general validity check. One skeptic asks only whether the
// problem is real at the claimed severity or a false positive.
// Simplification findings propose rewriting working code, so their skeptic checks the rewrite
// itself: a simpler version that changes behavior for any caller is refuted.
const SIMPLIFY_LENS = 'grep every caller and implementation of the cited code, then confirm the proposed simpler version does the same job for every one of them, and that no stated reason (a second implementation, a repo convention, a test seam, a public API, a documented constraint) earns the extra structure. Refute if the rewrite changes behavior for any caller, if a reason for the complexity exists, or if the simpler version is not actually simpler'
const MEDIUM_LENS = 'read the cited code and confirm this is a real problem at the claimed severity, not a false positive - misread, already handled or refactored away, or a trivial non-issue dressed up as a defect'

// ---- the fix plan, assembled in code --------------------------------------
// The plan has the shape of the 05-create-plan template: frontmatter, Overview, Current State
// Analysis, Desired End State, What We're NOT Doing, Implementation Approach, then one
// `## Phase 1:` section with Overview, Changes Required (one `#### 1.N` block per finding, with a
// diff), and Success Criteria (automated and manual checkboxes). 06-implement-plan runs it as is.
// The appendix and plan-fidelity sections carry no `## Phase` heading, so phase readers skip them.

const LANG_BY_EXT = { js: 'javascript', jsx: 'jsx', ts: 'typescript', tsx: 'tsx', py: 'python', go: 'go', rb: 'ruby', rs: 'rust', java: 'java', kt: 'kotlin', sh: 'bash', sql: 'sql', md: 'markdown', yml: 'yaml', yaml: 'yaml', json: 'json', css: 'css', html: 'html', swift: 'swift', php: 'php', cs: 'csharp' }

function fence(text, lang) {
  const body = String(text).replace(/\s+$/, '')
  const ticks = /```/.test(body) ? '~~~~' : '```'
  return ticks + (lang || '') + '\n' + body + '\n' + ticks
}

function fixDiffBlock(before, after) {
  const mark = function (text, sign) {
    return String(text).replace(/\s+$/, '').split('\n').map(function (l) { return sign + l }).join('\n')
  }
  const parts = []
  if (before && String(before).trim()) parts.push(mark(before, '-'))
  if (after && String(after).trim()) parts.push(mark(after, '+'))
  return fence(parts.join('\n'), 'diff')
}

function sevSummary(counts) {
  return ['Critical', 'High', 'Medium'].filter(function (s) { return counts[s] }).map(function (s) { return counts[s] + ' ' + s }).join(', ')
}

function findingBlock(f, i) {
  const sev = f.effective_severity || f.severity
  const cat = f.gate_category || f.category
  const fix = String(f.fix || '').trim()
  const multiline = fix.indexOf('\n') !== -1
  const out = [
    '#### 1.' + (i + 1) + ' ' + f.title,
    '',
    '**File**: `' + f.file + '`',
    '**Changes**: ' + (fix && !multiline ? fix : 'Replace the code as shown in the diff below.'),
    '**Severity**: ' + sev + ' (Confidence: ' + (f.confidence || 'MED') + ') · **Source**: `' + f.source + '` · **Category**: ' + cat + (f.category && f.category !== cat ? ' (' + f.category + ')' : ''),
    '**Problem**: ' + String(f.problem || '').trim(),
  ]
  const hasBefore = f.code && String(f.code).trim()
  if (hasBefore || multiline) out.push('', fixDiffBlock(hasBefore ? f.code : '', multiline ? fix : (hasBefore ? fix : '')))
  out.push('')
  return out.join('\n')
}

function renderPlan(confirmed, nonBlocking, planFidelity) {
  const ordered = confirmed.slice().sort(function (a, b) {
    const d = SEV_RANK[a.effective_severity || a.severity] - SEV_RANK[b.effective_severity || b.severity]
    return d || String(a.file).localeCompare(String(b.file))
  })
  const n = ordered.length
  const m = nonBlocking.length
  const counts = severityCounts(ordered)
  const name = featureName || branch
  const files = changedFiles.length ? changedFiles.length + (changedFiles.length === 1 ? ' file' : ' files') : ''
  const out = []
  out.push('---')
  if (taskName) out.push('task: ' + taskName)
  out.push('type: plan')
  if (repoName) out.push('repo: ' + repoName)
  out.push('branch: ' + branch)
  if (headSha) out.push('sha: ' + headSha)
  out.push('---', '')
  out.push('# ' + name + ' Code Review Fix Plan', '')
  out.push('## Overview', '')
  out.push('The specialist reviewers surfaced ' + (n + m) + ' distinct findings on this branch. '
    + n + ' were rated **Medium or above** by the severity gate (correctness, security, production blast radius, or maintainability/structure impact) and are scheduled below, highest severity first.'
    + (m ? ' The remaining ' + m + ' are Low-severity and listed in the appendix for the record, not for action.' : ''), '')
  if (planPath) out.push('- **Source plan**: `' + planPath + '`')
  out.push('- **Branch**: `' + branch + '`')
  if (prLabel) out.push('- **Pull request**: ' + prLabel)
  out.push('- **Diff base**: `' + diffBase + '`' + (files ? ' (' + files + ' changed)' : ''))
  if (planDate) out.push('- **Date**: ' + planDate)
  out.push('- **Scheduled findings**: ' + n + ' (' + sevSummary(counts) + ') · **Low-severity observations**: ' + m + ' (not scheduled)')
  out.push('- **Reviewers**: ' + (FULL_ROSTER ? '13 full' : '8 core') + ' specialist reviewers' + (BRAND_REVIEW ? ' + brand' : ''), '')
  out.push('## Current State Analysis', '')
  out.push('The branch is built and its diff against `' + diffBase + '` has been audited. The scheduled findings are the places where it falls short of ship-ready.', '')
  out.push('### Key Discoveries:')
  const top = ordered.filter(function (f) { return SEV_RANK[f.effective_severity || f.severity] <= SEV_RANK.High })
  if (top.length) top.forEach(function (f) { out.push('- ' + (f.effective_severity || f.severity) + ': ' + f.title + ' (`' + f.file + '`)') })
  else out.push('- No Critical or High findings. Every scheduled finding is Medium.')
  out.push('')
  out.push('## Desired End State', '')
  out.push('All ' + n + ' scheduled findings are fixed. The automated checks below pass. Re-running the affected reviewers on the touched files reports none of them. The feature this branch added still works: ' + (featureCheck || 'its main path runs end-to-end against the local dev environment.'), '')
  out.push('## What We\'re NOT Doing', '')
  out.push('- Reopening design decisions from the design discussion (those are locked).')
  out.push('- Restructuring the feature beyond what a scheduled finding requires; a structural fix stays scoped to the finding, not a redesign that needs design sign-off.')
  if (m) out.push('- Acting on the low-severity appendix - those are recorded, not scheduled.')
  out.push('')
  out.push('## Implementation Approach', '')
  out.push('One phase. Fix the findings in the order listed: Critical, then High, then Medium. Each fix is independent of the others and stays inside the file and behavior the finding names.', '')
  out.push('---', '')
  out.push('## Phase 1: Findings to fix (Medium and above)', '')
  out.push('### Overview', '')
  out.push('Fixes the ' + n + ' finding(s) the severity gate rated Medium or above - production blast-radius defects and the maintainability/structure problems that raise the cost or risk of change.', '')
  out.push('### Changes Required:', '')
  ordered.forEach(function (f, i) { out.push(findingBlock(f, i)) })
  out.push('### Success Criteria:', '')
  out.push('#### Automated Verification:')
  if (checkCommands.length) checkCommands.forEach(function (c) { out.push('- [ ] `' + String(c).replace(/^`|`$/g, '') + '` passes') })
  else out.push('- [ ] The project\'s unit test, type-check, and lint commands pass (the ones its CLAUDE.md documents)')
  out.push('', '#### Manual Verification:')
  out.push('- [ ] Re-run the affected reviewer(s) on the touched files and confirm those specific findings are gone.')
  out.push('- [ ] Each blast-radius finding exercised end-to-end and now blocked; each structural finding confirmed corrected.')
  out.push('- [ ] The feature this branch added still works end-to-end: ' + (featureCheck || 'exercise its main path against the local dev environment.'))
  out.push('', '**Implementation Note**: After completing this phase and all automated verification passes, pause here for manual confirmation from the human that the manual testing was successful before proceeding.', '')
  out.push('---', '')
  if (m) {
    out.push('## Low-severity observations (not scheduled)', '')
    nonBlocking.forEach(function (f) { out.push('- `' + (f.gate_category || f.category || 'none') + '` - ' + f.title + ' (`' + f.file + '`)') })
    out.push('')
  }
  if (planFidelity) {
    const bucket = function (title, items) {
      out.push('### ' + title)
      ;(items && items.length ? items : ['None']).forEach(function (x) { out.push('- ' + x) })
      out.push('')
    }
    out.push('## Plan fidelity (reconciliation, not scheduled)', '')
    bucket('Implemented as planned', planFidelity.implemented_as_planned)
    bucket('Deviations / surprises', planFidelity.deviations)
    bucket('Additions not in plan', planFidelity.additions_not_in_plan)
    bucket('Planned but not implemented', planFidelity.planned_but_not_implemented)
  }
  out.push('## References', '')
  const refs = references.length ? references : (planPath ? ['Source plan: `' + planPath + '`'] : ['(no upstream artifacts were supplied)'])
  refs.forEach(function (r) { out.push('- ' + String(r).replace(/^-\s*/, '')) })
  return out.join('\n') + '\n'
}

phase('Recall')
log('Auditing the ' + RANGE + (prLabel ? ' diff of PR ' + prLabel : ' diff') + ' with ' + REVIEWERS.length + ' specialists (' + (FULL_ROSTER ? 'full roster' : 'core roster') + ') + plan-fidelity (read-only)...')

const recallThunks = REVIEWERS.map(function (r) {
  return function () {
    return agent(reviewerPrompt(r), {
      schema: FINDING_SCHEMA, phase: 'Recall', label: 'review:' + r.key,
      agentType: r.key,           // use the registered specialist subagent type
      model: A.model || r.tier,   // per-specialist tier; args.model forces one tier for all
      effort: r.effort,

    }).then(function (res) {
      return { source: r.key, findings: (res && res.findings) || [] }
    }).catch(function () { return { source: r.key, findings: [] } })
  }
})

// plan-fidelity is orthogonal to the specialist findings: its result feeds ONLY the plan, never the
// gate or verify. Launch it NOW so it runs concurrently with recall + gate + verify, but keep it OUT of
// the recall barrier - otherwise a slow plan/diff read holds up the gate for no reason. Same agent, same
// prompt, same concurrency budget as before; only the wait is removed. Collected just before Write.
const fidelityPromise = !planPath
  ? Promise.resolve(null)
  : agent(fidelityPrompt(), { schema: FIDELITY_SCHEMA, phase: 'Recall', label: 'review:plan-fidelity', agentType: 'plan-fidelity-reviewer', model: cheapModel, effort: cheapEffort })
      .catch(function () { return null })

const recall = (await parallel(recallThunks)).filter(Boolean)

// ---- merge, dedupe ---------------------------------------------------------

const SEV_RANK = { Critical: 0, High: 1, Medium: 2, Low: 3 }

function normLine(file) { return (file || '').toLowerCase().trim() }
function dedupeKey(f) { return normLine(f.file) + '::' + (f.title || '').slice(0, 40).toLowerCase().replace(/\s+/g, ' ').trim() }

// Pool the specialist findings. Copy every incoming finding (Object.assign into a fresh object)
// rather than mutating in place - agent() results (and args) may be returned read-only/frozen by
// the harness.
const pooled = []
// A reviewer reading a temporary worktree may report absolute paths inside it. Make them relative to
// the reviewed root: the worktree is removed after the run, and the plan must cite repo paths.
function relToRoot(file) {
  const s = String(file || '').trim()
  return reviewRoot && s.startsWith(reviewRoot + '/') ? s.slice(reviewRoot.length).replace(/^\/+/, '') : s
}
recall.forEach(function (r) {
  (r.findings || []).forEach(function (f) { pooled.push(Object.assign({}, f, { source: r.source, file: relToRoot(f.file) })) })
})

// Scope enforcement, in script rather than by prompt. A reviewer that wandered outside the changed
// files produces findings nobody asked for, and the prompt rule alone is not a guarantee. Drop any
// finding whose path is not one of the changed files. Paths are compared by suffix, because a
// reviewer may report `./src/a.ts:12`, an absolute path, or the repo-relative one; the `file` field
// carries a trailing `:LINE` that is stripped first. Skipped when no changed-file list was supplied
// (nothing to check against), so a caller that omits it loses the filter, not the review.
function pathOf(file) {
  return String(file || '').trim().replace(/:\d+(:\d+)?$/, '').replace(/^\.\//, '')
}
function inScope(file) {
  if (!changedFiles.length) return true
  const f = pathOf(file)
  if (!f) return false
  return changedFiles.some(function (c) {
    const cc = pathOf(c)
    return f === cc || f.endsWith('/' + cc) || cc.endsWith('/' + f)
  })
}
const outOfScope = pooled.filter(function (f) { return !inScope(f.file) })
const inScopeFindings = pooled.filter(function (f) { return inScope(f.file) })
if (outOfScope.length) {
  log('Dropped ' + outOfScope.length + ' finding(s) outside the changed files: ' +
      outOfScope.map(function (f) { return pathOf(f.file) + ' (' + f.source + ')' }).join(', '))
}

// Two specialists on the same spot: keep the higher severity.
const byKey = {}
inScopeFindings.forEach(function (f) {
  const k = dedupeKey(f); const cur = byKey[k]
  if (!cur) { byKey[k] = f; return }
  byKey[k] = (SEV_RANK[f.severity] < SEV_RANK[cur.severity]) ? f : cur
})
const merged = Object.keys(byKey).map(function (k) { return byKey[k] })

// ---- helpers -------------------------------------------------------------

async function withRetry(fn, tries) {
  for (let t = 0; t < tries; t++) {
    try {
      const r = await fn()
      if (r) return r
    } catch (e) {
      // transient - fall through
    }
  }
  return null
}

// ---- severity gate (Medium+ schedules, Low appendixes) -------------------

merged.forEach(function (f, i) { f.key = 'F' + i })

// The gate runs in batches so it is not one long single-agent step: each batch is rated by its own
// gate agent, in parallel, and its scheduled findings go straight on to verify while other batches
// are still being rated. The rubric is absolute (impact at target scale), so a batch needs no view
// of the other findings to calibrate.
const GATE_BATCH = 8
const gateBatches = []
for (let i = 0; i < merged.length; i += GATE_BATCH) gateBatches.push(merged.slice(i, i + GATE_BATCH))

// Schedule everything the gate rates Medium or above; Low (and any finding the gate somehow
// dropped, which falls back to the reviewer tag) collapses to the low-severity appendix. The
// gate's normalized severity OVERRIDES the reviewer tag.
const SCHEDULE_AT_OR_ABOVE = SEV_RANK.Medium   // Critical(0) High(1) Medium(2) schedule; Low(3) does not
const scheduled = []
const nonBlocking = []
function applyGate(batch, gate) {
  const verdictByKey = {}
  ;((gate && gate.verdicts) || []).forEach(function (v) { verdictByKey[v.key] = v })
  const batchScheduled = []
  batch.forEach(function (f) {
    const v = verdictByKey[f.key]
    if (v) { f.gate_category = v.category; f.gate_reason = v.reason }
    let sev = (v && v.severity) || f.severity   // fall back to the reviewer tag if the gate dropped this key
    // Simplifications rewrite working code, so they never outrank a real defect. Enforced here, not
    // only in the gate prompt, so a gate that ignores the cap cannot promote one.
    if (f.source === 'simplification-reviewer' && SEV_RANK[sev] < SEV_RANK.Medium) sev = 'Medium'
    f.effective_severity = sev
    if (SEV_RANK[sev] <= SCHEDULE_AT_OR_ABOVE) { scheduled.push(f); batchScheduled.push(f) }
    else { nonBlocking.push(f) }
  })
  return batchScheduled
}

// Critical gets two skeptics; High and Medium get one. Critical and High use high effort.
// Medium uses medium effort. Critical and High verification default to Opus 5.5, every other stage to Sonnet 5.5; explicit args override it.
function verifyOne(f) {
  const critical = f.effective_severity === 'Critical'
  const heavy = SEV_RANK[f.effective_severity] <= SEV_RANK.High
  const lenses = critical ? LENSES : [f.source === 'simplification-reviewer' ? SIMPLIFY_LENS : MEDIUM_LENS]
  const killThreshold = lenses.length            // every skeptic in the tier must refute to kill
  return parallel(lenses.map(function (lens) {
    return function () {
      return agent(verifyPrompt(f, lens), { schema: VERDICT_SCHEMA, phase: 'Verify', label: 'verify:' + f.key, model: heavy ? opusModel : cheapModel, effort: heavy ? strongEffort : cheapEffort })
    }
  })).then(function (votes) {
    const refutes = votes.filter(Boolean).filter(function (v) { return v.refuted }).length
    const survives = refutes < killThreshold      // a single confirm keeps it
    return Object.assign({}, f, { survives: survives, refute_votes: refutes, verify_lenses: lenses.length })
  })
}

phase('Gate')
log('Normalizing severity on ' + merged.length + ' merged findings in ' + gateBatches.length + ' batch(es) (Medium+ schedules, Low appendixes); each scheduled finding is verified as soon as its batch is rated: 2 skeptics for Critical, 1 otherwise...')
const verifiedByBatch = await pipeline(gateBatches,
  function (batch, _item, bi) {
    return withRetry(function () {
      return agent(gatePrompt(batch), { schema: GATE_SCHEMA, phase: 'Gate', label: 'gate:' + (bi + 1), model: model, effort: strongEffort })
    }, 2).then(function (gate) { return applyGate(batch, gate) })
  },
  function (batchScheduled) { return parallel(batchScheduled.map(function (f) { return function () { return verifyOne(f) } })) })
const verified = [].concat.apply([], verifiedByBatch.filter(Boolean))
// Batches finish in any order; restore the merge order so the appendix reads the same every run.
const byMergeOrder = function (a, b) { return Number(a.key.slice(1)) - Number(b.key.slice(1)) }
scheduled.sort(byMergeOrder)
nonBlocking.sort(byMergeOrder)
verified.sort(byMergeOrder)

const confirmed = verified.filter(Boolean).filter(function (f) { return f.survives })
const refuted   = verified.filter(Boolean).filter(function (f) { return !f.survives })

// plan-fidelity ran concurrently with recall + gate + verify; collect it now (usually already resolved).
const planFidelity = await fidelityPromise

// ---- assemble the fix plan (only when something survived) ------------------

const doc = confirmed.length > 0 ? renderPlan(confirmed, nonBlocking, planFidelity) : null
if (doc) log('Fix plan assembled (' + confirmed.length + ' confirmed scheduled finding(s), Medium+); the skill saves it to ' + outputPath + '.')

function catCounts(list) {
  const counts = {}
  list.forEach(function (f) {
    const c = f.gate_category || f.category || 'none'
    counts[c] = (counts[c] || 0) + 1
  })
  return counts
}

function severityCounts(list) {
  const counts = { Critical: 0, High: 0, Medium: 0, Low: 0 }
  list.forEach(function (f) {
    const s = f.effective_severity || f.severity
    if (counts[s] === undefined) counts[s] = 0
    counts[s]++
  })
  return counts
}

return {
  plan_path: doc ? outputPath : null,
  doc: doc,
  scheduled: confirmed.map(function (f) { return { title: f.title, file: f.file, severity: f.effective_severity || f.severity, category: f.gate_category || f.category, source: f.source } }),
  scheduled_severity_counts: severityCounts(confirmed),
  scheduled_category_counts: catCounts(confirmed),
  refuted: refuted.map(function (f) { return { title: f.title, file: f.file, severity: f.effective_severity || f.severity, refute_votes: f.refute_votes } }),
  non_blocking: nonBlocking.map(function (f) { return { category: f.gate_category || f.category, title: f.title, file: f.file, severity: f.effective_severity || f.severity } }),
  plan_fidelity: planFidelity,
  engine_breakdown: {
    reviewers: REVIEWERS.length,
    out_of_scope_dropped: outOfScope.length,
    roster: FULL_ROSTER ? 'full' : 'core',
    findings: merged.length,
    gated_in: scheduled.length, confirmed: confirmed.length, refuted: refuted.length,
  },
}
