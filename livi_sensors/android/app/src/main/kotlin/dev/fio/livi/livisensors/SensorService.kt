package dev.fio.livi.livisensors

import android.annotation.SuppressLint
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import android.os.Looper
import androidx.core.app.ActivityCompat
import androidx.core.app.NotificationCompat
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import org.json.JSONObject

/**
 * Foreground service that bridges the phone's GPS/magnetometer/accelerometer to
 * the LIVI rig over the wired Android Auto cable — zero wireless.
 *
 * Wire protocol is identical to phone.html:
 *   ws://127.0.0.1:9123/ws   (rig sets `adb reverse tcp:9123 tcp:9123` over USB)
 *   {"sensor":"gps",   lat, lng, alt, accuracy, speed, heading, satellites}
 *   {"sensor":"mag",   heading}
 *   {"sensor":"accel", forward, lateral, up}   (device axes, x=right y=forward z=up, in g)
 *
 * A foreground service (with ongoing notification) keeps running while the screen
 * is locked and while Android Auto is in the foreground; Flutter's own lifecycle
 * does not — that is why all the heavy lifting is native Kotlin here.
 *
 * Auto-launch: BOOT_COMPLETED/MY_PACKAGE_REPLACED receiver, plus the rig's USB
 * attach hook (`adb reverse` + `am start-foreground-service`).
 */
class SensorService : Service(), SensorEventListener, LocationListener {

    companion object {
        private const val TAG = "LIVI.Sensors"
        const val ACTION_START = "dev.fio.livi.livisensors.START"
        const val ACTION_STOP = "dev.fio.livi.livisensors.STOP"
        const val EXTRA_SOURCE = "source" // "boot" | "adb" | "ui"
        private const val CHANNEL_ID = "livi_sensors"
        private const val NOTIF_ID = 1
        private const val BRIDGE_URL = "ws://127.0.0.1:9123/ws"
        private const val GPS_MIN_MS = 200L
        private const val GPS_MIN_DIST = 1.0f
        private const val MAG_INTERVAL_MS = 90L   // matches phone.html (>90)
        private const val ACCEL_INTERVAL_MS = 80L // matches phone.html (>80)
        private const val GPS_INTERVAL_MS = 250L

        @Volatile private var running = false
        @Volatile private var streaming = false
        @Volatile private var lastHeading = 0f
        @Volatile private var lastForward = 0f
        @Volatile private var lastLateral = 0f

        fun isRunning(): Boolean = running
        fun isStreaming(): Boolean = streaming
        fun lastValues(): Triple<Float, Float, Float> =
            Triple(lastHeading, lastForward, lastLateral)
    }

    private lateinit var sensorManager: SensorManager
    private lateinit var locationManager: LocationManager
    private var wsClient: WebSocketClient? = null
    private val executor = Executors.newSingleThreadScheduledExecutor()

