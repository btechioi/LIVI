#!/usr/bin/env python3
"""LIVI telemetry simulator — pushes every field the LIVI telemetry API accepts.

Ejects a coherent 240-second drive cycle (standstill, urban run, junction stop,
reverse manoeuvre, freeway cruise, lane changes, coasting, final stop) while a
second, scaled clock drives the environment (ambient temperature, daylight,
dimmer). All values are derived from shared state, so they stay physically
consistent: speed feeds rpm/gear/odometer, throttle feeds load/MAP/boost/lambda,
load feeds the engine temperatures, alternator state feeds battery voltage,
fuel burn feeds fuel level, range and consumption.

Runs as a `uv` project from this directory:

    uv run simulate.py --host 192.168.1.20
    uv run simulate.py --host livi.local --hz 5 --seed 7

Offline mode (no network, prints one payload and exits):

    uv run simulate.py --json
    uv run simulate.py --json | python -m json.tool

Live phone sensors (USB only, no wireless — wired Android Auto cable):

    uv run simulate.py --host livi.local --phone
    # 1. Plug the phone in; phone shows up in `adb devices`.
    # 2. On the phone open http://127.0.0.1:9123  (forwarded over USB by adb reverse).
    # 3. Tap "Start sensors". GPS + magnetometer + accelerometer then drive the
    #    speed, heading and position instead of the synthetic drive cycle.

Deps are declared in pyproject.toml; `uv` creates the environment on first run.
"""
from __future__ import annotations

import argparse
import json
import math
import random
import sys
import time

import socketio


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--host", help="LIVI host IP or hostname (unused with --json)")
    p.add_argument("--port", type=int, default=4000, help="LIVI telemetry port")
    p.add_argument("--hz", type=float, default=10.0, help="push rate in Hz")
    p.add_argument("--seed", type=int, default=42, help="RNG seed for reproducible runs")
    p.add_argument("--env-scale", type=float, default=360.0,
                   help="how fast the day/night clock runs (360 = a full day every 4 min)")
    p.add_argument("--json", action="store_true",
                   help="print one telemetry payload as JSON and exit (no network)")
    p.add_argument("--once", action="store_true",
                   help="connect, push a single payload, then exit")
    p.add_argument("--echo", action="store_true",
                   help="echo telemetry:update broadcasts returned by LIVI")
    p.add_argument("--phone", action="store_true",
                   help="use the USB-connected phone's GPS / magnetometer / "
                        "accelerometer (wired Android Auto cable; adb reverse, no wireless)")
    p.add_argument("--phone-port", type=int, default=9123,
                   help="USB bridge port (default 9123)")
    return p


CYCLE = 240.0        # manoeuvre cycle length in seconds
START_ODO_KM = 142000.0
TANK_LITRES = 55.0
BATTERY_CAPACITY_KWH = 60.0

LAT0, LNG0 = 52.5200, 13.4050               # Berlin; matches the simulated timezone
DEG_LAT_M = 111320.0
DEG_LNG_M = 111320.0 * math.cos(math.radians(LAT0))


class Ambient:
    """Day/night model. `sod` = seconds-of-day on the scaled environment clock."""

    def __init__(self, rng: random.Random):
        self.rng = rng
        self.baro = 100.2
        self._baro_phase = rng.uniform(0, 2 * math.pi)

    def update(self, sod: float, dt: float) -> None:
        self.baro = 100.2 + 0.9 * math.sin(self._baro_phase + sod / 2400.0) \
            + self.rng.gauss(0, 0.05)

    def sun(self, sod: float) -> float:
        # 0 before 06:00 and after 18:00, sinusoidal peak at noon.
        return max(0.0, math.sin(math.pi * max(0.0, min(1.0, (sod - 6 * 3600) / (12 * 3600)))))

    def ambient_c(self, sun: float) -> float:
        return 5.0 + 17.0 * sun + self.rng.gauss(0, 0.15)

    def lux(self, sun: float) -> float:
        return round(15 + sun ** 1.4 * 75000, 1)

    def dimmer_pct(self, sun: float) -> float:
        return round(max(8.0, min(100.0, 100.0 * (1.0 - sun) + 8.0)), 1)


