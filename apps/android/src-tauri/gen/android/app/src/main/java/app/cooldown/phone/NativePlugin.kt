package app.cooldown.phone

import android.Manifest
import android.app.Activity
import android.appwidget.AppWidgetManager
import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Build
import android.provider.Settings
import android.util.Base64
import android.util.Log
import androidx.activity.result.ActivityResult
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.ViewCompat
import androidx.appcompat.app.AppCompatActivity
import androidx.core.app.NotificationManagerCompat
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.util.concurrent.Executors
import org.json.JSONArray
import org.json.JSONObject

@InvokeArg
class SyncAlarm {
  var id: String = ""
  var atMs: Long = 0
  var title: String = ""
  var body: String = ""
}

@InvokeArg
class SyncAlarmsArgs {
  /** Null (field missing) is an error, so a typo can never be read as "cancel everything". */
  var alarms: List<SyncAlarm>? = null
}

@InvokeArg
class WidgetArgs {
  lateinit var json: String
}

@InvokeArg
class ScheduleArgs {
  var enabled: Boolean = false
}

@InvokeArg
class WidgetConfigArgs {
  /** Snapshot ids to show; null (or missing) means every window. */
  var windows: List<String>? = null
}

@InvokeArg
class RawKeyArgs {
  /** Standard base64 of exactly 32 bytes. */
  var raw: String? = null
}

@InvokeArg
class BlobArgs {
  /** Standard base64 of a blob made by `wrapKey`. */
  var blob: String? = null
}

@InvokeArg
class MulticastArgs {
  var hold: Boolean = false
}

/**
 * The native pieces Tauri cannot do on Android: exact alarms (persisted across reboots), the
 * Keystore-wrapped identity key, the widget and its settings, the background sync schedule,
 * permissions, QR scanning and the Wi-Fi multicast lock.
 *
 * Threading: Tauri runs every command on the Android main thread. Commands that can take a while
 * (`wrapKey`, `unwrapKey`) work on a background thread and answer from there; `scanQr` answers when
 * the user finishes. A Rust caller must therefore not block the thread it needs for anything else
 * while waiting (use `run_mobile_plugin_async` or `spawn_blocking`).
 */
@TauriPlugin
class NativePlugin(private val activity: Activity) : Plugin(activity) {

  private val appContext: Context get() = activity.applicationContext
  private val keyExecutor = Executors.newSingleThreadExecutor()
  /** When the current runtime prompt was started (0 = none). A prompt older than a minute is forgotten. */
  @Volatile private var askingSince = 0L
  private var multicast: WifiManager.MulticastLock? = null

  // ---- permissions -------------------------------------------------------------------------

  private fun canScheduleExact(): Boolean = AlarmScheduler.canScheduleExact(appContext)

  /** Covers both the runtime permission (API 33+) and the user switching notifications off. */
  private fun canNotify(): Boolean = NotificationManagerCompat.from(appContext).areNotificationsEnabled()

  /** Below API 37 there is no such permission, so the answer is true. */
  private fun hasLocalNetwork(): Boolean =
    Build.VERSION.SDK_INT < LOCAL_NETWORK_API ||
      activity.checkSelfPermission(PERM_LOCAL_NETWORK) == PackageManager.PERMISSION_GRANTED

  private fun permissionState(): JSObject = JSObject().apply {
    put("notifications", canNotify())
    put("exactAlarms", canScheduleExact())
    put("localNetwork", hasLocalNetwork())
  }

  /**
   * `{ bottom }`: the navigation bar's height in CSS pixels (dp). With edge-to-edge the page runs under it,
   * and the WebView reports the status bar to CSS (`env(safe-area-inset-top)`) but not this (measured on
   * API 36: bottom 0px), so the bottom tabs would sit under the gesture handle.
   */
  @Command
  fun navigationInset(invoke: Invoke) {
    val insets = ViewCompat.getRootWindowInsets(activity.window.decorView)
    val px = insets?.getInsets(WindowInsetsCompat.Type.navigationBars())?.bottom ?: 0
    invoke.resolve(JSObject().apply { put("bottom", (px / activity.resources.displayMetrics.density).toDouble()) })
  }

  /** `{ notifications, exactAlarms, localNetwork }`: what is granted right now. */
  @Command
  fun permissions(invoke: Invoke) {
    invoke.resolve(permissionState())
  }

