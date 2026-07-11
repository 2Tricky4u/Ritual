#!/usr/bin/env bash
# Hardware-in-the-loop check template (embedded projects).
# fast  = build + static checks only (no hardware needed)
# full  = flash the target, capture serial output, assert on it
#
# Pair with `check_timeout_secs` in .ritual/config.toml — a dead board must
# fail the check, not hang the pipeline.
set -e

# ---- build + static (fast path) --------------------------------------------
# Zephyr example: west build -b "$BOARD" -p auto
# ESP-IDF example: idf.py build
make build 2>/dev/null || west build -p auto 2>/dev/null || {
  echo "adapt the build step to your toolchain" >&2
  exit 1
}
[ "${1:-}" = fast ] && exit 0

# ---- flash ------------------------------------------------------------------
# west flash / idf.py flash / probe-rs run / openocd ...
make flash 2>/dev/null || west flash 2>/dev/null

# ---- capture serial + assert -----------------------------------------------
PORT="${SERIAL_PORT:-/dev/ttyUSB0}"
BAUD="${SERIAL_BAUD:-115200}"
CAPTURE_SECS="${CAPTURE_SECS:-10}"
LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

timeout "$CAPTURE_SECS" cat "$PORT" > "$LOG" 2>/dev/null || true
stty -F "$PORT" "$BAUD" 2>/dev/null || true

# Assert on what the firmware must print (adapt patterns):
grep -q "BOOT OK"        "$LOG" || { echo "no boot banner"; tail -20 "$LOG"; exit 1; }
! grep -qE "PANIC|FAULT" "$LOG" || { echo "fault detected"; grep -E "PANIC|FAULT" "$LOG"; exit 1; }

echo "HIL check passed"