class Vehicle:
    """Cross-coupled vehicle sensor state."""

    def __init__(self, rng: random.Random):
        self.rng = rng
        self.t = 0.0                       # manoeuvre clock
        self.speed = 0.0                   # km/h
        self.throttle = 0.0
        self.brake = 0.0
        self.reverse = False
        self.parking_brake = True
        self.hazards = False
        self.high_beam = False
        self.lights = False
        self.turn = "none"
        self.steering_deg = 0.0
        self.gear = "P"
        self.rpm = 750.0
        self.coolant = 30.0
        self.oil = 35.0
        self.transmission = 40.0
        self.battery_v = 12.6
        self.fuel_pct = 64.0
        self.fuel_rate_lph = 0.6
        self.cons_avg = 8.2
        self.odo_km = START_ODO_KM
        self.trip_km = 0.0
        self.map_kpa = 99.0
        self.boost_kpa = 0.0
        self.lambda_ = 1.0
        self.heading = 90.0                # starting driving direction, deg
        self.lat = LAT0
        self.lng = LNG0
        self.alt = 34.0
        self.sat_used = 12
        self.acc_lux = 0.0
        self.dimmer = 100.0
        self.volume = 0.22
        self._rpm_phase = rng.uniform(0, 2 * math.pi)
        self._temp_phase = rng.uniform(0, 2 * math.pi)
        self._steer_phase = rng.uniform(0, 2 * math.pi)
        self._man_ticks = 0
        self._hazard_until = -1.0
        self._est_speed_ms = 0.0

    def _windows(self) -> tuple[float, float, bool]:
        m = self.t % CYCLE
        reverse = 100.0 <= m < 112.0
        if m < 20.0:                       # standstill, parking brake
            return 0.0, 1.0, False
        if m < 60.0:                       # urban acceleration
            return 0.62, 0.0, False
        if m < 75.0:                       # cruise, left lane change at t 62-66
            return 0.28, 0.0, False
        if m < 90.0:                       # braking for junction
            return 0.0, 0.65, False
        if m < 100.0:                      # at the junction
            return 0.0, 0.0, False
        if m < 112.0:                      # reverse out
            return 0.35, 0.0, True
        if m < 120.0:                      # back to D, pull forward
            return 0.3, 0.0, False
        if m < 150.0:                      # freeway acceleration
            return 0.62, 0.0, False
        if m < 175.0:                      # freeway cruise, right lane change 155-159
            return 0.34, 0.0, False
        if m < 195.0:                      # coast down to 70
            return 0.0, 0.0, False
        if m < 220.0:                      # 70 cruise, left merge 205-209
            return 0.22, 0.0, False
        return 0.0, 0.85, False            # final braking, hazards

    def step(self, dt: float, amb: Ambient, sod: float, phone: "PhoneSensorState | None" = None) -> None:
        self.t += dt
        self._man_ticks += 1
        self._phone = phone
        throttle_t, brake_t, reverse = self._windows()
        self.throttle = throttle_t
        self.brake = brake_t
        self.reverse = reverse

        m = self.t % CYCLE
        speed_ms = self.speed / 3.6
        acc = self.throttle * 2.8 - 0.0022 * speed_ms * speed_ms - self.brake * 5.5
        self.speed = max(0.0, self.speed + acc * 3.6 * dt)
        if self.reverse:
            self.speed = 6.0 if m < 108.0 else 3.0   # steady reverse crawl
        self._phone_speed(phone, dt)

        self._env(sod, amb)
        self._telltales(m)
        self._gearbox()
        self._engine(dt, amb, sod)
        self._electrical()
        self._fuel_and_range(dt)
        self._intake_and_boost(amb)
        self._steering(m)
        self._gps(dt)

    def _env(self, sod: float, amb: Ambient) -> None:
        sun = amb.sun(sod)
        self._ambient_sun = sun
        self._ambient_c = amb.ambient_c(sun)
        self.acc_lux = amb.lux(sun)
        self.dimmer = amb.dimmer_pct(sun)

    def _telltales(self, m: float) -> None:
        sun = self._ambient_sun
        self.lights = sun < 0.30
        self.parking_brake = (m < 20.0) or (m >= 239.0 and self.speed == 0.0)
        self.hazards = 0.0 <= m - 218.0 < 6.0 or self.t < self._hazard_until
        if 218.0 <= m < 224.0:
            self._hazard_until = self.t + 14.0

        self.high_beam = self.lights and 150.0 <= m < 175.0   # freeway cruise at night

        self.turn = "none"
        for start, dur, direction in ((62.0, 4.0, "left"),
                                      (155.0, 4.0, "right"),
                                      (205.0, 4.0, "left")):
            if start <= m < start + dur:
                self.turn = direction
                break

    def _high_beam_time(self) -> bool:
        # deterministic pseudo-random blips while it is dark
        return int(self.t // 45) % 7 == 0 and (self.t % 45) < 14

    def _gearbox(self) -> None:
        v = self.speed
        if self.reverse:
            self.gear = "R"
        elif v == 0.0 and self.parking_brake:
            self.gear = "P"
        elif v < 0.6:
            self.gear = "N"
        else:
            self.gear = "D"
        self.rpm = 750.0 + self.throttle * 2600.0 \
            + self.speed * 22.0 * (1.35 - self.throttle) \
            + 40.0 * math.sin(self._rpm_phase + self.t * 3.0)
        self.rpm = max(700.0, min(6100.0, self.rpm))

    def _engine(self, dt: float, amb: Ambient, sod: float) -> None:
        load = self.throttle * 100.0 + self.speed * 0.22
        target_coolant = 80.0 + load * 0.22 + self._ambient_c * 0.12
        target_oil = target_coolant + 12.0 + self.throttle * 8.0
        target_trans = 50.0 + self.speed * 0.2 + self.throttle * 16.0
        k = 1.0 - math.exp(-dt / 35.0)
        self.coolant += (target_coolant - self.coolant) * k
        self.oil += (target_oil - self.oil) * (1.0 - math.exp(-dt / 55.0))
        self.transmission += (target_trans - self.transmission) * (1.0 - math.exp(-dt / 50.0))
        self.coolant += self.rng.gauss(0, 0.05)

    def _electrical(self) -> None:
        lights_w = 1.6 if self.lights else 0.0
        hazards_w = 2.4 if self.hazards else 0.0
        beam_w = 2.0 if self.high_beam else 0.0
        audio_w = self.volume * 6.0
        load_w = lights_w + hazards_w + beam_w + audio_w + self.throttle * 18.0
        self.battery_v = round(13.4 + (14.4 - 13.4) * 0.55 - load_w * 0.05 + self.rng.gauss(0, 0.02), 2)
        self.battery_v = max(12.2, self.battery_v)

    def _fuel_and_range(self, dt: float) -> None:
        cruise_factor = 1.0 + self.throttle * 1.6
        rate = 0.55 + (self.throttle * 7.5 + self.speed * 0.048 * cruise_factor)
        self.fuel_rate_lph = rate
        self.fuel_pct = max(0.0, self.fuel_pct - rate * dt / 3600.0 / TANK_LITRES * 100.0)

        if self.speed >= 6.0:
            cons_l100 = rate / self.speed * 100.0
            self.cons_avg += (cons_l100 - self.cons_avg) * (1.0 - math.exp(-dt / 120.0))
        fuel_l = self.fuel_pct / 100.0 * TANK_LITRES
        self._range_km = fuel_l / max(3.5, self.cons_avg) * 100.0
        self.odo_km += self.speed * dt / 3600.0
        self.trip_km += self.speed * dt / 3600.0

    def _intake_and_boost(self, amb: Ambient) -> None:
        self._iat_c = self._ambient_c + 6.0 + self.throttle * 16.0
        self.map_kpa = amb.baro + self.throttle * 58.0 * (1.0 - self.throttle * 0.3)
        self.boost_kpa = max(0.0, self.map_kpa - amb.baro)
        target_lambda = 1.02 - self.throttle * 0.34
        self.lambda_ = target_lambda + 0.03 * math.sin(self._temp_phase + self.t * 2.0)
        self._afr = self.lambda_ * 14.7

    def _steering(self, m: float) -> None:
        amp = 1.2 if self.speed < 1.0 else 0.0
        amp += 26.0 * (1.0 if (62.0 <= m < 66.0 or 155.0 <= m < 159.0 or 205.0 <= m < 209.0) else 0.0)
        self.steering_deg = round(amp * math.sin(self._steer_phase + self.t * (1.0 + self.speed * 0.004)), 2)

    def _gps(self, dt: float) -> None:
        if getattr(self, "_phone", None) and self._phone.gps_fresh():
            ph = self._phone
            self.lat = ph.gps_lat
            self.lng = ph.gps_lng
            self.alt = ph.gps_alt
            sat = ph.gps_satellites
            if sat > 0:
                self.sat_used = sat
            heading = ph.mag_heading if ph.compass_fresh() else ph.gps_heading
            if heading >= 0:
                self.heading = heading % 360.0
            if ph.gps_speed_ms > 0:
                self.speed = ph.gps_speed_ms * 3.6
            self._gps_accuracy = ph.gps_accuracy_m
            return
        if getattr(self, "_phone", None) and self._phone.compass_fresh():
            self.heading = self._phone.mag_heading % 360.0
        self.heading = (self.heading + 3.0 * math.sin(self.t / 21.0)) % 360.0
        ds = self.speed / 3.6 * dt
        rad = math.radians(self.heading)
        self.lat += math.cos(rad) * ds / DEG_LAT_M
        self.lng += math.sin(rad) * ds / DEG_LNG_M
        self.alt += 0.22 * math.sin(self.t / 14.0) + self.rng.gauss(0, 0.05)
        self.sat_used = min(14, 10 + int(2 * math.sin(self.t / 90.0)) + self.rng.randint(-1, 2))

    def _phone_speed(self, phone, dt: float) -> None:
        if phone is None:
            self._est_speed_ms = self.speed / 3.6
            return
        # Bleed gravity bias out of the accelerometer while at rest.
        if phone.accel_fresh():
            a_forward = phone.accel_forward * 9.81   # g -> m/s²
            if abs(phone.gps_speed_ms) < 1.0 and abs(phone.accel_forward) < 0.25:
                self._est_speed_ms = 0.0
            else:
                self._est_speed_ms += a_forward * dt
                self._est_speed_ms = max(0.0, self._est_speed_ms)
        if phone.gps_fresh():
            # Snap belief to the GPS ground speed (accurate), then let the
            # accelerometer bridge the gaps between fixes.
            if phone.gps_speed_ms > 0:
                self._est_speed_ms = phone.gps_speed_ms
            self.speed = phone.gps_speed_ms * 3.6
        elif phone.compass_fresh() or phone.accel_fresh():
            # No live GPS fix: dead-reckon from the accelerometer, drift back
            # toward the synthetic drive cycle within a few seconds.
            target_ms = self.speed / 3.6
            k = 1.0 - math.exp(-dt / 6.0)
            self._est_speed_ms += (target_ms - self._est_speed_ms) * (1.0 - k)
            self.speed = self._est_speed_ms * 3.6

    def payload(self, now_ms: int, baro: float) -> dict:
        sun = self._ambient_sun
        return {
            "ts": now_ms,
            "speedKph": round(self.speed, 1),
            "rpm": round(self.rpm),
            "gear": self.gear,
            "reverse": self.reverse,
            "steeringDeg": self.steering_deg,
            "turn": self.turn,
            "lights": self.lights,
            "highBeam": self.high_beam,
            "hazards": self.hazards,
            "parkingBrake": self.parking_brake,
            "nightMode": sun < 0.05,
            "volume": round(self.volume, 3),
            "fuelPct": round(self.fuel_pct, 2),
            "rangeKm": round(self._range_km, 1),
            "fuelRateLph": round(self.fuel_rate_lph, 2),
            "consumptionLPer100Km": round(self.fuel_rate_lph / max(3.0, self.speed) * 100.0, 1),
            "consumptionAvgLPer100Km": round(self.cons_avg, 1),
            "batteryCapacityKwh": BATTERY_CAPACITY_KWH,
            "batteryLevelKwh": round(BATTERY_CAPACITY_KWH * self.fuel_pct / 100.0, 2),
            "coolantC": round(self.coolant, 1),
            "oilC": round(self.oil, 1),
            "transmissionC": round(self.transmission, 1),
            "iatC": round(self._iat_c, 1),
            "ambientC": round(self._ambient_c, 1),
            "baroKpa": round(baro, 2),
            "mapKpa": round(self.map_kpa, 1),
            "boostKpa": round(self.boost_kpa, 1),
            "lambda": round(self.lambda_, 3),
            "afr": round(self._afr, 2),
            "batteryV": self.battery_v,
            "ambientLux": self.acc_lux,
            "dimmerPct": self.dimmer,
            "odometerKm": round(self.odo_km, 1),
            "odometerTripKm": round(self.trip_km, 2),
            "drivingStatus": 0,
            "gps": {
                "lat": round(self.lat, 6),
                "lng": round(self.lng, 6),
                "alt": round(self.alt, 1),
                "heading": round(self.heading, 1),
                "speedMs": round(self.speed / 3.6, 2),
                "accuracyM": round(getattr(self, "_gps_accuracy", 2.4), 1),
                "satellites": self.sat_used,
                "fixTs": now_ms,
            },
            "gnss": {
                "connected": True,
                "device": "/dev/ttyACM0",
                "baudRate": 38400,
                "fixQuality": "rtkFloat" if self.sat_used >= 11 else "gps",
                "fixMode": "3d",
                "satellitesUsed": self.sat_used,
                "satellitesVisible": min(20, self.sat_used + 5),
                "pdop": round(1.1 + 0.4 * abs(math.sin(self.t / 40.0)), 2),
                "hdop": round(0.6 + 0.25 * abs(math.sin(self.t / 40.0)), 2),
                "vdop": round(1.3 + 0.5 * abs(math.sin(self.t / 40.0)), 2),
                "receiverTime": now_ms,
                "timezone": "Europe/Berlin",
            },
            "can": ({"id": 0x21B, "data": [0xAA, 0x55, 0x01],
                     "bus": 0} if self._man_ticks % 60 == 0 else None),
        }


def main() -> None:
    args = build_parser().parse_args()
    if not args.json and not args.host:
        build_parser().error("--host is required (or use --json for offline output)")

    rng = random.Random(args.seed)
    amb = Ambient(rng)
    vehicle = Vehicle(rng)
    dt = 1.0 / args.hz

    phone = None
    if args.phone:
        from phone_sensors import (
            PhoneSensorState,
            adb_reverse,
            start_bridge_thread,
        )
        if not adb_reverse(args.phone_port):
            print("phone bridge needs adb reverse; see phone_sensors.py", file=sys.stderr)
            sys.exit(1)
        phone = PhoneSensorState()
        start_bridge_thread()
        print(
            f"phone sensors via USB: adb reverse tcp:{args.phone_port}\n"
            "open http://127.0.0.1:{port} on the phone and tap 'Start sensors'"
            .replace("{port}", str(args.phone_port)),
            file=sys.stderr,
        )

    sio = socketio.Client(reconnection=True, reconnection_attempts=0, reconnection_delay=1)

    if args.echo:

        @sio.on("telemetry:update")
        def on_update(payload):
            print("LIVI ->", json.dumps(payload))

    if args.once or args.json:
        sod = 9 * 3600.0 + 240.0     # ~09:04 local, to start in daylight
        vehicle.step(dt, amb, sod, phone)
        sod = (sod + dt * args.env_scale) % 86400.0
        payload = vehicle.payload(int(time.time() * 1000), amb.baro)
        if args.json:
            print(json.dumps(payload, indent=2))
            return
        sio.connect(f"http://{args.host}:{args.port}")
        sio.emit("telemetry:push", payload)
        sio.disconnect()
        return

    sio.connect(f"http://{args.host}:{args.port}")
    print(f"pushing to http://{args.host}:{args.port} at {args.hz} Hz", file=sys.stderr)

    sod = 9 * 3600.0 + 240.0
    try:
        while True:
            vehicle.step(dt, amb, sod, phone)
            sod = (sod + dt * args.env_scale) % 86400.0
            amb.update(sod, dt)
            payload = vehicle.payload(int(time.time() * 1000), amb.baro)
            if phone is not None and phone.gps_fresh():
                payload["gnss"]["connected"] = True
                payload["gnss"]["device"] = "phone-USB"
                payload["gnss"]["fixQuality"] = "gps"
                payload["gnss"]["satellitesUsed"] = max(
                    payload["gnss"]["satellitesUsed"], phone.gps_satellites
                ) if phone.gps_satellites else payload["gnss"]["satellitesUsed"]
            sio.emit("telemetry:push", payload)
            time.sleep(dt)
    except KeyboardInterrupt:
        sio.disconnect()
        print("stopped", file=sys.stderr)


if __name__ == "__main__":
    main()