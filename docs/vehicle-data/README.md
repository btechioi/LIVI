# Vehicle Data — retrofitting a classic with no OBD/CAN

LIVI does **not** poll OBD/CAN. It **receives** a typed telemetry payload and rerenders the dashboards
live. So on a car with no factory electronics (e.g. a 1978 Mitsubishi Galant) you become the sensor
harness: wire aftermarket sensors into a microcontroller, and have it push data to LIVI over the
network.

This project is the copy-paste starting point for that build.

- `firmware/esp32-car-data/` — reference ESP32 firmware (sensors + Socket.IO push)
- `shims/python/push_telemetry.py` — laptop shim to test LIVI's dashboards without hardware
- `shims/python/livi-sim/` — `uv`-managed simulator that pushes a coherent loop of **every** telemetry field (drive cycle + environment) to LIVI's Socket.IO server

To try it without any wiring:

```sh
cd shims/python/livi-sim
uv run simulate.py --json                 # print one payload, no network
uv run simulate.py --host <livi-ip>       # stream the sensor loop live
```

Live phone sensors over USB (wired Android Auto cable, **no wireless**):

```sh
uv run simulate.py --host <livi-ip> --phone
# phone shows up in `adb devices` -> open http://127.0.0.1:9123 on the phone
# -> tap "Start sensors". GPS + magnetometer + accelerometer then drive the
# speed, heading and position instead of the synthetic drive cycle.
```

For a phone-side bridge that keeps streaming with the screen locked or Android Auto
in the foreground, install the native companion app in
[`android/livi_sensors`](android/livi_sensors) — a Flutter shell over a Kotlin
foreground service that talks the same WebSocket protocol over the same
`adb reverse` tunnel and auto-launches when the rig detects the phone (`89-LIVI-phone.rules`).

The phone reaches the rig over the ADB reverse port forward set up by `simulate.py`
(`adb reverse tcp:9123 tcp:9123`): the phone's `localhost:9123` maps to the rig over
the USB cable, so the sensor stream never touches a wireless network.

The authoritative field contract lives in
[`src/main/shared/types/Telemetry.ts`](../../src/main/shared/types/Telemetry.ts). That file is the
single source of truth — read it before adding fields.

---

## 1. How data reaches LIVI

Two transports, identical JSON shape (`TelemetryPayload`):

| Transport | How | Example |
|---|---|---|
| **Socket.IO** | `ws://<livi-host>:4000`, event `telemetry:push` | `io('ws://<host>:4000').emit('telemetry:push', { speedKph: 73 })` |
| **IPC** (renderer) | `telemetry:push` on IPC (main-process only) | — |
| **USB bridge** (current source only, not in the v9.x AppImage) | `car <Key> <value>` lines on a USB CDC port | `car speedKph 73` |

The Socket.IO server listens on **all interfaces**, port **4000**, no auth. Merging is per-field —
partial pushes are fine, LIVI re-broadcasts only what changed (`TelemetryStore`).

The head unit's own UI consumes these fields (routing table `TELEMETRY_ROUTES`); the same fields also
feed the AA / CarPlay-native channels when present.

## 2. Sensor → field map

Everything below is `TelemetryPayload`. The gauges on Dash 1–3 read: `speedKph`, `rpm`, `gear`,
`oilC` (bottom-left temp), `fuelPct` (bottom-right), and the telltale bar reads `lights`, `highBeam`,
`parkingBrake`, `turn`, `hazards`, `ambientC`.

| What | Tap / sensor | Field | Sensor? |
|---|---|---|---|
| Speed | reed/hall on the speedometer cable, or a GPS module | `speedKph` / `gps.speedMs` | 1 (or GPS) |
| RPM | ignition coil (−) wire pulse, via opto | `rpm` | ~0 (wire only) |
| Coolant | NTC thermistor in the top radiator hose | `coolantC` | 1 |
| Oil temp | oil-temp probe in the sump | `oilC` | 1 |
| Fuel | existing sender resistance (float) | `fuelPct` | 0 (reuse) |
| Left/right blinkers | column / fusebox wiring, via PC817 | `turn: 'left' / 'right'` | 0 |
| Hazards | hazard switch wiring, via PC817 (optional) | `hazards` | 0 |
| Low beam | headlamp relay, via PC817 | `lights` | 0 |
| High beam | dip switch wiring, via PC817 | `highBeam` | 0 |
| Parking brake | handbrake switch, via PC817 | `parkingBrake` | 0 |
| Battery | 12V → voltage divider → ADC | `batteryV` | 0 |
| Outside temp | sensor on the grille / mirror (optional) | `ambientC` | 1 (opt) |

