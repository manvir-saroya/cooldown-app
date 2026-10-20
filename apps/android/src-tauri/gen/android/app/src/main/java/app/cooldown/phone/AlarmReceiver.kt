package app.cooldown.phone

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log
import androidx.core.app.NotificationCompat

/**
 * Posts the "your limit is back" notification when an alarm fires, even with the app closed, and
 * forgets the alarm so a later reboot does not schedule it again.
 */
class AlarmReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    val id = intent.getStringExtra("id") ?: "alarm"
    val atMs = intent.getLongExtra("atMs", 0)
    try {
      notify(context, id, intent.getStringExtra("title"), intent.getStringExtra("body"))
    } catch (e: Exception) {
      // Never crash from a receiver: the worst case is a missing notification.
      Log.w("CooldownAlarms", "Could not post the notification: ${e.javaClass.simpleName}")
    }
    AlarmStore.removeFired(context, id, atMs)
  }

  private fun notify(context: Context, id: String, title: String?, body: String?) {
    val notifications = context.getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      notifications.createNotificationChannel(
        NotificationChannel(CHANNEL, "Limit resets", NotificationManager.IMPORTANCE_HIGH)
      )
    }
    val open = PendingIntent.getActivity(
      context, 0, Intent(context, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE
    )
    val notification = NotificationCompat.Builder(context, CHANNEL)
      .setSmallIcon(android.R.drawable.ic_lock_idle_alarm)
      .setContentTitle(title ?: "Your limit is back")
      .setContentText(body ?: "")
      .setPriority(NotificationCompat.PRIORITY_HIGH)
      .setCategory(NotificationCompat.CATEGORY_REMINDER)
      .setContentIntent(open)
      .setAutoCancel(true)
      .build()
    notifications.notify(id.hashCode(), notification)
  }

  companion object {
    const val CHANNEL = "resets"
  }
}
