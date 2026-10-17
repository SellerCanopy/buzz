#!/usr/bin/env node
// Minimal stdio MCP server for tests/aws/test-agents-mcp.sh.
// Speaks newline-delimited JSON-RPC 2.0. Usage: probe-mcp.js <name>.
// On start it appends <name> to /tmp/probe-started, so a test can see which
// probe servers an agent actually launched. It answers initialize and
// tools/list (one tool named <name>) and ignores notifications.
"use strict";
const fs = require("fs");
const readline = require("readline");

const name = process.argv[2];
if (!name) {
  console.error("usage: probe-mcp.js <name>");
  process.exit(2);
}
fs.appendFileSync("/tmp/probe-started", name + "\n");

const send = (msg) => process.stdout.write(JSON.stringify(msg) + "\n");

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (line) => {
  if (!line.trim()) return;
  let msg;
  try {
    msg = JSON.parse(line);
  } catch (e) {
    return;
  }
  if (msg.id === undefined || msg.id === null) return;
  if (msg.method === "initialize") {
    const version = (msg.params && msg.params.protocolVersion) || "2024-11-05";
    send({
      jsonrpc: "2.0",
      id: msg.id,
      result: {
        protocolVersion: version,
        capabilities: { tools: {} },
        serverInfo: { name: "probe-" + name, version: "0.0.1" },
      },
    });
  } else if (msg.method === "tools/list") {
    send({
      jsonrpc: "2.0",
      id: msg.id,
      result: {
        tools: [
          {
            name: name,
            description: "Probe tool " + name,
            inputSchema: { type: "object", properties: {} },
          },
        ],
      },
    });
  } else {
    send({
      jsonrpc: "2.0",
      id: msg.id,
      error: { code: -32601, message: "method not found: " + msg.method },
    });
  }
});
rl.on("close", () => process.exit(0));
