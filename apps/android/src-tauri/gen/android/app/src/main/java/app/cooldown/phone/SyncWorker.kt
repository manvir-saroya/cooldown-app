package app.cooldown.phone

import android.content.Context
import android.net.wifi.WifiManager
import android.util.Base64
import android.util.Log
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.NetworkType
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import java.io.File
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject

/**
 * The door into the Rust library for the background sync (`jni_entry.rs`). Nothing here starts the
 * Tauri app: the library is only loaded, and one function is called.
 */
object SyncBridge {
  init {
    System.loadLibrary("cooldown_android_lib")
  }

  /**
   * One sync round with the paired PC, for the phone whose files are in [dataDir] and whose private
   * key is [key] (32 bytes; the caller wipes it afterwards). Returns a JSON report
   * `{ status, message, alarms, widget }` and never throws.
   */
  @JvmStatic external fun runSyncRound(dataDir: String, key: ByteArray): String
}

/** What a round hands back: see `Report` in `background.rs`. */
class SyncReport(val status: String, val message: String?, val alarms: List<StoredAlarm>?, val widget: String?) {
  companion object {
    fun parse(json: String): SyncReport {
      val o = JSONObject(json)
      val list = o.optJSONArray("alarms")?.let { array ->
        (0 until array.length()).mapNotNull { i -> array.optJSONObject(i)?.let(StoredAlarm::fromJson) }
      }
      return SyncReport(
        o.optString("status", "failed"),
        if (o.isNull("message")) null else o.optString("message"),
        list,
        if (o.isNull("widget")) null else o.optString("widget")
      )
    }
  }
}

/**
 * Runs every ~15 minutes while a PC is paired and the phone has a network (WorkManager's minimum
 * period). Reconnects to the PC, takes its newest sync, re-sets the alarms from it, refreshes the
 * widget and disconnects. No foreground service: it is a short job, and everything it needs is on
 * disk. Alarms and the widget do not depend on it; it only keeps them current.
 */
class SyncWorker(context: Context, params: WorkerParameters) : CoroutineWorker(context, params) {
  override suspend fun doWork(): Result = withContext(Dispatchers.IO) {
    val report = BackgroundSync.run(applicationContext)
    Log.i(TAG, "round ${report.status}${report.message?.let { ": $it" } ?: ""} (attempt ${runAttemptCount + 1})")
    when (report.status) {
      "synced" -> Result.success()
      "no_pc" -> {
        // Nobody to sync with: stop being scheduled until a PC is paired again.
        SyncScheduler.cancel(applicationContext)
        Result.success()
      }
      // Not reachable (away from home, PC asleep), the app is starting, or something broke: try once
      // more after the backoff, then leave it to the next period instead of hammering a dead network.
      else -> if (runAttemptCount + 1 < MAX_ATTEMPTS) Result.retry() else Result.success()
    }
  }

  companion object {
    private const val TAG = "CooldownSync"
    private const val MAX_ATTEMPTS = 2
  }
}

/** Keeps the periodic job scheduled exactly while a PC is paired. */
object SyncScheduler {
  const val UNIQUE_NAME = "cooldown-background-sync"
  private const val PERIOD_MINUTES = 15L
  private const val BACKOFF_MINUTES = 1L

  fun schedule(context: Context) {
    val request = PeriodicWorkRequestBuilder<SyncWorker>(PERIOD_MINUTES, TimeUnit.MINUTES)
      .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
      .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, BACKOFF_MINUTES, TimeUnit.MINUTES)
      // The app is open (and syncing by itself) when this is first scheduled, right after pairing.
      .setInitialDelay(PERIOD_MINUTES, TimeUnit.MINUTES)
      .build()
    // KEEP: opening the app again must not push the next run back.
    WorkManager.getInstance(context.applicationContext)
      .enqueueUniquePeriodicWork(UNIQUE_NAME, ExistingPeriodicWorkPolicy.KEEP, request)
  }

  fun cancel(context: Context) {
    WorkManager.getInstance(context.applicationContext).cancelUniqueWork(UNIQUE_NAME)
  }
}

/** One round and the applying of its result. Plain Kotlin so the Rust side stays free of Android. */
object BackgroundSync {
  private const val TAG = "CooldownSync"
  private const val WRAPPED_KEY_FILE = "phone-identity.wrapped"

  /** Never throws: a failure becomes a report with status "failed". */
  fun run(context: Context): SyncReport {
    val app = context.applicationContext
    val dataDir = app.dataDir
    val wrapped = File(dataDir, WRAPPED_KEY_FILE)
    // No key file: the app has never been opened, so there is nothing paired.
    if (!wrapped.exists()) return SyncReport("no_pc", "The app has not been set up.", emptyList(), "")
    var key: ByteArray? = null
    // Android drops multicast packets unless this is held: the discovery fallback needs it.
    val lock = try {
      app.getSystemService(WifiManager::class.java)?.createMulticastLock("cooldown-background")?.apply {
        setReferenceCounted(false)
        acquire()
      }
    } catch (e: Exception) {
      null
    }
    return try {
      key = KeyWrap.unwrap(Base64.decode(wrapped.readText().trim(), Base64.DEFAULT))
      val report = SyncReport.parse(SyncBridge.runSyncRound(dataDir.absolutePath, key))
      apply(app, report)
      report
    } catch (e: Throwable) {
      // The message is the exception's class only: nothing from the key path may reach the log.
      Log.w(TAG, "round failed: ${e.javaClass.simpleName}")
      SyncReport("failed", e.javaClass.simpleName, null, null)
    } finally {
      key?.fill(0)
      try {
        lock?.let { if (it.isHeld) it.release() }
      } catch (e: Exception) {
        // Nothing to do.
      }
    }
  }

  /** Alarms and widget from what the round found. `null` means "the phone knows nothing": leave them. */
  private fun apply(context: Context, report: SyncReport) {
    report.alarms?.let { AlarmScheduler.sync(context, it) }
    val widget = report.widget
    if (widget != null) WidgetData.write(context, widget)
    else WidgetData.refresh(context)
  }
}
