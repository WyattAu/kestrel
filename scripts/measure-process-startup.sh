#!/usr/bin/env bash
# backlog #3 — process-level harness for the REAL frontend binaries (not
# in-process benches). Measures, per binary class:
#
#   KESTREL_STARTUP_MS  p50 wall-clock until the app process execs and is
#                       detected in /proc (wrapper boot included: xvfb-run
#                       for the GUI, script/pty for the TUI)
#   KESTREL_IDLE_RSS_KB p50 idle RSS of the app process after it has stayed
#                       alive through the settle window (crash check)
#
# Gates (requirements.md §8 hard limits apply to idle RSS; the startup bound
# here is a boot budget that catches hung/blocked startups — the interactive
# cold-start SLA is enforced by the criterion benches):
#   TUI:  boot budget 500 ms;  idle RSS fail 40 MB  (warn > 25 MB)
#   GUI:  boot budget 1500 ms; idle RSS fail 200 MB (warn > 120 MB)
#
#   scripts/measure-process-startup.sh <binary> [--xvfb]
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN="${1:-}"; [ -n "$BIN" ] || { echo "usage: $0 <binary> [--xvfb]" >&2; exit 2; }
XVFB=0
[ "${2:-}" = "--xvfb" ] && XVFB=1
BIN="$(realpath -m "$BIN")"
[ -x "$BIN" ] || { echo "binary not found (build first): $BIN" >&2; exit 2; }

APP="$(basename "$BIN")"
case "$APP" in
  kestrel-tui) HARD_STARTUP_MS=500  ; RSS_WARN_MB=25  ; RSS_FAIL_MB=40  ;;
  kestrel-gui) HARD_STARTUP_MS=1500 ; RSS_WARN_MB=120 ; RSS_FAIL_MB=200 ;;
  *)           HARD_STARTUP_MS=1500 ; RSS_WARN_MB=120 ; RSS_FAIL_MB=200 ;;
esac

# /proc is Linux-only; elsewhere report and skip (SLA numbers are Linux).
[ -d /proc ] || { echo "skip: /proc not available (non-Linux)"; exit 0; }

need() { command -v "$1" >/dev/null 2>&1 || { echo "skip: missing $1" >&2; exit 0; }; }
need timeout
if [ "$XVFB" -eq 1 ]; then need xvfb-run; fi
if [ "$APP" = "kestrel-tui" ]; then need script; fi

# comm(5) is truncated to 15 bytes; both binary names fit.
APP_COMM="$APP"

# Polls /proc for a process whose comm (or cmdline, as a fallback) matches
# the app; echoes its pid.
find_app_pid() {
  local tries="${1:-2000}" i pid
  for ((i = 0; i < tries; i++)); do
    for pid in /proc/[0-9]*; do
      if [ -r "$pid/comm" ] && [ "$(cat "$pid/comm" 2>/dev/null)" = "$APP_COMM" ]; then
        echo "${pid#/proc/}"
        return 0
      fi
    done
    sleep 0.005
  done
  return 1
}

CRASH_LOG="$(mktemp)"
trap 'rm -f "$CRASH_LOG"' EXIT

SAMPLES=5
STARTUP=()
RSS_KB=()

for _ in $(seq 1 "$SAMPLES"); do
  start=$(date +%s%N)
  RUNNER=()
  if [ "$XVFB" -eq 1 ]; then RUNNER=(xvfb-run -a); fi
  if [ "$APP" = "kestrel-tui" ]; then
    # TUI needs a pty; wrap in `script` so it stays alive.
    script -qec "timeout 20 $BIN" /dev/null >/dev/null 2>&1 &
  else
    # Headless runners have no GPU: the Slint/femtovg shell needs software
    # GL (mesa llvmpipe), so force it. `--headless` is accepted and ignored.
    timeout 20 "${RUNNER[@]}" env LIBGL_ALWAYS_SOFTWARE=1 "$BIN" >"$CRASH_LOG" 2>&1 &
  fi
  WRAPPER=$!

  # Wait for the real app process to exec (this is the start latency).
  APP_PID="$(find_app_pid 2000 || true)"
  now=$(date +%s%N)
  if [ -z "$APP_PID" ]; then
    echo "FAIL: $APP did not exec within 10s" >&2
    echo "--- last app/wrapper output ---" >&2
    tail -15 "$CRASH_LOG" >&2 || true
    kill -TERM "$WRAPPER" 2>/dev/null || true
    exit 1
  fi
  STARTUP+=("$(( (now - start) / 1000000 ))")

  # Settle window: app must stay alive (crash check), then read its RSS.
  sleep 1.5
  if [ ! -d "/proc/$APP_PID" ]; then
    echo "FAIL: $APP exited during the settle window (startup crash)" >&2
    echo "--- last app output ---" >&2
    tail -15 "$CRASH_LOG" >&2 || true
    kill -TERM "$WRAPPER" 2>/dev/null || true
    exit 1
  fi
  rss=$(awk '/VmRSS/{print $2}' "/proc/$APP_PID/status" 2>/dev/null || echo 0)
  RSS_KB+=("${rss:-0}")

  kill -TERM "$APP_PID" 2>/dev/null || true
  kill -TERM "$WRAPPER" 2>/dev/null || true
  wait "$WRAPPER" 2>/dev/null || true
  sleep 0.5
done

# p50 (median) of the samples.
sorted_s=$(printf '%s\n' "${STARTUP[@]}" | sort -n)
n=$(printf '%s\n' "${STARTUP[@]}" | wc -l)
mid=$(( (n + 1) / 2 ))
P50_MS=$(printf '%s\n' "$sorted_s" | sed -n "${mid}p")
total=0; for r in "${RSS_KB[@]}"; do total=$((total + r)); done
P50_RSS_KB=$((total / ${#RSS_KB[@]}))
P50_RSS_MB=$((P50_RSS_KB / 1024))

echo "KESTREL_STARTUP_MS=$P50_MS"
echo "KESTREL_IDLE_RSS_KB=$P50_RSS_KB"

FAIL=0
if [ "$P50_MS" -gt "$HARD_STARTUP_MS" ]; then
  echo "FAIL: $APP p50 startup ${P50_MS} ms exceeds boot budget ${HARD_STARTUP_MS} ms" >&2
  FAIL=1
fi
if [ "$P50_RSS_MB" -gt "$RSS_FAIL_MB" ]; then
  echo "FAIL: $APP idle RSS ${P50_RSS_MB} MB exceeds hard limit ${RSS_FAIL_MB} MB" >&2
  FAIL=1
elif [ "$P50_RSS_MB" -gt "$RSS_WARN_MB" ]; then
  echo "warning: $APP idle RSS ${P50_RSS_MB} MB above target ${RSS_WARN_MB} MB" >&2
fi

if [ "$FAIL" -eq 1 ]; then exit 1; fi
echo "ok: $APP p50 startup ${P50_MS} ms, idle RSS ${P50_RSS_MB} MB"