  /**
   * Starts asking for whatever is missing, in this order: notifications, then local network (API 37+),
   * then the exact-alarm Settings page. Answers at once with the state *before* any prompt is
   * answered: the system dialogs are asynchronous. When the user has answered the runtime prompts, the
   * exact-alarm page opens by itself if it is still needed. Call `permissions` again when the app
   * returns to the foreground to see the result. Calling this while a prompt is open does nothing new.
   */
  @Command
  fun askPermissions(invoke: Invoke) {
    val state = permissionState()
    val runtime = mutableListOf<String>()
    if (Build.VERSION.SDK_INT >= 33 && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
      runtime.add(Manifest.permission.POST_NOTIFICATIONS)
    }
    if (!hasLocalNetwork()) runtime.add(PERM_LOCAL_NETWORK)

    if (runtime.isNotEmpty()) {
      val now = System.currentTimeMillis()
      if (askingSince == 0L || now - askingSince > 60_000) {
        askingSince = now
        try {
          // The invoke is only used by Tauri to report a manifest mistake; it is answered below.
          handle!!.requestPermissions(invoke, runtime.toTypedArray(), "onRuntimePermissions")
        } catch (e: Exception) {
          askingSince = 0L
          Log.w(TAG, "Could not start the permission prompt: ${e.javaClass.simpleName}")
          openExactAlarmSettingsIfNeeded()
        }
      }
    } else {
      openExactAlarmSettingsIfNeeded()
    }
    invoke.resolve(state)
  }

  @PermissionCallback
  fun onRuntimePermissions(@Suppress("UNUSED_PARAMETER") invoke: Invoke) {
    askingSince = 0L
    openExactAlarmSettingsIfNeeded()
  }

