package app.cooldown.phone

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log

/**
 * Puts the persisted alarms back when Android has forgotten them:
 *
 * - `BOOT_COMPLETED`: a reboot clears every alarm. (Runs after the first unlock: the alarm list lives
 *   in credential-protected storage, so there is deliberately no direct-boot handling.)
 * - `MY_PACKAGE_REPLACED`: an app update clears the app's alarms.
 * - `TIME_SET`: the clock changed; scheduling again is idempotent and keeps the times honest.
 * - `SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED`: the user granted (or revoked) the exact-alarm
 *   permission in Settings; granted means alarms that were inexact can now be exact.
 *
 * `TIMEZONE_CHANGED` is not handled on purpose: alarm times are absolute epoch milliseconds, so a
 * time zone change does not move them.
 */
class BootReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    when (intent.action) {
      Intent.ACTION_BOOT_COMPLETED,
      Intent.ACTION_MY_PACKAGE_REPLACED,
      Intent.ACTION_TIME_CHANGED,
      ACTION_EXACT_PERMISSION_CHANGED -> {
        val n = try {
          AlarmScheduler.rescheduleAll(context)
        } catch (e: Exception) {
          Log.w("CooldownAlarms", "Rescheduling failed: ${e.javaClass.simpleName}")
          -1
        }
        Log.i("CooldownAlarms", "${intent.action}: $n alarm(s) scheduled again")
      }
    }
  }

  companion object {
    // AlarmManager.ACTION_SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED (API 31), spelled out so the
    // class loads on older releases too.
    const val ACTION_EXACT_PERMISSION_CHANGED =
      "android.app.action.SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED"
  }
}
