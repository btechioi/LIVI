#!/usr/bin/env bash
# livi-kiosk-session.sh — outer kiosk session for LIVI on Arch.
# Starts labwc on the kiosk tty, waits for its Wayland socket so the physical
# outputs exist, runs livi-kiosk-prep.sh (detect outputs -> regenerate rc.xml
# window rules + merge LIVI config), reloads labwc config so the rules see the
# new windows, then execs the LIVI AppImage from which the nested livi-compositor
# spawns the per-screen windows ("LIVI"/"Dash"/"Auxiliary") that labwc pins to
# the matching outputs.
set -euo pipefail

export ELECTRON_OZONE_PLATFORM_HINT=wayland
export LIVI_KIOSK=1
unset DISPLAY WAYLAND_DISPLAY XDG_CURRENT_DESKTOP 2>/dev/null || true

PREP="/opt/livi-kiosk/livi-kiosk-prep.sh"
APP="${LIVI_APPIMAGE:-/opt/livi-kiosk/LIVI.AppImage}"

[ -x "$PREP" ] || { echo "livi-kiosk: missing $PREP" >&2; exit 1; }
[ -f "$APP" ]  || { echo "livi-kiosk: missing $APP" >&2; exit 1; }

log() { printf 'livi-kiosk: %s\n' "$*" >&2; }

labwc &
LABWC_PID=$!
trap 'kill "$LABWC_PID" 2>/dev/null || true' EXIT

ready=0
for _ in $(seq 1 150); do
  if kill -0 "$LABWC_PID" 2>/dev/null; then
    socket="$(ls -t "$XDG_RUNTIME_DIR"/wayland-* 2>/dev/null | head -1 || true)"
    if [ -n "$socket" ]; then
      export WAYLAND_DISPLAY="$(basename "$socket")"
      if "$PREP"; then ready=1; break; fi
    fi
  else
    log "labwc exited during startup"
    exit 1
  fi
  sleep 0.2
done

if [ "$ready" -ne 1 ]; then
  log "timed out waiting for labwc/WLR outputs"
  exit 1
fi

log "outputs mapped; reloading labwc config"
# Reload rc.xml so the freshly generated window rules apply to windows created after this point.
labwc --reconfigure || log "labwc --reconfigure failed (window placement may be wrong)"

log "starting $APP"
if "$APP"; then
  rc=0
else
  rc=$?
fi
log "LIVI exited ($rc), tearing the labwc session down"
kill "$LABWC_PID" 2>/dev/null || true
exit "$rc"