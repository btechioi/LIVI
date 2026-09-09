#!/usr/bin/env python3
"""Live-test shim for LIVI dashboards — no car hardware required.

Pushes TelemetryPayload JSON to LIVI's Socket.IO server (port 4000).

Usage:
    python push_telemetry.py --host <livi-ip> --demo
    python push_telemetry.py --host <livi-ip>          # reads JSON-lines from stdin
    echo '{"speedKph":73,"rpm":2100}' | python push_telemetry.py --host x --stdin

Deps:  pip install "python-socketio[client]"
"""
from __future__ import annotations

import argparse
import json
import math
import sys
import time

import socketio


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--host", required=True, help="LIVI host IP")
    p.add_argument("--port", type=int, default=4000, help="LIVI telemetry port")
    p.add_argument("--demo", action="store_true", help="push a synthetic drive cycle")
    p.add_argument("--stdin", action="store_true", help="read JSON-lines from stdin")
    p.add_argument("--echo", action="store_true", help="print telemetry:update broadcasts")
    return p


def demo_cycle() -> None:
    """Rough urban drive: accelerate, cruise, brake, stop, repeat."""
    t = 0.0
    speed = 0.0
    while True:
        phase = (t % 60.0) / 60.0  # 60 s cycle
        if phase < 0.5:
            speed = min(60.0, speed + 0.6)          # accelerate
        elif phase < 0.75:
            speed = 60.0                             # cruise
        else:
            speed = max(0.0, speed - 1.2)            # brake

        rpm = speed * 90.0 + 700.0
        gear = "D" if speed > 1 else "N"
        yield {
            "speedKph": round(speed, 1),
            "rpm": round(rpm),
            "gear": gear,
            "coolantC": round(82 + 6 * math.sin(t / 30.0), 1),
            "oilC": round(90 + 4 * math.sin(t / 40.0), 1),
            "fuelPct": max(0, 62 - int(t / 600.0)),
            "turn": "left" if 20 < (t % 60) < 23 else ("right" if 35 < (t % 60) < 38 else "none"),
            "lights": not (10 < (t % 60) < 50),
            "parkingBrake": speed == 0 and (t % 60) > 57,
        }
        t += 1.0
        time.sleep(0.5)


def main() -> None:
    args = build_parser().parse_args()

    sio = socketio.Client()

    if args.echo:

        @sio.on("telemetry:update")
        def on_update(payload: object) -> None:
            print("LIVI ->", json.dumps(payload))

    url = f"http://{args.host}:{args.port}"
    print(f"connecting to {url} ...")
    sio.connect(url)

    if args.demo:
        for payload in demo_cycle():
            sio.emit("telemetry:push", payload)

    elif args.stdin or not sys.stdin.isatty():
        for line in sys.stdin:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            try:
                payload = json.loads(line)
            except json.JSONDecodeError as e:
                print(f"skip bad line: {e}", file=sys.stderr)
                continue
            sio.emit("telemetry:push", payload)

    else:
        print("no input mode selected; use --demo, --stdin, or pipe JSON-lines", file=sys.stderr)
        sys.exit(1)

    sio.disconnect()


if __name__ == "__main__":
    main()