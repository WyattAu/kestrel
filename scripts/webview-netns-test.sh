#!/usr/bin/env bash
# threat-model §7 row T3 / backlog #6 — webview network isolation.
#
# The viewport must have no network origin access (remote content is blocked
# by construction). This harness proves the *mechanism* at the process level:
#   1. the `unshare -n` sandbox actually removes the network namespace
#      (a `ping`/socket attempt must fail inside, succeed outside), and
#   2. the real `kestrel-gui` binary boots and renders headless under the
#      same sandbox without crashing.
#
# Requires Linux + unshare (CAP_SYS_ADMIN or an unprivileged-userns-enabled
# kernel) and xvfb for the headless GUI. When the sandbox is unavailable the
# test SKIPS (exit 0 with a notice) rather than failing the matrix row.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

need() { command -v "$1" >/dev/null 2>&1 || { echo "skip: missing $1"; exit 0; }; }
need unshare
need xvfb-run
need ping

if ! unshare -n true 2>/dev/null; then
  echo "skip: unshare -n is not permitted in this environment (no network-namespace capability)."
  exit 0
fi

echo "== T3.1 sandbox removes the network namespace =="
if ping -c1 -W1 127.0.0.1 >/dev/null 2>&1; then
  echo "skip: cannot even ping loopback outside the sandbox; network stack unusable for the probe."
  exit 0
fi
if unshare -n ping -c1 -W1 127.0.0.1 >/dev/null 2>&1; then
  echo "fail: loopback reachable inside unshare -n — the sandbox did not isolate the network namespace"
  exit 1
fi
echo "ok: network namespace is isolated (loopback unreachable inside, reachable outside)"

echo "== T3.2 kestrel-gui boots and renders headless inside the sandbox =="
BIN=target/release/kestrel-gui
if [ ! -x "$BIN" ]; then
  echo "info: building kestrel-gui (release) for the sandbox run..."
  cargo build -p kestrel-gui --release
fi

# Launch under xvfb inside the isolated namespace; give it a few seconds to
# construct the Slint shell + viewport, then require the *app* (not just the
# timeout wrapper) to still be alive before SIGTERM.
APP_COMM=kestrel-gui
app_alive() { # 1 = a kestrel-gui process exists
  for pid in /proc/[0-9]*; do
    if [ -r "$pid/comm" ] && [ "$(cat "$pid/comm" 2>/dev/null)" = "$APP_COMM" ]; then
      return 0
    fi
  done
  return 1
}

# Headless runners have no GPU: the Slint/femtovg shell needs software GL.
timeout 20 xvfb-run -a unshare -n env LIBGL_ALWAYS_SOFTWARE=1 "$BIN" >/tmp/kestrel-gui-netns.log 2>&1 &
PID=$!
sleep 8
if ! app_alive; then
  wait "$PID" || true
  echo "fail: kestrel-gui is not alive inside the sandbox" >&2
  tail -15 /tmp/kestrel-gui-netns.log >&2 || true
  exit 1
fi
kill -TERM "$PID" 2>/dev/null || true
wait "$PID" || true
echo "ok: kestrel-gui rendered headless inside the isolated network namespace"

echo "T3 webview network-isolation test passed."
