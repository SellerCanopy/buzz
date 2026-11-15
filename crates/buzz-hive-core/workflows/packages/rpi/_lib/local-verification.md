# Verifying manual items on the local stack

A `Manual Verification` item is work for the agent, not the human. Run each one against the local stack, record what you saw, and tick the box only when it passed. Never test in production.

## Who runs what

The implementer agents have no browser tools and cannot spawn agents. The session that launched them does.

- **The implementer agent** runs every item it can from the shell: `curl` the API, run the changed code path, read logs, query the database read-only. It ticks those boxes and lists the rest as browser items.
- **The launching session** runs each browser item with the `chrome-devtools-tester` agent, one behavior per call. It ticks the box on PASS.
- **Claude in Chrome**, if it is connected in the session, is an acceptable substitute for `chrome-devtools-tester`.

## Steps

1. **Start the stack** the way the project documents it. Rebuild if the change needs it. Check that the app answers (`curl -sS -o /dev/null -w '%{http_code}' <url>`) before testing.
2. **Run the item.**
   - UI behavior: hand `chrome-devtools-tester` the URL, the steps and the success criteria.
   - API behavior: `curl` the endpoint and compare the response to the expected one.
   - Runtime bug or changed code path: run the path directly (a script, a CLI call, a focused test, or a REPL call) with the inputs that triggered the bug. Add temporary logging if the cause is not visible, rerun, read the output, then remove the logging.
   - Data change: query the database read-only.
3. **Record the evidence.** Write one line per item with what was run and what it showed. A failure includes the actual error text.
4. **Tick the box** only on a pass. On a failure, fix the cause and rerun the item. Do not ask the human whether it worked.

## When a check is blocked

If the stack will not start, a login recipe is missing, or a command is denied, fix the blocker. If you cannot, report the blocker and what would unblock it. Do not hand the check to the human.

Only these go to the human, each named in the report with the reason:

- A third-party UI the agent cannot reach.
- A second user account.
- A physical device.
- A credential that is not documented anywhere.
