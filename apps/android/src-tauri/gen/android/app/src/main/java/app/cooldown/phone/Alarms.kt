package app.cooldown.phone

import android.app.AlarmManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.util.Log
import org.json.JSONArray
import org.json.JSONObject

/** One scheduled "your limit is back" alarm, as the app asked for it. */
data class StoredAlarm(val id: String, val atMs: Long, val title: String, val body: String) {
  companion object {
    /** One alarm from its JSON form `{ id, atMs, title, body }`; null when it has no id. */
    fun fromJson(o: JSONObject): StoredAlarm? {
      val id = o.optString("id", "")
      return if (id.isEmpty()) null else StoredAlarm(id, o.optLong("atMs", 0), o.optString("title", ""), o.optString("body", ""))
    }
  }
}

/**
 * The set of alarms the app wants scheduled, kept in app-private SharedPreferences (excluded from
 * backup by data_extraction_rules.xml) so [BootReceiver] can put them back after a reboot.
 *
 * Every read-modify-write goes through [lock]; the file is written with `commit()` so a reboot right
 * after a sync cannot lose it. The list is tiny (a handful of alarms).
 */
object AlarmStore {
  private const val PREFS = "cooldown_alarms"
  private const val KEY = "alarms"
  private val lock = Any()

  fun <T> locked(block: () -> T): T = synchronized(lock, block)

  fun load(context: Context): List<StoredAlarm> = locked {
    val raw = prefs(context).getString(KEY, null) ?: return@locked emptyList()
    try {
      val array = JSONArray(raw)
      (0 until array.length()).mapNotNull { i -> array.optJSONObject(i)?.let(StoredAlarm::fromJson) }
    } catch (e: Exception) {
      Log.w(TAG, "Stored alarm list was unreadable and has been dropped")
      emptyList()
    }
  }

  fun save(context: Context, alarms: List<StoredAlarm>) = locked {
    val array = JSONArray()
    for (a in alarms) {
      array.put(
        JSONObject().put("id", a.id).put("atMs", a.atMs).put("title", a.title).put("body", a.body)
      )
    }
    prefs(context).edit().putString(KEY, array.toString()).commit()
    Unit
  }

  /**
   * Called when an alarm has fired: forget it, unless a newer sync moved the same id to a later time
   * (then the stored entry belongs to the next alarm, not the one that just rang).
   */
  fun removeFired(context: Context, id: String, firedAtMs: Long) = locked {
    val all = load(context)
    val kept = all.filterNot { it.id == id && (firedAtMs <= 0 || it.atMs <= firedAtMs) }
    if (kept.size != all.size) save(context, kept)
  }

  private fun prefs(context: Context) =
    context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

  private const val TAG = "CooldownAlarms"
}

/** Talks to [AlarmManager]. One PendingIntent per alarm id; the id is part of the intent's data URI. */
object AlarmScheduler {
  private const val TAG = "CooldownAlarms"
  const val ACTION_ALARM = "app.cooldown.phone.ALARM"

  fun canScheduleExact(context: Context): Boolean {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return true
    return context.getSystemService(AlarmManager::class.java).canScheduleExactAlarms()
  }

  /**
   * The intent that identifies one alarm. `Intent.filterEquals` (which PendingIntent identity uses)
   * ignores extras but compares action, data and component, so the id in the data URI gives every
   * alarm its own PendingIntent with no hash collisions.
   */
  private fun alarmIntent(context: Context, id: String): Intent =
    Intent(context, AlarmReceiver::class.java)
      .setAction(ACTION_ALARM)
      .setData(Uri.Builder().scheme("cooldown").authority("alarm").appendPath(id).build())

  /** Schedule (or replace) one alarm. Returns true when it is exact. Never throws. */
  fun schedule(context: Context, alarm: StoredAlarm): Boolean {
    val app = context.applicationContext
    val intent = alarmIntent(app, alarm.id)
      .putExtra("id", alarm.id)
      .putExtra("atMs", alarm.atMs)
      .putExtra("title", alarm.title)
      .putExtra("body", alarm.body)
    val pending = PendingIntent.getBroadcast(
      app, 0, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
    )
    val alarms = app.getSystemService(AlarmManager::class.java)
    if (canScheduleExact(app)) {
      try {
        alarms.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, alarm.atMs, pending)
        return true
      } catch (e: SecurityException) {
        // The permission was revoked between the check and the call: fall through to inexact.
        Log.w(TAG, "Exact alarm refused, scheduling inexact")
      }
    }
    // Without the exact-alarm permission it still rings, but Android may delay it.
    alarms.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, alarm.atMs, pending)
    return false
  }

  fun cancel(context: Context, id: String) {
    val app = context.applicationContext
    val pending = PendingIntent.getBroadcast(
      app, 0, alarmIntent(app, id), PendingIntent.FLAG_NO_CREATE or PendingIntent.FLAG_IMMUTABLE
    ) ?: return
    app.getSystemService(AlarmManager::class.java).cancel(pending)
    pending.cancel()
  }

  data class SyncResult(val scheduled: Int, val exact: Boolean)

  /**
   * Make the scheduled set exactly [wanted]: cancel what is no longer listed, (re)schedule the rest,
   * skip (and forget) anything already in the past. The stored list is updated to match.
   */
  fun sync(context: Context, wanted: List<StoredAlarm>, now: Long = System.currentTimeMillis()): SyncResult =
    AlarmStore.locked {
      val previous = AlarmStore.load(context)
      val future = wanted.filter { it.atMs > now }
      val keep = future.map { it.id }.toSet()
      for (old in previous) if (old.id !in keep) cancel(context, old.id)
      // A listed id whose time is already past must not leave an older alarm pending either.
      for (stale in wanted) if (stale.atMs <= now) cancel(context, stale.id)
      var exact = canScheduleExact(context)
      for (alarm in future) exact = schedule(context, alarm) && exact
      AlarmStore.save(context, future)
      SyncResult(future.size, exact)
    }

  /**
   * After a reboot, an app update or a clock change: schedule every stored alarm still in the future
   * again, and drop the ones that passed while the phone was off. Returns how many were scheduled.
   */
  fun rescheduleAll(context: Context, now: Long = System.currentTimeMillis()): Int = AlarmStore.locked {
    val stored = AlarmStore.load(context)
    val future = stored.filter { it.atMs > now }
    for (alarm in future) schedule(context, alarm)
    if (future.size != stored.size) AlarmStore.save(context, future)
    future.size
  }
}
