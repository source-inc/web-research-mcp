#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
compose=(docker compose -f "${repo_dir}/compose.contract.yaml")
endpoint="http://127.0.0.1:${WEB_RESEARCH_MCP_CONTRACT_PORT:-19213}"
tmp_dir="$(mktemp -d)"

cleanup() {
  "${compose[@]}" down --volumes --remove-orphans
  rm -rf "${tmp_dir}"
}
trap cleanup EXIT

"${compose[@]}" up --build --detach
for _ in $(seq 1 60); do
  if curl --fail --silent "${endpoint}/healthz" >"${tmp_dir}/health.json"; then
    break
  fi
  sleep 1
done
curl --fail --silent "${endpoint}/healthz" | grep -q '"status":"ok"'

curl --fail --silent --dump-header "${tmp_dir}/headers" \
  --header 'accept: application/json, text/event-stream' \
  --header 'content-type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"docker-contract","version":"0"}}}' \
  "${endpoint}/mcp" >"${tmp_dir}/initialize"
session_id="$(awk 'BEGIN {IGNORECASE=1} /^mcp-session-id:/ {gsub("\r", "", $2); print $2}' "${tmp_dir}/headers")"
test -n "${session_id}"

curl --fail --silent \
  --header 'accept: application/json, text/event-stream' \
  --header 'content-type: application/json' \
  --header "mcp-session-id: ${session_id}" \
  --data '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  "${endpoint}/mcp" >/dev/null

curl --fail --silent \
  --header 'accept: application/json, text/event-stream' \
  --header 'content-type: application/json' \
  --header "mcp-session-id: ${session_id}" \
  --data '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  "${endpoint}/mcp" >"${tmp_dir}/tools"
grep -q 'web_search' "${tmp_dir}/tools"
grep -q 'web_scrape_url' "${tmp_dir}/tools"
grep -q 'web_verify_quote' "${tmp_dir}/tools"

curl --fail --silent \
  --header 'accept: application/json, text/event-stream' \
  --header 'content-type: application/json' \
  --header "mcp-session-id: ${session_id}" \
  --data '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"web_search","arguments":{"query":"fixture evidence","max_results":2}}}' \
  "${endpoint}/mcp" >"${tmp_dir}/search"
grep -q 'Primary fixture source' "${tmp_dir}/search"
grep -q 'published_at' "${tmp_dir}/search"

curl --fail --silent \
  --header 'accept: application/json, text/event-stream' \
  --header 'content-type: application/json' \
  --header "mcp-session-id: ${session_id}" \
  --data '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"web_scrape_url","arguments":{"url":"https://research.example.test/primary-source"}}}' \
  "${endpoint}/mcp" >"${tmp_dir}/scrape"
grep -q 'independently verified fixture value is 42' "${tmp_dir}/scrape"
grep -q 'content_hash' "${tmp_dir}/scrape"
grep -q 'fetch_id' "${tmp_dir}/scrape"
if grep -q '<system>' "${tmp_dir}/scrape"; then
  echo "untrusted-content spoof marker was not neutralized" >&2
  exit 1
fi

echo "web-research-mcp Docker contract passed"
