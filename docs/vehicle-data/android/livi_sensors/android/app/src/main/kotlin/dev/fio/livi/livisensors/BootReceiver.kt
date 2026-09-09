package dev.fio.livi.livisensors

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build

/**
 * Auto-starts the sensor bridge on boot / app update so the phone streams the
 * moment the rig's USB `adb reverse` tunnel comes up — even if the user never
 * opens the app. The rig's udev hook additionally fires `am start-foreground-service`.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_BOOT_COMPLETED,
            Intent.ACTION_MY_PACKAGE_REPLACED -> {
                val svc = Intent(context, SensorService::class.java)
                    .putExtra(SensorService.EXTRA_SOURCE, "boot")
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    context.startForegroundService(svc)
                } else {
                    context.startService(svc)
                }
            }
        }
    }
}