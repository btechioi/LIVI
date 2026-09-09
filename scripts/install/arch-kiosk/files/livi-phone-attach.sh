#!/usr/bin/env bash
# livi-phone-attach — rig side of the USB-only phone sensor bridge.
#
# Fired by udev (89-LIVI-phone.rules) when a phone's USB device node appears.
# It sets up the adb reverse tunnel so the phone's 127.0.0.1:9123 reaches this
# rig, then tells the LIVI Sensors app (if installed) to run its foreground
# service. Everything stays on the USB cable — zero wireless.
#
# Exit codes are ignored by udev; logs for debugging.
set -u
LOG=/var/log/livi-phone-attach.log
exec >>"$LOG" 2>&1
echo "[$(date +%T)] livi-phone-attach: devnode=${1:-unknown}"

ADB=$(command -v adb || true)

# Give the phone a moment to finish USB device-mode negotiation.
sleep 2

if [ -z "$ADB" ]; then
  echo "  adb not found (install android-tools); still attempting reverse via helper"
else
  # Keep the phone listed even if an earlier daemon exists.
  "$ADB" start-server || true
  # The phone may enumerate as 'unauthorized' the first time the key changes.
  for i in $(seq 1 15); do
    STATE=$("$ADB" devices | awk 'NR>1 && $2!="" {print $2}' | head -1)
    case "$STATE" in
      device) break ;;
      unauthorized) echo "  waiting for USB authorization ($i)"; sleep 2 ;;
      *) sleep 1 ;;
    esac
  done

  "$ADB" reverse tcp:9123 tcp:9123 \
    && echo "  adb reverse tcp:9123 tcp:9123  (ok)"
fi

# Start the phone-side foreground service over the USB link. This is what makes
# the bridge survive a locked screen / Android Auto being in the foreground.
if [ -n "$ADB" ]; then
  "$ADB" shell am start-foreground-service \
    -n dev.fio.livi.livisensors/.SensorService \
    --es source adb \
    || echo "  am start-foreground-service failed (app missing?)"
  "$ADB" shell am start-service \
    -n dev.fio.livi.livisensors/.SensorService \
    --es source adb \
    || true
fi
echo "  done"