#!/usr/bin/env bash
# backlog #3 — process-level cold-start + idle-RSS harness for the REAL
# frontend binaries (not in-process benches).
#
#   scripts/measure-process-startup.sh <binary> [--xvfb]
#
# Prints, on the last line, machine-readable values:
#   KESTREL_STARTUP_MS=<p50 wall-clock ms to first stable second>
#   KESTREL_IDLE_RSS_KB=<idle RSS of the live process>
# and exits non-zero if the p50 startup exceeds the hard threshold for the
# binary's class (TUI: 150 ms / GUI: 500 ms — requirements §8, bench SLA).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN="${1:-}"; [ -n "$BIN" ] || { echo "usage: $0 <binary> [--xvfb]" >&2; exit 2; }
XVFB=0
[ "${2:-}" = "--xvfb" ] && XVFB=1
BIN="$(realpath -m "$BIN")"
[ -x "$BIN" ] || { echo "binary not found (build first): $BIN" >&2; exit 2; }

case "$(basename "$BIN")" in
  kestrel-tui) HARD_MS=150 ;;
  kestrel-gui) HARD_MS=500 ;;
  *) HARD_MS=500 ;;
esac

need() { command -v "$1" >/dev/null 2>&1 || { echo "skip: missing $1" >&2; exit 0; }; }
need timeout
if [ "$XVFB" -eq 1 ]; then need xvfb-run; fi

RUNNER=()
if [ "$XVFB" -eq 1 ]; then RUNNER=(xvfb-run -a); fi
if [ "$(basename "$BIN")" = "kestrel-tui" ]; then
  # TUI needs a pty; wrap in `script` so it stays alive, then SIGTERM.
  need script
fi

SAMPLES=5
STARTUP=()
RSS=()

for _ in $(seq 1 "$SAMPLES"); do
  start=$(date +%s%N)
  if [ "$(basename "$BIN")" = "kestrel-tui" ]; then
    script -qec "timeout 6 $BIN" /dev/null >/dev/null 2>&1 &
  else
    timeout 6 "${RUNNER[@]}" "$BIN" >/dev/null 2>&1 &
  fi
  PID=$!
  # Startup = wall-clock until the process has been alive one full second
  # (survived init; no crash). Then read idle RSS.
  sleep 1
  now=$(date +%s%N)
  STARTUP+=("$(( (now - start) / 1000000 ))")
  if [ -d "/proc/$PID" ]; then
    rss=$(awk '/VmRSS/{print $2}' "/proc/$PID/status" 2>/dev/null || echo 0)
    RSS+=("${rss:-0}")
  fi
  kill -TERM "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
  sleep 1
done

# p50 of the samples.
sorted=$(printf '%s\n' "${STARTUP[@]}" | sort -n)
n=$(printf '%s\n' "${STARTUP[@]}" | wc -l)
mid=$(( (n + 1) / 2 ))
P50_MS=$(printf '%s\n' "$sorted" | sed -n "${mid}p")
P50_RSS=0
if [ "${#RSS[@]}" -gt 0 ]; then
  total=0; for r in "${RSS[@]}"; do total=$((total + r)); done
  P50_RSS=$((total / ${#RSS[@]}))
fi

echo "KESTREL_STARTUP_MS=$P50_MS"
echo "KESTREL_IDLE_RSS_KB=$P50_RSS"

if [ "$P50_MS" -gt "$HARD_MS" ]; then
  echo "FAIL: p50 startup ${P50_MS} ms exceeds hard threshold ${HARD_MS} ms for $(basename "$BIN")" >&2
  exit 1
fi
echo "ok: $(basename "$BIN") p50 startup ${P50_MS} ms (≤ ${HARD_MS} ms), idle RSS ~${P50_RSS} KB"