  private fun openExactAlarmSettingsIfNeeded() {
    if (canScheduleExact() || Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return
    try {
      activity.startActivity(
        Intent(Settings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM, Uri.parse("package:${activity.packageName}"))
      )
    } catch (e: ActivityNotFoundException) {
      openSettingsPage()
    }
  }

  private fun openSettingsPage() {
    activity.startActivity(
      Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:${activity.packageName}"))
    )
  }

  // ---- alarms ------------------------------------------------------------------------------

  /**
   * Makes the scheduled set exactly `alarms`: unlisted ids are cancelled, the rest are (re)scheduled,
   * entries whose `atMs` is already past are skipped. The list is persisted for [BootReceiver].
   * `exact` is false when any alarm had to be inexact because the exact-alarm permission is missing.
   */
  @Command
  fun syncAlarms(invoke: Invoke) {
    val args = invoke.parseArgs(SyncAlarmsArgs::class.java)
    val list = args.alarms
    if (list == null) {
      invoke.reject("syncAlarms needs an \"alarms\" list.")
      return
    }
    // Last one wins when an id is listed twice; blank ids are ignored.
    val wanted = LinkedHashMap<String, StoredAlarm>()
    for (a in list) {
      if (a.id.isBlank()) continue
      wanted[a.id] = StoredAlarm(a.id, a.atMs, a.title, a.body)
    }
    val result = AlarmScheduler.sync(appContext, wanted.values.toList())
    invoke.resolve(JSObject().apply {
      put("scheduled", result.scheduled)
      put("exact", result.exact)
    })
  }

  // ---- identity key wrapping -----------------------------------------------------------------

  /** `{ raw: base64(32 bytes) } -> { blob: base64 }`. Creates the Keystore key on first use. */
  @Command
  fun wrapKey(invoke: Invoke) {
    val args = invoke.parseArgs(RawKeyArgs::class.java)
    keyExecutor.execute {
      var raw: ByteArray? = null
      try {
        raw = decode(args.raw, "raw")
        val blob = KeyWrap.wrap(raw)
        invoke.resolve(JSObject().apply { put("blob", Base64.encodeToString(blob, Base64.NO_WRAP)) })
      } catch (e: KeyWrapException) {
        invoke.reject(e.message)
      } catch (e: Exception) {
        invoke.reject("The key could not be protected (${e.javaClass.simpleName}).")
      } finally {
        raw?.fill(0)
      }
    }
  }

  /** `{ blob } -> { raw: base64(32 bytes) }`. Rejects, never crashes, on a wrong or corrupt blob. */
  @Command
  fun unwrapKey(invoke: Invoke) {
    val args = invoke.parseArgs(BlobArgs::class.java)
    keyExecutor.execute {
      var raw: ByteArray? = null
      try {
        val blob = decode(args.blob, "blob")
        raw = KeyWrap.unwrap(blob)
        invoke.resolve(JSObject().apply { put("raw", Base64.encodeToString(raw, Base64.NO_WRAP)) })
      } catch (e: KeyWrapException) {
        invoke.reject(e.message)
      } catch (e: Exception) {
        invoke.reject("The stored key could not be opened (${e.javaClass.simpleName}).")
      } finally {
        raw?.fill(0)
      }
    }
  }

  private fun decode(value: String?, name: String): ByteArray {
    if (value.isNullOrEmpty()) throw KeyWrapException("Missing \"$name\".")
    try {
      return Base64.decode(value, Base64.DEFAULT)
    } catch (e: IllegalArgumentException) {
      throw KeyWrapException("\"$name\" is not valid base64.")
    }
  }

  // ---- QR scanning ---------------------------------------------------------------------------

  /**
   * Opens the camera scanner. Resolves `{ text }` with the first QR code that starts with `CD1:`;
   * rejects "cancelled" if the user backs out, "That is not a Cooldown pairing code" for any other
   * QR, or a camera message (permission denied, no camera). Asks for the CAMERA permission itself.
   */
  @Command
  fun scanQr(invoke: Invoke) {
    ScanResultHolder.take() // forget anything left over
    startActivityForResult(invoke, Intent(activity, ScanActivity::class.java), "onScanResult")
  }

  @ActivityCallback
  fun onScanResult(invoke: Invoke, result: ActivityResult) {
    val text = ScanResultHolder.take()
    if (result.resultCode == Activity.RESULT_OK && text != null) {
      invoke.resolve(JSObject().apply { put("text", text) })
    } else {
      invoke.reject(result.data?.getStringExtra(ScanActivity.EXTRA_ERROR) ?: "cancelled")
    }
  }

  // ---- Wi-Fi multicast ---------------------------------------------------------------------

  /**
   * `{ hold: true|false } -> { held }`. Android drops most multicast packets to save battery; the
   * lock lets discovery probes get their replies. Not reference counted: holding twice is one hold,
   * and one release frees it. Also released when the activity is destroyed.
   */
  @Command
  fun multicastLock(invoke: Invoke) {
    val args = invoke.parseArgs(MulticastArgs::class.java)
    val held = synchronized(this) {
      val lock = multicast ?: appContext.getSystemService(WifiManager::class.java)
        .createMulticastLock("cooldown-discovery")
        .apply { setReferenceCounted(false) }
        .also { multicast = it }
      if (args.hold) {
        if (!lock.isHeld) lock.acquire()
      } else if (lock.isHeld) {
        lock.release()
      }
      lock.isHeld
    }
    invoke.resolve(JSObject().apply { put("held", held) })
  }

  override fun onDestroy(activity: AppCompatActivity) {
    synchronized(this) {
      multicast?.let { if (it.isHeld) it.release() }
    }
  }

  // ---- widget ------------------------------------------------------------------------------

  private fun canPinWidget(): Boolean {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return false
    return activity.getSystemService(AppWidgetManager::class.java)?.isRequestPinAppWidgetSupported == true
  }

  /** Ask the launcher to place the widget (Android shows its own "Add to home screen" sheet). */
  @Command
  fun pinWidget(invoke: Invoke) {
    val supported = canPinWidget()
    if (supported) {
      activity.getSystemService(AppWidgetManager::class.java)
        .requestPinAppWidget(ComponentName(activity, CooldownWidgetReceiver::class.java), null, null)
    }
    invoke.resolve(JSObject().apply { put("requested", supported) })
  }

  private fun widgetConfigState(): JSObject = JSObject().apply {
    val windows = WidgetConfig.get(appContext)
    put("windows", if (windows == null) JSONObject.NULL else JSONArray(windows))
    put("canPin", canPinWidget())
  }

  /** `{ windows: [snapshot id] | null, canPin }`: what the widget shows (null = every window). */
  @Command
  fun getWidgetConfig(invoke: Invoke) {
    invoke.resolve(widgetConfigState())
  }

  /** `{ windows }` -> same as [getWidgetConfig]. Redraws the widget. */
  @Command
  fun setWidgetConfig(invoke: Invoke) {
    val args = invoke.parseArgs(WidgetConfigArgs::class.java)
    WidgetConfig.set(appContext, args.windows)
    invoke.resolve(widgetConfigState())
  }

  /** Writes the widget file (empty text removes it) and redraws the widget. */
  @Command
  fun writeWidgetData(invoke: Invoke) {
    val args = invoke.parseArgs(WidgetArgs::class.java)
    WidgetData.write(appContext, args.json)
    invoke.resolve(JSObject().apply { put("bytes", args.json.length) })
  }

  // ---- background sync ---------------------------------------------------------------------

  /**
   * `{ enabled }`: keep the 15-minute background sync scheduled (a PC is paired) or cancel it. Safe
   * to call as often as needed; an existing schedule keeps its place.
   */
  @Command
  fun scheduleSync(invoke: Invoke) {
    val args = invoke.parseArgs(ScheduleArgs::class.java)
    if (args.enabled) SyncScheduler.schedule(appContext) else SyncScheduler.cancel(appContext)
    invoke.resolve(JSObject().apply { put("enabled", args.enabled) })
  }

  companion object {
    private const val TAG = "CooldownNative"
    private const val LOCAL_NETWORK_API = 37
    private const val PERM_LOCAL_NETWORK = "android.permission.ACCESS_LOCAL_NETWORK"
  }
}
