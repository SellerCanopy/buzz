#!/usr/bin/env python3
"""Save the document a research workflow returned, without the agent retyping it.

Workflow scripts cannot touch disk, so the research workflows assemble their document
as a string and return it as `result.doc`. The harness stores every run's return value
in its run record at ~/.claude/projects/<project>/<session>/workflows/<runId>.json. This
copies that string to the output path, so the calling skill never re-emits the document
as tool input.

The run-record location is observed harness behavior, not a documented interface. On
any nonzero exit the calling skill falls back to writing `result.doc` with the Write tool.

Usage:
  save-workflow-doc.py <runId> <outputPath>

Prints one line (the path and byte count) on success. Exit codes:
  0 saved, 2 bad input, 5 run record not found, 6 record has no result.doc yet.
"""
import glob
import json
import os
import sys


def find_record(run_id):
    """Locate the run record for a workflow run.

    Args:
        run_id: The workflow run ID, e.g. "wf_0c7541fa-ac1".

    Returns:
        The newest matching record path, or None when no record exists.
    """
    pattern = os.path.expanduser(f'~/.claude/projects/*/*/workflows/{run_id}.json')
    matches = sorted(glob.glob(pattern), key=os.path.getmtime)
    return matches[-1] if matches else None


def main(argv):
    """Copy result.doc from a run record to the output path.

    Args:
        argv: Command-line arguments: the run ID and the absolute output path.

    Returns:
        The process exit code.
    """
    if len(argv) != 3 or not argv[1].startswith('wf_') or not os.path.isabs(argv[2]):
        print('usage: save-workflow-doc.py <runId wf_...> <absolute outputPath>', file=sys.stderr)
        return 2
    run_id, out_path = argv[1], argv[2]
    record = find_record(run_id)
    if not record:
        print(f'no run record for {run_id}', file=sys.stderr)
        return 5
    with open(record, encoding='utf-8') as fh:
        result = json.load(fh).get('result')
    if isinstance(result, str):
        try:
            result = json.loads(result)
        except ValueError:
            result = None
    doc = result.get('doc') if isinstance(result, dict) else None
    if not isinstance(doc, str) or not doc.strip():
        print(f'run record {record} has no result.doc', file=sys.stderr)
        return 6
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, 'w', encoding='utf-8') as fh:
        fh.write(doc)
    print(f'saved {out_path} ({len(doc.encode("utf-8"))} bytes)')
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
