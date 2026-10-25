#!/bin/sh
# One scripted MCP client session on stdout: initialize, initialized,
# tools/list, tools/call screenshot. Pipe it into the desk's socket (socat)
# or its --mcp-stdio.
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"panoptes-e2e","version":"0"}}}'
sleep 1
printf '%s\n' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
printf '%s\n' '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
printf '%s\n' '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"screenshot","arguments":{"scale":0.5}}}'
sleep "${MCP_LINGER:-3}"
