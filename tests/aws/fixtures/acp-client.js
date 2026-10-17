#!/usr/bin/env node
// Minimal ACP client for tests/aws/test-agents-mcp.sh. It spawns an ACP
// adapter (default claude-agent-acp) over stdio and sends the smallest message
// sequence that makes the adapter start its MCP servers.
// Usage: acp-client.js [--steps init,new] [--wait-ms N] [--cmd CMD]
"use strict";
const { spawn } = require("child_process");
const readline = require("readline");

const args = process.argv.slice(2);
const opt = (flag, dflt) => {
  const i = args.indexOf(flag);
  return i >= 0 ? args[i + 1] : dflt;
};
const steps = opt("--steps", "init,new").split(",");
const waitMs = Number(opt("--wait-ms", "120000"));
const cmd = opt("--cmd", "claude-agent-acp");

// buzz-acp always passes its own stdio MCP server in session/new
// (crates/buzz-acp/src/lib.rs, build_mcp_servers). This stand-in has the same
// shape. It writes nothing to /tmp/probe-started, so the test can still tell
// which servers came from .mcp.json.
const harnessServers = [
  {
    name: "buzz-dev-mcp",
    command: "node",
    args: ["-e", "setInterval(() => {}, 1000)"],
    env: [{ name: "BUZZ_RELAY_URL", value: "wss://buzz.example.com" }],
  },
];

const child = spawn(cmd, [], { stdio: ["pipe", "pipe", "inherit"] });
let nextId = 1;
const pending = new Map();

const send = (msg) => child.stdin.write(JSON.stringify(msg) + "\n");
const request = (method, params) =>
  new Promise((resolve) => {
    const id = nextId++;
    pending.set(id, resolve);
    send({ jsonrpc: "2.0", id, method, params });
  });

readline.createInterface({ input: child.stdout }).on("line", (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch (e) {
    return;
  }
  if (msg.id !== undefined && msg.method === undefined) {
    const done = pending.get(msg.id);
    if (done) {
      pending.delete(msg.id);
      done(msg);
    }
  } else if (msg.id !== undefined && msg.method) {
    // Agent-to-client request (permissions, file access): refuse politely.
    if (msg.method === "session/request_permission") {
      send({ jsonrpc: "2.0", id: msg.id, result: { outcome: { outcome: "cancelled" } } });
    } else {
      send({ jsonrpc: "2.0", id: msg.id, error: { code: -32601, message: "unsupported" } });
    }
  }
});

const within = (p, ms) =>
  Promise.race([p, new Promise((r) => setTimeout(() => r({ timeout: true }), ms))]);

(async () => {
  let sessionId;
  if (steps.includes("init")) {
    const r = await within(
      request("initialize", { protocolVersion: 1, clientCapabilities: {} }),
      waitMs,
    );
    console.error("initialize:", JSON.stringify(r).slice(0, 300));
  }
  if (steps.includes("new")) {
    const r = await within(
      request("session/new", { cwd: process.cwd(), mcpServers: harnessServers }),
      waitMs,
    );
    console.error("session/new:", JSON.stringify(r).slice(0, 300));
    sessionId = r.result && r.result.sessionId;
  }
  if (steps.includes("prompt") && sessionId) {
    const r = await within(
      request("session/prompt", {
        sessionId,
        prompt: [{ type: "text", text: "hello" }],
      }),
      waitMs,
    );
    console.error("session/prompt:", JSON.stringify(r).slice(0, 300));
  }
  // Give MCP servers time to start after the last reply.
  await new Promise((r) => setTimeout(r, 5000));
  child.kill("SIGTERM");
  setTimeout(() => process.exit(0), 500);
})();
