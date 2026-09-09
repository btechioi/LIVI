# Arch Linux LIVI kiosk (labwc multi-screen)

Turn a dedicated Arch box into a LIVI kiosk that boots straight into the app with
**independent** screens: the nested `livi-compositor` opens one outer window per
screen (`LIVI` / `Dash` / `Auxiliary`), and **labwc** replaces Cage so each of
those windows is pinned to its own physical output.

- Biggest detected output → **Main**
- Next biggest → **Dash**
- Smallest → **Aux**
- Fewer outputs → fewer screens; their active flags are cleared each boot.
- The mapping is regenerated on every power-on from `wlr-randr`, so hotplug
  chains (turn the phone screen into a Main, etc.) only take effect at the next
  boot of the kiosk.

## Layout

| File | Runs as | Purpose |
|---|---|---|
| `install.sh` | root (on the rig) | one-shot Arch install |
| `files/livi-kiosk-prep.sh` | kiosk user, each boot | detect outputs → emit `~/.config/labwc/rc.xml` rules → merge sizes into `~/.config/LIVI/config.json` |
| `files/livi-kiosk-session.sh` | kiosk user, each boot | start labwc → wait for socket → prep → `labwc --reconfigure` → exec AppImage |
| `files/livi-kiosk.service` | systemd | session service on tty1 |
| `files/livi-kiosk.pam` | PAM | Arch-native policy; `pam_systemd` provides `XDG_RUNTIME_DIR` |
| `files/livi-autologin.conf` | systemd | tty1 autologin drop-in |

## Why labwc and not Cage

LIVI's multi-screen support works by launching a nested smithay compositor
(`resources/compositor/livi-compositor`, bundled in the AppImage). The compositor
relaunches the app inside itself and hosts one window per screen on the outer
compositor. Cage only ever shows a single window, so Main/Dash/Aux would collapse;
labwc is a stacked WM with output placement rules, which is exactly what the
per-screen windows need.

## Window placement

labwc applies window rules **at window creation**, and `MoveToOutput` refuses to
move a fullscreen window — so the reload (`labwc --reconfigure`, SIGHUP) happens
**before** the AppImage starts. The generated `rc.xml`:

```xml
<windowRule title="LIVI">
  <action name="MoveToOutput" output="DP-1" />
  <action name="Maximize" />
</windowRule>
```

Titles come from `native/livi-compositor/rust/src/state.rs::role_title`
(`LIVI`/`Dash`/`Auxiliary`).

## Install

Build a fresh AppImage **first** (the current dev tree has the universal helper
staging for `~/.config/LIVI/driver/livi-helperd` that older builds lack):

```sh
pnpm run build:linux
```

Then, on the kiosk box:

```sh
sudo ./install.sh /path/to/dist/LIVI-9.0.0-linux-x86_64.AppImage
sudo reboot
```

`install.sh` also installs the LIVI udev rules, the root-helper sudoers rule
(`/etc/sudoers.d/99-LIVI-bt`, canonical `~/.config/LIVI/driver/livi-helperd` path),
seatd, PipeWire, tty1 autologin, and flips the default target to
`multi-user.target`.

## Test without rebooting

From a spare VT (`Ctrl+Alt+F2`):

```sh
sudo systemctl start livi-kiosk
journalctl -u livi-kiosk -f
```

The service takes over tty1 (getty hands off and returns on stop). If the app
crashes, the service ends (`Restart=no`) and getty comes back on tty1 for a debug
shell; check `journalctl -u livi-kiosk` for the compositor/sandbox logs.

## Troubleshooting

- **Electron sandbox errors** (`The SUID sandbox helper binary was found, but is
  not configured correctly`): Arch usually allows unprivileged user namespaces,
  but if the kernel blocks them, add `--no-sandbox` to the AppImage command line
  in `files/livi-kiosk-session.sh`.
- **`wlr-randr` finds no outputs**: the session waits up to 30s; make sure the
  user is in `seat`/`render`/`video` and `seatd` is running (`systemctl status seatd`
  and `wlr-randr` from a labwc session).
- **Windows on the wrong screens**: run `/opt/livi-kiosk/livi-kiosk-prep.sh`
  manually with `WAYLAND_DISPLAY` set to the labwc socket and inspect the
  generated `~/.config/labwc/rc.xml`.
- **Nothing on the Aux panel**: assign content to the Aux role in LIVI Settings;
  geometry and on/off state are automatic.

## Rollback

```sh
sudo systemctl disable livi-kiosk.service
sudo systemctl set-default graphical.target
sudo rm /etc/systemd/system/getty@tty1.service.d/livi-autologin.conf
sudo rm -rf /opt/livi-kiosk
sudo systemctl daemon-reload
```