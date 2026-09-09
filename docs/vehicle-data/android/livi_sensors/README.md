# LIVI Sensors (Flutter) — phone-side bridge over USB

Native Android **foreground service** that streams the phone's GPS, magnetometer
(compass) and accelerometer to the LIVI rig over the *wired* cable the phone is
already connected on — zero wireless.

A foreground service (with an ongoing notification) is the one component that
keeps running while the screen is locked **and** while Android Auto is in the
foreground, which a Flutter-only isolate cannot guarantee. That is why the app is
Flutter for the control UI + notification, and the streaming itself is native
Kotlin (`SensorService`).

## Wire protocol (identical to `phone.html`)

The rig runs the bridge at `ws://127.0.0.1:9123/ws` and sets
`adb reverse tcp:9123 tcp:9123` so the phone reaches it over USB. Frames:

```json
{"sensor":"gps",  "lat":52.52,"lng":13.40,"alt":0,"accuracy":3.5,
 "speed":13.9,"heading":90.6,"satellites":0}
{"sensor":"mag",  "heading":90.6}
{"sensor":"accel","forward":0.02,"lateral":-0.01,"up":0.99}   // device axes, g
```

Compass heading uses the same `(360 - alpha) % 360` convention as `phone.html`.

## Build

On the rig (or any machine with Flutter + Android SDK):

```bash
cd docs/vehicle-data/android/livi_sensors

# First build: Flutter auto-generates android/gradlew and local.properties.
# (If your Flutter version complains about a missing wrapper, copy gradlew/
#  gradle-wrapper.jar from any other Flutter android/ project into android/.)
flutter pub get
flutter build apk --debug

# Install on the phone over USB:
adb install -r build/app/outputs/flutter-apk/app-debug.apk

# Grant permissions and exercise the auto-start path:
adb shell pm grant dev.fio.livi.livisensors android.permission.ACCESS_FINE_LOCATION
adb shell pm grant dev.fio.livi.livisensors android.permission.ACCESS_COARSE_LOCATION
adb shell pm grant dev.fio.livi.livisensors android.permission.POST_NOTIFICATIONS
adb shell am start-foreground-service -n dev.fio.livi.livisensors/.SensorService \
    --es source adb
```

> `build.gradle` targets compileSdk/targetSdk 34 and minSdk 26. If your Android SDK
> is newer, keep these as-is (they still build); bump only if a plugin demands it.

## How it works

| Piece | File | Role |
|---|---|---|
| `SensorService` | `android/.../SensorService.kt` | Foreground service: GPS via `LocationManager`, compass via `TYPE_ROTATION_VECTOR` (fused mag+gyro), accel via `TYPE_ACCELEROMETER`; streams over OkHttp WebSocket to the rig, auto-reconnects on backoff |
| `WebSocketClient` | `android/.../WebSocketClient.kt` | Reconnecting WS; the tunnel only exists when the phone is plugged into the rig |
| `BootReceiver` | `android/.../BootReceiver.kt` | Auto-starts the FGS on boot / app update so it streams the moment the USB link is up |
| `MainActivity` | `android/.../MainActivity.kt` | Flutter host + control channel + one-time runtime permission flow |
| `lib/main.dart` | `lib/main.dart` | Tiny status screen (start/stop, heading/accel readout) |

## Auto-launch on connection to the rig

Two independent triggers, either one is enough:

1. **Phone cold-boot**: `BootReceiver` starts the FGS at boot; the WS reconnect
   loop grabs the tunnel as soon as the rig sets `adb reverse`.
2. **rig attach hook** (kiosk): the installer drops `/etc/udev/rules.d/89-LIVI-phone.rules`
   + `/usr/local/lib/livi/livi-phone-attach.sh`; on phone plug-in the rig runs
   `adb reverse tcp:9123 tcp:9123` and `am start-foreground-service` over the USB
   cable. Screen may be locked — the FGS keeps running regardless.

## Battery / OEM notes

- Add LIVI Sensors to the phone's battery *unrestricted* list (EMUI/HyperOS/MIUI
  kill background services aggressively).
- The notification is `IMPORTANCE_LOW` and ongoing; clear it only via **Stop** in
  the app or `adb shell am stop-service ...` — the service restarts on next boot.