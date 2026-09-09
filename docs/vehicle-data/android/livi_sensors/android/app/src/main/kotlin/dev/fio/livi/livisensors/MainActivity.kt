package dev.fio.livi.livisensors

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {

    private val chName = "dev.fio.livi.livisensors/control"

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, chName)
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "start" -> {
                        ensurePermissions()
                        startService(Intent(this, SensorService::class.java)
                            .putExtra(SensorService.EXTRA_SOURCE, "ui"))
                        result.success(true)
                    }
                    "stop" -> {
                        stopService(Intent(this, SensorService::class.java))
                        result.success(true)
                    }
                    "status" -> {
                        result.success(hashMapOf<String, Any>(
                            "running" to SensorService.isRunning(),
                            "streaming" to SensorService.isStreaming(),
                            "heading" to SensorService.lastValues().first.toDouble(),
                            "forward" to SensorService.lastValues().second.toDouble(),
                            "lateral" to SensorService.lastValues().third.toDouble()
                        ))
                    }
                    else -> result.notImplemented()
                }
            }
    }

    private fun ensurePermissions() {
        val perms = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU)
            arrayOf(
                Manifest.permission.ACCESS_FINE_LOCATION,
                Manifest.permission.ACCESS_COARSE_LOCATION,
                Manifest.permission.POST_NOTIFICATIONS
            )
        else
            arrayOf(
                Manifest.permission.ACCESS_FINE_LOCATION,
                Manifest.permission.ACCESS_COARSE_LOCATION
            )
        val missing = perms.filter {
            ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED
        }.toTypedArray()
        if (missing.isNotEmpty()) {
            ActivityCompat.requestPermissions(this, missing, 1001)
        }
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == 1001) {
            startService(Intent(this, SensorService::class.java)
                .putExtra(SensorService.EXTRA_SOURCE, "ui"))
        }
    }
}