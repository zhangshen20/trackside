#!/usr/bin/env bash
# Records and cuts the submission video in one go: builds the MCP server and the simulator from
# this checkout, runs them locally on the live data snapshot (memory in the real DynamoDB table,
# Claude on Bedrock choosing the tools), records the scenes in scenes.json with Playwright,
# speaks the lines with Polly (or espeak as a placeholder) and renders the MP4 with ffmpeg.
#
#   demo/video/make.sh [OUT.mp4]
#
# Needs: cargo, node with playwright (and a Chromium it can launch), python3 with boto3, ffmpeg,
# espeak-ng for the placeholder voice, and AWS credentials (TRACKSIDE_ROLE_ARN is assumed when set)
# that can read Trackside's bucket, read and write the memory table, call Bedrock and, for the real
# voices, Polly. Set TRACKSIDE_SNAPSHOT to a local snapshot to skip the download; VOICE_ENGINE to
# polly|espeak|auto (default auto); SCENES to re-record only some scenes.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
work=${VIDEO_WORK:-$here/work}
out=${1:-$work/trackside-demo.mp4}
mkdir -p "$work"
export VIDEO_WORK=$work
export AWS_REGION=${AWS_REGION:-ap-southeast-2}
export TRACKSIDE_MEMORY_TABLE=${TRACKSIDE_MEMORY_TABLE:-trackside-listeners}

echo "== build"
(cd "$root" && cargo build --release -p trackside-mcp -p trackside-sim)

snapshot=${TRACKSIDE_SNAPSHOT:-$work/snapshot.json.gz}
if [ ! -f "$snapshot" ]; then
  echo "== snapshot"
  python3 "$here/awsx.py" fetch-snapshot "$snapshot"
fi

echo "== servers"
pids=()
cleanup() { for p in "${pids[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT
TRACKSIDE_STATELESS=1 TRACKSIDE_SNAPSHOT=$snapshot TRACKSIDE_BIND=127.0.0.1:8000 \
  python3 "$here/awsx.py" run "$root/target/release/trackside-mcp" >"$work/mcp.log" 2>&1 &
pids+=($!)
TRACKSIDE_SIM_MCP_URL=http://127.0.0.1:8000/mcp TRACKSIDE_SIM_BIND=127.0.0.1:8001 TRACKSIDE_SIM_VOICE=off \
  python3 "$here/awsx.py" run "$root/target/release/trackside-sim" >"$work/sim.log" 2>&1 &
pids+=($!)
for _ in $(seq 1 30); do
  curl -sf http://127.0.0.1:8001/sim/api/apps >/dev/null 2>&1 && break
  sleep 1
done
curl -sf http://127.0.0.1:8001/sim/api/apps >/dev/null || { echo "simulator did not come up; see $work/sim.log"; exit 1; }

echo "== record"
node "$here/record.cjs"

echo "== voices"
python3 "$here/awsx.py" run python3 "$here/voice.py" --engine "${VOICE_ENGINE:-auto}"

echo "== render"
python3 "$here/render.py" "$out"
