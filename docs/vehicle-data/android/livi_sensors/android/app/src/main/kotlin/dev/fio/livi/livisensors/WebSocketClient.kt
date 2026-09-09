package dev.fio.livi.livisensors

import android.os.Handler
import android.os.Looper
import android.util.Log
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener

/**
 * OkHttp WebSocket client with automatic reconnect.
 *
 * The bridge lives on 127.0.0.1 over the rig's `adb reverse tcp:9123 tcp:9123`
 * USB tunnel, so it is unreachable until the phone is plugged into the LIVI rig.
 * Reconnect-with-backoff handles "rig not attached yet" at every launch and
 * re-establishes the stream the moment the tunnel appears.
 */
class WebSocketClient(private val url: String) {

    interface Listener {
        fun onOpen()
        fun onClose(reason: String)
    }

    private val connected = AtomicBoolean(false)
    private val wsRef = AtomicReference<WebSocket?>(null)
    private val listenerRef = AtomicReference<Listener?>(null)
    private val reconnectDelayMs = AtomicReference(2000L)
    private val mainHandler = Handler(Looper.getMainLooper())

    private val client = OkHttpClient.Builder()
        .pingInterval(20, TimeUnit.SECONDS)
        .connectTimeout(2, TimeUnit.SECONDS)
        .retryOnConnectionFailure(true)
        .build()

    fun connect(listener: Listener? = null) {
        listenerRef.set(listener)
        openSocket()
    }

    private fun openSocket() {
        wsRef.get()?.cancel()
        val request = Request.Builder().url(url).build()
        val ws = client.newWebSocket(request, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                connected.set(true)
                reconnectDelayMs.set(2000L)
                mainHandler.post { listenerRef.get()?.onOpen() }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                Log.d("LIVI.WS", "ws failure: ${t.message}")
                handleDisconnect()
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                handleDisconnect()
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                // the sim bridge sends nothing back; ignore
            }
        })
        wsRef.set(ws)
    }

    private fun handleDisconnect() {
        connected.set(false)
        mainHandler.post { listenerRef.get()?.onClose("disconnected") }
        scheduleReconnect()
    }

    @Synchronized
    private fun scheduleReconnect() {
        if (reconnectThread?.isAlive == true) return
        reconnectThread = Thread {
            val delay = reconnectDelayMs.get()
            try {
                TimeUnit.MILLISECONDS.sleep(delay)
            } catch (_: InterruptedException) {
                return@Thread
            }
            if (!connected.get()) {
                if (reconnectDelayMs.get() < 30_000L) {
                    reconnectDelayMs.set(reconnectDelayMs.get() * 2)
                }
                openSocket()
            }
        }.also { it.isDaemon = true; it.start() }
    }

    @Volatile private var reconnectThread: Thread? = null

    fun isOpen(): Boolean = connected.get()

    fun send(text: String) {
        if (connected.get()) wsRef.get()?.send(text)
    }

    fun disconnect() {
        connected.set(false)
        reconnectThread?.interrupt()
        reconnectThread = null
        wsRef.get()?.cancel()
        wsRef.set(null)
    }
}