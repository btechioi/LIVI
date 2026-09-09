#!/usr/bin/env python3
"""USB-only phone sensor bridge — zero wireless.

Serves phone.html on GET /, accepts WebSocket sensor frames on /ws, and
drives PhoneSensorState for the sim to consume.

Run standalone (or imported from simulate.py):
    uv run phone_sensors.py

Requires ADB on the rig (Arch: sudo pacman -S android-tools):
    adb reverse tcp:9123 tcp:9123   # phone→rig over USB only
"""
from __future__ import annotations

import argparse
import asyncio
import json
import logging
import os
import shutil
import subprocess
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

PORT = 9123
LOG = logging.getLogger("livi.sim.phonesensor")

try:
    import websockets.asyncio.server  # websockets >= 12
    from websockets.datastructures import Headers
    from websockets.http11 import Response
except ImportError:
    websockets = None  # type: ignore[assignment]
    Headers = None  # type: ignore[assignment]
    Response = None  # type: ignore[assignment]


# ── Shared sensor state (written by WS handler, read by sim loop) ──────────────

GPS_STALE_S = 2.5
COMPASS_STALE_S = 2.5
ACCEL_STALE_S = 1.0


@dataclass
class PhoneSensorState:
    _lock: threading.Lock = field(default_factory=threading.Lock, repr=False)

    # GPS
    gps_lat: float = 0.0
    gps_lng: float = 0.0
    gps_alt: float = 0.0
    gps_speed_ms: float = 0.0          # speed derived between consecutive fixes
    gps_accuracy_m: float = 99.0
    gps_satellites: int = 0
    gps_heading: float = 0.0
    gps_ts: float = 0.0

    # Magnetometer (compass)
    mag_heading: float = 0.0
    mag_ts: float = 0.0

    # Accelerometer (longitudinal / forward in g, positive = acceleration)
    accel_forward: float = 0.0
    accel_lateral: float = 0.0
    accel_ts: float = 0.0

    # Estimated speed from accelerometer dead-reckoning
    _est_speed_ms: float = 0.0

    # meta
    last_frame_ts: float = 0.0
    ws_active: bool = False

    def gps_fresh(self) -> bool:
        return self.ws_active and (time.monotonic() - self.gps_ts) < GPS_STALE_S

    def compass_fresh(self) -> bool:
        return self.ws_active and (time.monotonic() - self.mag_ts) < COMPASS_STALE_S

    def accel_fresh(self) -> bool:
        return self.ws_active and (time.monotonic() - self.accel_ts) < ACCEL_STALE_S

    def gps_speed_kph(self) -> float:
        return self.gps_speed_ms * 3.6 if self.gps_fresh() else 0.0

    def apply_frame(self, msg: dict) -> None:
        now = time.monotonic()
        with self._lock:
            self.last_frame_ts = now
            self.ws_active = True

            kind = msg.get("sensor")
            if kind == "gps":
                self.gps_lat = float(msg.get("lat", 0.0))
                self.gps_lng = float(msg.get("lng", 0.0))
                self.gps_alt = float(msg.get("alt", 0.0))
                self.gps_accuracy_m = float(msg.get("accuracy", 99.0))
                self.gps_satellites = int(msg.get("satellites", 0))
                self.gps_heading = float(msg.get("heading", 0.0))
                self.gps_speed_ms = float(msg.get("speed", 0.0))
                self.gps_ts = now

            elif kind == "mag":
                self.mag_heading = float(msg.get("heading", 0.0))
                self.mag_ts = now

            elif kind == "accel":
                self.accel_forward = float(msg.get("forward", 0.0))
                self.accel_lateral = float(msg.get("lateral", 0.0))
                self.accel_ts = now

            elif kind == "link":
                self.ws_active = bool(msg.get("up", False))


# ── WebSocket / HTTP server ─────────────────────────────────────────────────────

PHONE_HTML_PATH = Path(__file__).resolve().parent / "phone.html"


def _serve(state: PhoneSensorState) -> None:
    if websockets is None:
        raise RuntimeError(
            "websockets library required: add 'websockets>=12' to dependencies"
        )

    async def _handler(ws):
        LOG.info("phone connected")
        state.ws_active = True
        try:
            async for raw in ws:
                try:
                    msg = json.loads(raw)
                    if isinstance(msg, dict):
                        state.apply_frame(msg)
                except (json.JSONDecodeError, TypeError):
                    pass
        finally:
            state.ws_active = False
            LOG.info("phone disconnected")

    async def _process_request(connection, request):
        try:
            if request.path == "/":
                body = PHONE_HTML_PATH.read_bytes()
                return Response(
                    200,
                    "OK",
                    Headers([("Content-Type", "text/html; charset=utf-8"),
                             ("Content-Length", str(len(body)))]),
                    body,
                )
        except Exception:
            LOG.exception("process_request failed")
        return None  # let websockets handle the rest (/ws upgrade)

    async def _run():
        async with websockets.asyncio.server.serve(
            _handler,
            "127.0.0.1",
            PORT,
            process_request=_process_request,
        ):
            LOG.info("phone bridge listening on ws://127.0.0.1:%d", PORT)
            await asyncio.Future()  # run forever

    asyncio.run(_run())


def start_bridge_thread(state: PhoneSensorState) -> threading.Thread:
    """Start the bridge in a daemon thread; returns immediately."""
    t = threading.Thread(target=_serve, args=(state,), daemon=True)
    t.start()
    return t


# ── ADB reverse port forward ───────────────────────────────────────────────────

def adb_reverse(port: int = PORT) -> bool:
    """Set up adb reverse so phone's localhost:port reaches rig's 127.0.0.1:port."""
    adb = shutil.which("adb")
    if not adb:
        LOG.error(
            "adb not found. Install android-tools (Arch) and plug in the phone:\n"
            "  sudo pacman -S android-tools\n"
            "  adb devices   # phone must show as 'device' (not 'unauthorized')"
        )
        return False
    try:
        subprocess.check_call(
            [adb, "reverse", f"tcp:{port}", f"tcp:{port}"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        LOG.info("adb reverse tcp:%d tcp:%d set", port, port)
        return True
    except subprocess.CalledProcessError:
        LOG.error(
            "adb reverse failed. Ensure:\n"
            "  1. Phone is connected via USB\n"
            "  2. Developer mode + USB debugging are enabled\n"
            "  3. 'adb devices' shows the phone as 'device'"
        )
        return False


def adb_reverse_clear(port: int = PORT) -> None:
    adb = shutil.which("adb")
    if adb:
        subprocess.run(
            [adb, "reverse", "--remove", f"tcp:{port}"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )


# ── Standalone entry point ──────────────────────────────────────────────────────

def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--port", type=int, default=PORT)
    p.add_argument("--no-adb", action="store_true", help="skip adb reverse setup")
    a = p.parse_args()
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s")
    if not a.no_adb:
        if not adb_reverse(a.port):
            return
    state = PhoneSensorState()
    _serve(state)


if __name__ == "__main__":
    main()