### Per-car calibration knobs (defaults for the firmware)

| Signal | Formula |
|---|---|
| NTC temp | 10 kΩ NTC + 10 kΩ to 3V3; `R = 10k · V · (3.3 − V)⁻¹`; temp via B-equation (`B = 3950`, `R0 = 10k @ 25 °C`) |
| Battery | divider 100 kΩ / 10 kΩ (`×11`) |
| Fuel sender | series resistor to 3V3; map `R_em pty … R_full` → 0–100% (defaults 1 Ω → 90 Ω) |
| RPM | `PULSES_PER_REV` (2 for a 4-cyl distributor) → `rpm = pulses/s · 60 / PULSES_PER_REV` |
| Wheel speed | `PULSES_PER_KM` from the speedo-cable pickup → `km/h = pulses/s · 3600 / PULSES_PER_KM` |

## 3. Wiring a 12V signals input (PC817)

Every 12V digital signal (blinker, lights, brake…) gets its **own PC817**. One channel:

```
PC817 (DIP-4, notch at top)
  pin 1  anode       pin 3  emitter
  pin 2  cathode     pin 4  collector

car 12V signal ──┬──[ 1 kΩ 1/2W ]── pin 1        car ground ── pin 2
ESP32 3V3 ──[ 10 kΩ ]──┐                                       ├─ pin 4 (collector) ── GPIO
                       └────────────────────────────────────────┴─ pin 3 (emitter) ── GND
```

- Resistor: 1 kΩ gives ~10 mA LED current at 12V (≈13 mA at 14.4V running) — safe.
- Output is **active‑LOW**: input off → GPIO high (10 kΩ pull-up); input on → transistor saturates → GPIO low.
- **Common ground is mandatory**: car chassis ground must tie to the ESP32 GND.
- React to blinkers by sampling the GPIO ≈50 Hz and timing the flashes.
- Optional protection (recommended on an old car): series **1N4007** (cathode → pin 1) in case the tap swings negative; a **TVS/flyback** if you ever tap anything inductive (coil, relays).

> Never feed 12V straight into an ESP32 pin. The optocoupler is the isolation boundary — put the
> divider *and* the opto on the car side of the wiring.

## 4. Analog inputs (ESP32)

- **Thermistors**: 10 kΩ NTC + 10 kΩ pull-up to 3V3, node → ADC pin. Use `ATTEN_11db` for full 0–3.3 V range.
- **Battery**: 100 kΩ (to 12V) / 10 kΩ (to GND) divider → ADC. Max ≈ 1.45V at 14.5V — stays under 3.3V. Compute `batteryV = V_node × 11`.
- **Fuel sender**: series resistor (e.g. 470 Ω) to 3V3, sender to GND → ADC. `R = R_ser × (3.3 − V) / V`, then map to `fuelPct`.

## 5. Network

The ESP32 joins the same network as LIVI (LIVI's own hotspot, or the LAN the head unit is on), then
connects: `ws://<livi-ip>:4000` and emits `telemetry:push`.

Outbound (for debugging): LIVI re-broadcasts every merged snapshot as `telemetry:update` to all
connected Socket.IO clients.

## 6. Quick test without hardware

```bash
pip install "python-socketio[client]"
python docs/vehicle-data/shims/python/push_telemetry.py --host <livi-ip> --demo
```

The `--demo` mode drives a fake speed/rpm/fuel cycle so you can watch the dash react before building
the harness.

## 7. Safety

- All 12V work: disconnect the battery, fuse every feed you tap.
- Coil / ignition wiring carries dangerous voltages — isolate with the opto + TVS and route the
  output away from the ECU/igniter wiring.
- Fuse the 12V supply to the MCU (e.g. 1 A inline blade fuse at the fusebox).
- Ground everything at one star point near the fusebox to avoid ground loops with the radio/head unit.