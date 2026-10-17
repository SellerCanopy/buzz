#!/usr/bin/env python3
"""Reads and checks a cloud agent's spec file, deploy/aws/agents/<name>/agent.json.

The FIELDS table below is the only place that lists the settings an agent can
have. Validation, the container environment, and the docs table all come from it.

Usage:
    agent_spec.py check <agent.json>
    agent_spec.py get <agent.json> <field> [--default VALUE]
    agent_spec.py env <agent.json> [--default FIELD=VALUE ...]
    agent_spec.py fields

A missing file is an empty spec. Errors go to stderr and exit 1.
"""

import argparse
import json
import os
import re
import sys

HEX64 = re.compile(r"[0-9a-f]{64}")
TOKEN = re.compile(r"[A-Za-z0-9._:/-]+")
FILENAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}")
MAX_INSTRUCTIONS_BYTES = 65536


class Field:
    """One setting an agent can have.

    Attributes:
        name: Key in agent.json.
        kind: One of text, token, enum, int, bool, hexlist, file.
        applied: Where the value takes effect: profile (make agents-attest),
            runtime (make deploy), or both.
        env: Container variable the value is written to, for runtime fields.
        doc: One-line description for the docs table.
        choices: Allowed values of an enum.
        limit: Maximum length of text, or the (low, high) range of an int.
    """

    def __init__(self, name, kind, applied, doc, env=None, choices=None, limit=None):
        self.name = name
        self.kind = kind
        self.applied = applied
        self.doc = doc
        self.env = env
        self.choices = choices
        self.limit = limit


FIELDS = [
    Field("display_name", "text", "both", "Name people see. Defaults to the AGENTS name.", limit=64),
    Field("about", "text", "profile", "Profile text.", limit=300),
    Field("model", "token", "runtime", "Model for every session: default, sonnet, fable, opus, haiku.",
          env="BUZZ_ACP_MODEL", limit=100),
    Field("effort", "token", "runtime", "How hard the model thinks: low, medium, high.",
          env="BUZZ_ACP_EFFORT_LEVEL", limit=16),
    Field("permission_mode", "enum", "runtime", "Tool permission mode. Default bypass-permissions.",
          env="BUZZ_ACP_PERMISSION_MODE",
          choices=["default", "auto", "accept-edits", "bypass-permissions", "dont-ask", "plan"]),
    Field("respond_to", "enum", "both", "Whose messages it answers. Defaults to AGENT_RESPOND_TO.",
          env="BUZZ_ACP_RESPOND_TO", choices=["owner-only", "allowlist", "anyone", "nobody"]),
    Field("respond_to_allowlist", "hexlist", "both",
          "Public keys it answers when respond_to is allowlist (the owner is always included).",
          env="BUZZ_ACP_RESPOND_TO_ALLOWLIST"),
    Field("parallelism", "int", "both", "Conversations it works on at once.",
          env="BUZZ_ACP_AGENTS", limit=(1, 32)),
    Field("session_policy", "enum", "runtime", "One session per channel or per thread.",
          env="BUZZ_ACP_SESSION_POLICY", choices=["channel", "thread"]),
    Field("multiple_event_handling", "enum", "runtime", "What a new mention does while it is mid-turn.",
          env="BUZZ_ACP_MULTIPLE_EVENT_HANDLING", choices=["queue", "steer", "interrupt", "owner-interrupt"]),
    Field("context_message_limit", "int", "runtime", "Earlier messages it reads for a thread or DM reply.",
          env="BUZZ_ACP_CONTEXT_MESSAGE_LIMIT", limit=(0, 200)),
    Field("max_turns_per_session", "int", "runtime", "Turns before a session restarts. 0 never restarts.",
          env="BUZZ_ACP_MAX_TURNS_PER_SESSION", limit=(0, 100000)),
    Field("idle_timeout", "int", "runtime", "Seconds of silence before a turn is stopped.",
          env="BUZZ_ACP_IDLE_TIMEOUT", limit=(1, 86400)),
    Field("max_turn_duration", "int", "runtime", "Seconds one turn may last at most.",
          env="BUZZ_ACP_MAX_TURN_DURATION", limit=(1, 86400)),
    Field("heartbeat_interval", "int", "runtime", "Seconds between self-prompts. 0 turns them off.",
          env="BUZZ_ACP_HEARTBEAT_INTERVAL", limit=(0, 86400)),
    Field("heartbeat_prompt", "text", "runtime", "Text of the self-prompt.",
          env="BUZZ_ACP_HEARTBEAT_PROMPT", limit=500),
    Field("memory", "bool", "runtime", "Whether the agent keeps notes between conversations. Default true."),
    Field("instructions_file", "file", "runtime",
          "File in the agent folder with its standing instructions (up to 64 KB).",
          env="BUZZ_ACP_SYSTEM_PROMPT_FILE"),
]
BY_NAME = {f.name: f for f in FIELDS}

# Where the deploy mounts the agent folder's staged files inside the container.
CONTAINER_CONFIG_DIR = "/etc/buzz-agent"


def fail(message):
    sys.exit("error: " + message)


