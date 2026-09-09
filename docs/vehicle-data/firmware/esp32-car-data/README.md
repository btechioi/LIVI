# ESP32 Reference Firmware — LIVI Vehicle Data Box

Streams `TelemetryPayload` fields to LIVI's Socket.IO server (port 4000) at 10 Hz.

## Libraries

Install from the Arduino Library Manager:

- `ArduinoJson` (bblanchon)
- `arduinoSocketIOClient` (aion-lp) — depends on `WebSockets` (links2004)

## Pin map

| ESP32 pin | Function | Wiring |
|---|---|---|
| 34 | Coolant NTC | 10 kΩ NTC + 10 kΩ pull-up to 3V3, mid node → 34 |
| 35 | Oil NTC | same as above |
| 36 | Battery | 100 kΩ (12V) / 10 kΩ (GND) divider → 36 |
| 39 | Fuel sender | 470 Ω to 3V3, sender to GND, mid node → 39 |
| 25 | Left blinker | PC817 output (active-low, 10 kΩ pull-up) |
| 26 | Right blinker | PC817 output |
| 27 | Low beam | PC817 output |
| 14 | High beam | PC817 output |
| 12 | Parking brake | PC817 output |
| 33 | Hazards *(optional)* | PC817 output; set `PIN_HAZARDS -1` to derive from L+R |
| 5 | RPM | coil (−) via PC817 + series protection; `RISING` count |
| 17 | Wheel speed *(optional)* | reed/hall on speedo cable via PC817; `-1` to disable (use GPS) |

One 3V3 power rail and a common ground to car chassis (star point at the fusebox).

## Calibrate before first drive

1. **`PULSES_PER_KM`** — count pulses between two mile markers, or log `wheelHz` at a known speed.
2. **`PULSES_PER_REV`** — for a distributor: one spark per cylinder per 2 crank revs. 4-cyl → 2.
3. **NTC beta** — read the datasheet of your probe; generic 10 kΩ probes ≈ 3950.
4. **Fuel sender** — measure ohms empty and full with a multimeter; set `FUEL_EMPTY_OHM` / `FUEL_FULL_OHM`.
5. **`batteryV`** — trust a multimeter: adjust `BATTERY_TOP_KOHM`/`BATTERY_LOW_KOHM` if off (default is 11×).

## Gear

`USE_GEAR_AUTO 0` sends no gear field (dash shows `P`). For a manual you can:

- add a gear position switch (five PC817 inputs, one per gear) and push `gear` as a string/number, or
- enable `USE_GEAR_AUTO 1` and tune the speed/rpm ratio table — it is deliberately crude.

## Flash & verify

1. Set `WIFI_SSID`, `WIFI_PASS`, `LIVI_HOST` (the head unit / LIVI host IP).
2. Upload, open Serial Monitor (115200). Wait for `[net] connected`.
3. On LIVI, enable the dashboards and watch the gauges move.

## Debug

LIVI re-broadcasts the merged snapshot as `telemetry:update` to all Socket.IO clients. See
the Python shim (`../../shims/python/push_telemetry.py`) to echo it from a laptop.