    private var rotationSensor: Sensor? = null
    private var accelSensor: Sensor? = null
    private var lastMagSent = 0L
    private var lastAccelSent = 0L
    private var lastGpsSent = 0L

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        running = true
        sensorManager = getSystemService(SENSOR_SERVICE) as SensorManager
        locationManager = getSystemService(LOCATION_SERVICE) as LocationManager
        wsClient = WebSocketClient(BRIDGE_URL).also { it.connect() }
        startForeground()
        registerSensors()
        requestGps()
        executor.scheduleAtFixedRate({ updateNotification() }, 2, 5, TimeUnit.SECONDS)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                stopEverything()
                stopSelf()
                return START_NOT_STICKY
            }
            else -> {
                // onCreate() already ran if the service is live; guard against
                // double-registered sensor/location listeners on repeated starts.
                if (wsClient == null) onCreate()
            }
        }
        return START_STICKY
    }

    override fun onDestroy() {
        stopEverything()
        super.onDestroy()
    }

    private fun stopEverything() {
        running = false
        streaming = false
        try { sensorManager.unregisterListener(this) } catch (_: Exception) {}
        try { locationManager.removeUpdates(this) } catch (_: Exception) {}
        wsClient?.disconnect()
        wsClient = null
        executor.shutdownNow()
        stopForeground(STOP_FOREGROUND_REMOVE)
    }

    private fun startForeground() {
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        val channel = NotificationChannel(
            CHANNEL_ID, "LIVI sensors", NotificationManager.IMPORTANCE_LOW
        ).apply {
            description = "Streams GPS/magnetometer/accel to the LIVI rig over USB"
            setShowBadge(false)
        }
        nm.createNotificationChannel(channel)

        val contentIntent = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val notification: Notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle("LIVI Sensors")
            .setContentText("Streaming GPS/compass/accel to rig over USB")
            .setSmallIcon(android.R.drawable.ic_menu_compass)
            .setOngoing(true)
            .setContentIntent(contentIntent)
            .build()

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q
            && hasLocationPermission()
        ) {
            startForeground(
                NOTIF_ID, notification,
                android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_LOCATION
            )
        } else {
            startForeground(NOTIF_ID, notification)
        }
    }

    private fun hasLocationPermission(): Boolean =
        ActivityCompat.checkSelfPermission(this, android.Manifest.permission.ACCESS_FINE_LOCATION) ==
            PackageManager.PERMISSION_GRANTED

    @SuppressLint("MissingPermission")
    private fun registerSensors() {
        if (!hasLocationPermission()) return
        rotationSensor = sensorManager.getDefaultSensor(Sensor.TYPE_ROTATION_VECTOR)
        accelSensor = sensorManager.getDefaultSensor(Sensor.TYPE_ACCELEROMETER)
        sensorManager.registerListener(this, rotationSensor, SensorManager.SENSOR_DELAY_GAME)
        sensorManager.registerListener(this, accelSensor, SensorManager.SENSOR_DELAY_GAME)
    }

    @SuppressLint("MissingPermission")
    private fun requestGps() {
        if (!hasLocationPermission()) return
        try {
            locationManager.requestLocationUpdates(
                LocationManager.GPS_PROVIDER, GPS_MIN_MS, GPS_MIN_DIST,
                this, Looper.getMainLooper()
            )
        } catch (_: Exception) {}
    }

    override fun onSensorChanged(event: SensorEvent) {
        val now = System.currentTimeMillis()
        when (event.sensor.type) {
            Sensor.TYPE_ROTATION_VECTOR -> {
                if (now - lastMagSent < MAG_INTERVAL_MS) return
                lastMagSent = now
                val rot = FloatArray(9)
                SensorManager.getRotationMatrixFromVector(rot, event.values)
                val orient = FloatArray(3)
                SensorManager.getOrientation(rot, orient)
                // Same convention as phone.html: (360 - alpha) % 360
                val azDeg = Math.toDegrees(orient[0].toDouble()).toFloat()
                val hdg = ((360 - (azDeg % 360 + 360) % 360) % 360 + 360) % 360
                lastHeading = hdg
                if (streaming) {
                    wsClient?.send(JSONObject().put("sensor", "mag").put("heading", hdg).toString())
                }
            }
            Sensor.TYPE_ACCELEROMETER -> {
                if (now - lastAccelSent < ACCEL_INTERVAL_MS) return
                lastAccelSent = now
                val g = event.values
                // Device axes in g: x=right y=forward z=up (matches phone.html)
                lastForward = g[1] / SensorManager.GRAVITY_EARTH
                lastLateral = g[0] / SensorManager.GRAVITY_EARTH
                val up = g[2] / SensorManager.GRAVITY_EARTH
                streaming = true
                wsClient?.send(
                    JSONObject()
                        .put("sensor", "accel")
                        .put("forward", lastForward)
                        .put("lateral", lastLateral)
                        .put("up", up)
                        .toString()
                )
            }
        }
    }

    @SuppressLint("MissingPermission")
    override fun onLocationChanged(location: Location) {
        val now = System.currentTimeMillis()
        if (now - lastGpsSent < GPS_INTERVAL_MS) return
        lastGpsSent = now
        streaming = true
        wsClient?.send(
            JSONObject()
                .put("sensor", "gps")
                .put("lat", location.latitude)
                .put("lng", location.longitude)
                .put("alt", if (location.hasAltitude()) location.altitude else 0.0)
                .put("accuracy", location.accuracy.toDouble())
                .put("speed", location.speed.toDouble())
                .put("heading", if (location.hasBearing()) location.bearing.toDouble() else 0.0)
                .put("satellites", 0)
                .toString()
        )
    }

    private fun updateNotification() {
        val nm = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        val (heading, fwd, _) = lastValues()
        val text = if (streaming && wsClient?.isOpen() == true)
            "streaming · heading %.0f° · fwd %.2fg".format(heading, fwd)
        else
            "waiting for rig (adb reverse)"
        nm.notify(
            NOTIF_ID,
            NotificationCompat.Builder(this, CHANNEL_ID)
                .setContentTitle("LIVI Sensors")
                .setContentText(text)
                .setSmallIcon(android.R.drawable.ic_menu_compass)
                .setOngoing(true)
                .build()
        )
    }

    override fun onStatusChanged(provider: String?, status: Int, extras: Bundle?) {}
    override fun onProviderEnabled(provider: String) {}
    override fun onProviderDisabled(provider: String) {}
    override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) {}
}