def check_value(field, value, path):
    """Returns the value as the text that goes in the environment, or exits 1."""
    where = "%s in %s" % (field.name, path)
    kind = field.kind
    if kind == "bool":
        if not isinstance(value, bool):
            fail("%s must be true or false" % where)
        return "true" if value else "false"
    if kind == "int":
        if isinstance(value, bool) or not isinstance(value, int):
            fail("%s must be a whole number" % where)
        low, high = field.limit
        if not low <= value <= high:
            fail("%s must be between %d and %d" % (where, low, high))
        return str(value)
    if kind == "hexlist":
        if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
            fail("%s must be a list of public keys" % where)
        if len(set(value)) != len(value) or not all(HEX64.fullmatch(v) for v in value):
            fail("%s must hold distinct 64-character lowercase hex public keys" % where)
        return ",".join(value)
    if not isinstance(value, str):
        fail("%s must be text" % where)
    if kind == "enum":
        if value not in field.choices:
            fail("%s must be one of %s" % (where, ", ".join(field.choices)))
    elif kind == "token":
        if len(value) > field.limit or not TOKEN.fullmatch(value):
            fail("%s may hold only letters, digits, and . _ : / - (at most %d characters)" % (where, field.limit))
    elif kind == "file":
        if not FILENAME.fullmatch(value) or value in ("mcp.json", "agent.json"):
            fail("%s must be a plain file name in the agent folder, other than mcp.json and agent.json" % where)
    else:
        if len(value) > field.limit:
            fail("%s is longer than %d characters" % (where, field.limit))
        if any(ord(c) < 32 or c in "'\\" for c in value):
            fail("%s has a quote, backslash, or line break, which the deploy cannot pass safely" % where)
    return value


def load(path):
    """Returns the spec as {field name: checked text}. A missing file gives {}."""
    if not os.path.isfile(path):
        return {}
    try:
        with open(path) as handle:
            spec = json.load(handle)
    except ValueError as err:
        fail("%s is not valid JSON: %s" % (path, err))
    if not isinstance(spec, dict):
        fail("%s must be a JSON object" % path)
    out = {}
    for key, value in spec.items():
        if key not in BY_NAME:
            fail("%s has an unknown field \"%s\" (allowed: %s)" % (path, key, ", ".join(BY_NAME)))
        out[key] = check_value(BY_NAME[key], value, path)
    name = out.get("instructions_file")
    if name:
        file_path = os.path.join(os.path.dirname(path), name)
        if not os.path.isfile(file_path):
            fail("instructions_file in %s names %s, which is not in the agent folder" % (path, name))
        if os.path.getsize(file_path) > MAX_INSTRUCTIONS_BYTES:
            fail("%s is larger than %d bytes" % (file_path, MAX_INSTRUCTIONS_BYTES))
    return out


def parse_defaults(pairs):
    defaults = {}
    for pair in pairs or []:
        key, _, value = pair.partition("=")
        if key not in BY_NAME:
            fail("unknown default field: %s" % key)
        defaults[key] = value
    return defaults


def env_lines(spec, defaults):
    """Returns the NAME=value container variable lines for a checked spec."""
    merged = dict(defaults)
    merged.update(spec)
    if merged.get("respond_to") == "allowlist" and not merged.get("respond_to_allowlist"):
        fail("respond_to is allowlist but there is no respond_to_allowlist (set it in agent.json or AGENT_RESPOND_TO_ALLOWLIST)")
    lines = []
    if merged.get("display_name"):
        lines.append("BUZZ_ACP_DISPLAY_NAME=%s" % merged["display_name"])
    for field in FIELDS:
        if field.env is None or merged.get(field.name, "") == "":
            continue
        value = merged[field.name]
        if field.name == "respond_to_allowlist" and merged.get("respond_to") != "allowlist":
            continue
        if field.kind == "file":
            value = "%s/%s" % (CONTAINER_CONFIG_DIR, value)
        lines.append("%s=%s" % (field.env, value))
    if merged.get("memory") == "false":
        lines.append("BUZZ_ACP_NO_MEMORY=true")
    elif merged.get("memory") == "true":
        lines.append("BUZZ_ACP_MEMORY=true")
    return lines


def fields_table():
    rows = ["| Field | Type | Applied by | Meaning |", "|-------|------|------------|---------|"]
    applied = {"profile": "`make agents-attest`", "runtime": "`make deploy`",
               "both": "`make deploy` and `make agents-attest`"}
    for field in FIELDS:
        kind = field.kind
        if field.choices:
            kind = " or ".join("`%s`" % c for c in field.choices)
        elif kind == "int":
            kind = "whole number %d to %d" % field.limit
        elif kind == "hexlist":
            kind = "list of public keys"
        elif kind == "file":
            kind = "file name"
        rows.append("| `%s` | %s | %s | %s |" % (field.name, kind, applied[field.applied], field.doc))
    return "\n".join(rows)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    check = sub.add_parser("check")
    check.add_argument("file")
    get = sub.add_parser("get")
    get.add_argument("file")
    get.add_argument("field")
    get.add_argument("--default", default="")
    env = sub.add_parser("env")
    env.add_argument("file")
    env.add_argument("--default", action="append")
    sub.add_parser("fields")
    args = parser.parse_args()

    if args.command == "fields":
        print(fields_table())
        return
    spec = load(args.file)
    if args.command == "get":
        if args.field not in BY_NAME:
            fail("unknown field: %s" % args.field)
        print(spec.get(args.field, args.default))
    elif args.command == "env":
        for line in env_lines(spec, parse_defaults(args.default)):
            print(line)


if __name__ == "__main__":
    main()
