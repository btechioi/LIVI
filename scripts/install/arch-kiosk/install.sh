#!/usr/bin/env bash
# install.sh — Arch Linux LIVI kiosk installer (labwc multi-screen edition).
#
# Designed to be run on the TiM (trip media / car rig) box, NOT a daily driver.
# It installs labwc + seatd + PipeWire, adds the running user to the seat/render
# groups, writes the LIVI udev + sudoers rules, installs the kiosk session to
# /opt/livi-kiosk, configures tty1 autologin and points the default boot target
# at multi-user.target so the kiosk owns the screen at power-on.
#
# Usage:
#     sudo ./install.sh [/path/to/LIVI.AppImage]
#
# If no AppImage path is given the newest dist/LIVI-*.AppImage is used, then
# /opt/livi-kiosk/LIVI.AppImage is expected to exist already.
set -euo pipefail

[ "$(id -u)" -eq 0 ] || { echo "run as root: sudo ./install.sh [AppImage]" >&2; exit 1; }
command -v pacman >/dev/null 2>&1 || { echo "Arch Linux (pacman) required" >&2; exit 1; }

LIVI_USER="${SUDO_USER:-$(logname 2>/dev/null || echo kiosk)}"
USER_HOME="$(eval echo "~$LIVI_USER")"
[ -d "$USER_HOME" ] || { echo "home directory for $LIVI_USER not found" >&2; exit 1; }

SRC_DIR="$(cd "$(dirname "$0")" && pwd)"
FILES_DIR="$SRC_DIR/files"
DEST_DIR="/opt/livi-kiosk"
APP_PATH="${1:-}"
REPO_DIST="$(cd "$SRC_DIR/../../.." 2>/dev/null && echo "$PWD/dist" || echo '')"

step() { printf '\n==> %s\n' "$*"; }
info() { printf '    %s\n' "$*"; }

# --- AppImage ----------------------------------------------------------------
if [ -z "$APP_PATH" ]; then
  if [ -n "$REPO_DIST" ]; then
    APP_PATH="$(ls -t "$REPO_DIST"/LIVI-*.AppImage 2>/dev/null | head -1 || true)"
  fi
fi
if [ -z "$APP_PATH" ]; then
  APP_PATH="$DEST_DIR/LIVI.AppImage"
fi
if [ ! -f "$APP_PATH" ]; then
  echo "AppImage not found at '$APP_PATH'. Pass one: sudo ./install.sh /path/to/LIVI.AppImage" >&2
  exit 1
fi
APP_PATH="$(realpath "$APP_PATH")"

step "Installing packages"
pacman -S --needed --noconfirm labwc seatd wlr-randr pipewire pipewire-pulse wireplumber android-tools

step "Adding $LIVI_USER to seat/render/audio groups"
groups_to_add=()
for g in video input render plugdev seat audio; do
  getent group "$g" >/dev/null 2>&1 && groups_to_add+=("$g")
done
if [ "${#groups_to_add[@]}" -gt 0 ]; then
  usermod -aG "$(IFS=,; echo "${groups_to_add[*]}")" "$LIVI_USER"
fi

step "Enabling seatd"
systemctl enable --now seatd

step "Preloading wired-CarPlay net drivers (cold-boot AV interface)"
mkdir -p /etc/modules-load.d
cat > /etc/modules-load.d/livi.conf <<'EOF'
cdc_ncm
cdc_ether
ipheth
EOF

step "Enabling lingering for user services (PipeWire/BT)"
loginctl enable-linger "$LIVI_USER"

step "Installing kiosk session to $DEST_DIR"
mkdir -p "$DEST_DIR"
install -T -m 0755 "$FILES_DIR/livi-kiosk-prep.sh"   "$DEST_DIR/livi-kiosk-prep.sh"
install -T -m 0755 "$FILES_DIR/livi-kiosk-session.sh" "$DEST_DIR/livi-kiosk-session.sh"
install -T -m 0755 "$APP_PATH" "$DEST_DIR/LIVI.AppImage"
info "AppImage: $APP_PATH -> $DEST_DIR/LIVI.AppImage"

step "Installing udev rules + touch filter"
install -d -m 0755 /usr/local/lib/livi
install -m 0755 "$SRC_DIR/../../../assets/linux/livi-touch-filter" /usr/local/lib/livi/livi-touch-filter
udev_src="$SRC_DIR/../../../assets/linux/99-LIVI.rules.template"
install -m 0644 "$udev_src" /etc/udev/rules.d/99-LIVI.rules
install -m 0644 "$SRC_DIR/files/89-LIVI-phone.rules" /etc/udev/rules.d/89-LIVI-phone.rules
install -m 0755 "$SRC_DIR/files/livi-phone-attach.sh" /usr/local/lib/livi/livi-phone-attach.sh
udevadm control --reload-rules && udevadm trigger

step "Installing sudoers rule for the root helper (BT/Wi-Fi)"
sudoers_src="$SRC_DIR/../../../assets/linux/99-LIVI-bt.sudoers.template"
install -m 0440 -o root -g root /dev/null /etc/sudoers.d/99-LIVI-bt
sed "s/__USERNAME__/$LIVI_USER/g" "$sudoers_src" > /etc/sudoers.d/99-LIVI-bt
visudo -cf /etc/sudoers.d/99-LIVI-bt

step "Configuring tty1 autologin"
mkdir -p /etc/systemd/system/getty@tty1.service.d
sed "s/__LIVI_USER__/$LIVI_USER/g" "$FILES_DIR/livi-autologin.conf" \
  > /etc/systemd/system/getty@tty1.service.d/livi-autologin.conf

step "Writing /etc/pam.d/livi-kiosk"
install -m 0644 "$FILES_DIR/livi-kiosk.pam" /etc/pam.d/livi-kiosk

step "Writing livi-kiosk.service"
sed "s/__LIVI_USER__/$LIVI_USER/g" "$FILES_DIR/livi-kiosk.service" > /etc/systemd/system/livi-kiosk.service

systemctl daemon-reload
systemctl enable livi-kiosk.service

if [ "$(systemctl get-default)" != multi-user.target ]; then
  step "Default target -> multi-user.target (kiosk owns the screen)"
  systemctl set-default multi-user.target
fi

step "Removing NetworkManager-wait-online (faster cold boot)"
systemctl disable NetworkManager-wait-online.service >/dev/null 2>&1 || true

cat <<EOF

LIVI kiosk installed for user $LIVI_USER.

  - AppImage:            $DEST_DIR/LIVI.AppImage
  - Kiosk service:       /etc/systemd/system/livi-kiosk.service (enabled)
  - tty1 autologin:      /etc/systemd/system/getty@tty1.service.d/livi-autologin.conf
  - Output detection:    $DEST_DIR/livi-kiosk-prep.sh (runs each boot,
                         maps biggest output to Main, next to Dash, last to Aux)

On next power-on LIVI will start in labwc kiosk mode with Main/Dash/Aux
pinned to the detected outputs. Test now from a spare VT (Ctrl+Alt+F2)
without rebooting:

    sudo systemctl start livi-kiosk    # owns tty1, getty hands over

To stop it:                          sudo systemctl stop livi-kiosk
To disable autostart:                sudo systemctl disable livi-kiosk.service
To restore the desktop target:       sudo systemctl set-default graphical.target && \
                                     rm /etc/systemd/system/getty@tty1.service.d/livi-autologin.conf

Aux panel content is chosen in LIVI Settings (assign dashboards/pages to
the Aux screen role); geometry and on/off state are handled automatically.
EOF