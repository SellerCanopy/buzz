#!/bin/sh
# A stand-in for the Claude CLI, for the supervisor's sign-in tests. Never touches the network.
#
#   claude-stub.sh <agent> mcp login <server> --no-browser
#   claude-stub.sh <agent> mcp list
#
# State lives in $STUB_STATE (set by exec-stub.sh):
#   <agent>/<server>.connected   exists once a sign-in was accepted
#   mode                         normal (default), reject, hang, silent or list-failed
#   login.pid                    the pid of the last login process
#   last-paste                   the line the last login process read
#   calls.log                    one line per invocation
set -u
state=${STUB_STATE:?}
agent=$1
shift
mode=normal
[ -f "$state/mode" ] && mode=$(cat "$state/mode")
echo "$agent $*" >> "$state/calls.log"

if [ "${1:-}" = mcp ] && [ "${2:-}" = login ]; then
  server=$3
  [ "${4:-}" = --no-browser ] || exit 2
  echo $$ > "$state/login.pid"
  # Prints no link at all.
  [ "$mode" = silent ] && exec sleep 30
  echo "Not opening a browser. Open this link to sign in to $server:"
  echo "  https://auth.example.org/oauth/authorize?client_id=stub&state=STUBSTATE&redirect_uri=http%3A%2F%2Flocalhost%3A45219%2Fcallback"
  echo "Paste the address the browser ends on:"
  IFS= read -r line || exit 1
  printf '%s\n' "$line" > "$state/last-paste"
  # A real CLI may echo what it was given. The supervisor must never pass that on.
  echo "received: $line"
  echo "received: $line" >&2
  case "$mode" in
    hang) exec sleep 30 ;;
    reject) exit 1 ;;
  esac
  case "$line" in
    http://localhost:45219/callback\?code=*state=STUBSTATE*)
      mkdir -p "$state/$agent"
      : > "$state/$agent/$server.connected"
      echo "Signed in to $server"
      exit 0 ;;
    *) exit 1 ;;
  esac
fi

if [ "${1:-}" = mcp ] && [ "${2:-}" = list ]; then
  echo "Checking MCP server health..."
  echo
  echo "filesystem: npx -y @modelcontextprotocol/server-filesystem@1.0.0 /home/agent - ✓ Connected"
  if [ "$mode" = list-failed ]; then
    echo "docs: https://mcp.example.org/mcp (HTTP) - ✗ Failed to connect"
  elif [ -f "$state/$agent/docs.connected" ]; then
    echo "docs: https://mcp.example.org/mcp (HTTP) - ✓ Connected"
  else
    echo "docs: https://mcp.example.org/mcp (HTTP) - ⚠ Needs authentication"
  fi
  exit 0
fi

echo "unexpected: $*" >&2
exit 2
