#!/usr/bin/env bash
# livi-kiosk-prep.sh — maps the physical outputs discovered via wlr-randr to the
# LIVI screen roles (main/dash/aux, biggest first), regenerates the labwc rc.xml
# window rules that pin each LIVI window to its output, and merges the detected
# geometry into LIVI's config.json. Runs inside the kiosk session once the
# compositor socket exists (needs $WAYLAND_DISPLAY + seat access).
# Every boot: only roles with a matching physical output are enabled.
set -euo pipefail

CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
LABWC_HOME="$CONFIG_HOME/labwc"
RC="$LABWC_HOME/rc.xml"
LIVI_CFG="$CONFIG_HOME/LIVI/config.json"
ROLES=(main dash aux)

log() { printf 'livi-kiosk-prep: %s\n' "$*" >&2; }

command -v wlr-randr >/dev/null 2>&1 || { log "wlr-randr not found"; exit 1; }
command -v python3 >/dev/null 2>&1 || { log "python3 not found"; exit 1; }

# --- 1. Enumerate connected outputs with an active mode -----------------------
NAMES=(); WS=(); HS=()
while read -r name st geo _rest; do
  [ "$st" = connected ] || continue
  case "$geo" in
    *x*+*+*) ;;
    *) continue ;;
  esac
  w="${geo%%x*}"
  h="${geo#*x}"; h="${h%%+*}"
  [[ "$w" =~ ^[0-9]+$ && "$h" =~ ^[0-9]+$ && "$w" -gt 0 && "$h" -gt 0 ]] || continue
  NAMES+=("$name"); WS+=("$w"); HS+=("$h")
done < <(wlr-randr 2>/dev/null)

[ "${#NAMES[@]}" -gt 0 ] || { log "no connected outputs with an active mode"; exit 1; }

# --- 2. Sort by pixel area, biggest first -------------------------------------
readarray -t ORDERED < <(
  for i in "${!NAMES[@]}"; do
    echo "$((WS[i] * HS[i])) ${NAMES[$i]} $i"
  done | sort -k1 -nr | awk '{print $3}'
)

n=$(( ${#ORDERED[@]} < 3 ? ${#ORDERED[@]} : 3 ))

declare -a OUT W H
for i in $(seq 0 $((n - 1))); do
  idx="${ORDERED[$i]}"
  OUT[$i]="${NAMES[$idx]}"
  W[$i]="${WS[$idx]}"
  H[$i]="${HS[$idx]}"
  log "role ${ROLES[$i]} -> ${OUT[$i]} ${W[$i]}x${H[$i]}"
done

# --- 3. Regenerate labwc rc.xml with per-role window rules --------------------
mkdir -p "$LABWC_HOME"
{
  printf '%s\n' '<?xml version="1.0"?>'
  printf '%s\n' '<labwc_config>'
  printf '%s\n' '  <core>' '    <deco>none</deco>' '  </core>'
  printf '%s\n' '  <keyboard>' '    <default />' '  </keyboard>'
  printf '%s\n' '  <windowRules>'
  for i in $(seq 0 $((n - 1))); do
    case "${ROLES[$i]}" in
      main) title="LIVI" ;;
      dash) title="Dash" ;;
      aux) title="Auxiliary" ;;
    esac
    printf '    <windowRule title="%s">\n' "$title"
    printf '      <action name="MoveToOutput" output="%s" />\n' "${OUT[$i]}"
    printf '      <action name="Maximize" />\n'
    printf '    </windowRule>\n'
  done
  printf '%s\n' '  </windowRules>'
  printf '%s\n' '</labwc_config>'
} > "$RC"

# --- 4. Merge detected geometry into LIVI's config.json ------------------------
printf '' > /tmp/livi-kiosk-alloc
for i in $(seq 0 $((n - 1))); do
  printf '%s %s %s %s\n' "${ROLES[$i]}" "${OUT[$i]}" "${W[$i]}" "${H[$i]}" >> /tmp/livi-kiosk-alloc
done

python3 - "$LIVI_CFG" /tmp/livi-kiosk-alloc <<'PYEOF'
import json, os, sys

cfg_path, alloc_path = sys.argv[1], sys.argv[2]
cfg = {}
if os.path.exists(cfg_path):
    try:
        cfg = json.load(open(cfg_path))
    except Exception:
        cfg = {}

alloc = {}
for line in open(alloc_path):
    role, out, w, h = line.split()
    alloc[role] = {"out": out, "w": int(w), "h": int(h)}

for role in ("main", "dash", "aux"):
    cfg.setdefault("kiosk", {})[role] = role in alloc

m = alloc.get("main")
if m:
    cfg["mainScreenWidth"] = m["w"]
    cfg["mainScreenHeight"] = m["h"]

d = alloc.get("dash")
cfg["dashScreenActive"] = d is not None
if d:
    cfg["dashScreenWidth"] = d["w"]
    cfg["dashScreenHeight"] = d["h"]

a = alloc.get("aux")
cfg["auxScreenActive"] = a is not None
if a:
    cfg["auxScreenWidth"] = a["w"]
    cfg["auxScreenHeight"] = a["h"]

os.makedirs(os.path.dirname(cfg_path), exist_ok=True)
json.dump(cfg, open(cfg_path, "w"), indent=2)
PYEOF

log "config merged into $LIVI_CFG"
exit 0