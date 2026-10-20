package app.cooldown.phone

import android.content.Intent
import android.content.Context
import android.text.format.DateUtils
import android.util.Log
import androidx.compose.runtime.remember
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.glance.GlanceId
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.longPreferencesKey
import androidx.glance.GlanceModifier
import androidx.glance.currentState
import androidx.glance.action.clickable
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.GlanceAppWidgetReceiver
import androidx.glance.appwidget.action.actionStartActivity
import androidx.glance.appwidget.lazy.LazyColumn
import androidx.glance.appwidget.lazy.items
import androidx.glance.appwidget.GlanceAppWidgetManager
import androidx.glance.appwidget.provideContent
import androidx.glance.appwidget.state.updateAppWidgetState
import androidx.glance.state.PreferencesGlanceStateDefinition
import androidx.glance.background
import androidx.glance.color.ColorProvider
import androidx.glance.layout.Column
import androidx.glance.layout.Row
import androidx.glance.layout.Spacer
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.fillMaxWidth
import androidx.glance.layout.height
import androidx.glance.layout.padding
import androidx.glance.text.FontWeight
import androidx.glance.text.Text
import androidx.glance.text.TextStyle
import java.io.File
import java.util.Calendar
import java.util.Locale
import java.util.TimeZone
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject

/** One window on the widget: its name, the figure, and a smaller line saying when it resets. */
data class WidgetLine(val label: String, val value: String, val sub: String)

/** What the widget shows: the chosen windows and when the data is from, or a plain message. */
data class WidgetModel(val lines: List<WidgetLine>, val asOf: String?, val message: String?)

/** One window the user can pick for the widget: its snapshot id and the name to show in a list. */
data class WidgetChoice(val id: String, val label: String)

/**
 * Home-screen widget: the limit windows the user chose (all of them by default, in the order the
 * app lists them), from the JSON the app or the background sync last wrote to `files/snapshots.json`.
 * It says when the data came from the PC, so stale data is plain to see, and scrolls when there are
 * more windows than fit. Tapping it opens the app.
 */
class CooldownWidget : GlanceAppWidget() {

  /**
   * One number, bumped by [WidgetData.refresh] whenever the file or the choice of windows changed.
   * Glance keeps a widget's composition alive and only redraws it when something it reads changes,
   * so the file is read inside the composition, keyed on this stamp, not once up front.
   */
  override val stateDefinition = PreferencesGlanceStateDefinition

  override suspend fun provideGlance(context: Context, id: GlanceId) {
    val openApp = Intent(context, MainActivity::class.java)
    provideContent {
      val stamp = currentState<Preferences>()[REFRESH_STAMP]
      val model = remember(stamp) { WidgetData.read(context) }
      val primary = ColorProvider(day = Color(0xFF14212B), night = Color(0xFFEAF1F6))
      val secondary = ColorProvider(day = Color(0xFF52626E), night = Color(0xFFA9B8C3))
      Column(
        modifier = GlanceModifier
          .fillMaxSize()
          .background(ColorProvider(day = Color(0xFFF4F8FB), night = Color(0xFF16222B)))
          .padding(12.dp)
          .clickable(actionStartActivity(openApp))
      ) {
        Row(modifier = GlanceModifier.fillMaxWidth()) {
          Text(
            "Cooldown",
            modifier = GlanceModifier.defaultWeight(),
            style = TextStyle(fontWeight = FontWeight.Bold, fontSize = 15.sp, color = primary)
          )
          if (model.asOf != null) {
            Text("as of ${model.asOf}", maxLines = 1, style = TextStyle(fontSize = 11.sp, color = secondary))
          }
        }
        Spacer(modifier = GlanceModifier.height(6.dp))
        if (model.message != null) {
          Text(model.message, style = TextStyle(fontSize = 13.sp, color = primary))
        } else {
          LazyColumn(modifier = GlanceModifier.fillMaxWidth().defaultWeight()) {
            items(model.lines) { line ->
              Column(modifier = GlanceModifier.fillMaxWidth().padding(bottom = 4.dp).clickable(actionStartActivity(openApp))) {
                Row(modifier = GlanceModifier.fillMaxWidth()) {
                  Text(
                    line.label,
                    modifier = GlanceModifier.defaultWeight(),
                    maxLines = 1,
                    style = TextStyle(fontWeight = FontWeight.Medium, fontSize = 13.sp, color = primary)
                  )
                  Text(line.value, maxLines = 1, style = TextStyle(fontWeight = FontWeight.Medium, fontSize = 13.sp, color = primary))
                }
                if (line.sub.isNotEmpty()) {
                  Text(line.sub, maxLines = 1, style = TextStyle(fontSize = 11.sp, color = secondary))
                }
              }
            }
          }
        }
      }
    }
  }

  companion object {
    const val DATA_FILE = "snapshots.json"
    val REFRESH_STAMP = longPreferencesKey("refresh-stamp")
  }
}

/**
 * Which windows the widget shows, chosen in the app's settings or when the widget is added. Kept in
 * app-private SharedPreferences (excluded from backup) as a JSON array of snapshot ids; absent means
 * every window, which is the default and also covers windows the PC adds later. One choice for all
 * Cooldown widgets on the home screen.
 */
object WidgetConfig {
  private const val PREFS = "cooldown_widget"
  private const val KEY = "windows"

  /** The chosen snapshot ids, or null for every window. */
  fun get(context: Context): List<String>? {
    val raw = prefs(context).getString(KEY, null) ?: return null
    return try {
      val array = JSONArray(raw)
      val ids = (0 until array.length()).mapNotNull { array.optString(it).takeIf { id -> id.isNotEmpty() } }
      // A damaged or empty choice is no choice: show everything rather than a blank widget.
      ids.ifEmpty { null }
    } catch (e: Exception) {
      null
    }
  }

  /** Stores the choice (null = every window) and redraws the widget at once. */
  fun set(context: Context, windows: List<String>?) {
    val editor = prefs(context).edit()
    if (windows == null) editor.remove(KEY) else editor.putString(KEY, JSONArray(windows).toString())
    editor.commit()
    WidgetData.refresh(context)
  }

  private fun prefs(context: Context) =
    context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}

/** Reads and formats `snapshots.json`. Pure Kotlin/Android APIs only (minSdk 24 has no java.time). */
object WidgetData {
  private const val TAG = "CooldownWidget"

  /**
   * Replaces the widget file with [json] and redraws the widget. Written beside the target and
   * renamed, so the widget never reads half a file. Empty [json] removes the file: nothing is paired,
   * and the widget goes back to asking for a pairing.
   */
  fun write(context: Context, json: String) {
    val dir = context.filesDir
    val target = File(dir, CooldownWidget.DATA_FILE)
    if (json.isEmpty()) {
      target.delete()
    } else {
      val temp = File(dir, CooldownWidget.DATA_FILE + ".tmp")
      temp.writeText(json)
      if (!temp.renameTo(target)) {
        target.writeText(json)
        temp.delete()
      }
    }
    refresh(context)
  }

  /** Redraw every Cooldown widget from the file as it is (also moves "resets 14:05" to "reset"). */
  fun refresh(context: Context) {
    val app = context.applicationContext
    CoroutineScope(Dispatchers.Default).launch {
      try {
        val widget = CooldownWidget()
        for (id in GlanceAppWidgetManager(app).getGlanceIds(CooldownWidget::class.java)) {
          // A new stamp is what makes a widget that is already on screen read the file again.
          updateAppWidgetState(app, id) { it[CooldownWidget.REFRESH_STAMP] = System.nanoTime() }
          widget.update(app, id)
        }
      } catch (e: Exception) {
        Log.w(TAG, "widget not redrawn: ${e.javaClass.simpleName}")
      }
    }
  }

  /** Every window in the file, in the order the app lists them, for the list of choices. */
  fun choices(context: Context): List<WidgetChoice> {
    val file = File(context.filesDir, CooldownWidget.DATA_FILE)
    if (!file.exists()) return emptyList()
    return try {
      val snapshots = JSONObject(file.readText()).getJSONArray("snapshots")
      val all = (0 until snapshots.length()).map { snapshots.getJSONObject(it) }
      all.map { s ->
        val provider = providerName(s.optString("provider", "?"))
        val title = windowTitle(s.optString("provider", ""), s.optString("window", ""))
        // Two accounts with the same window need telling apart.
        val twins = all.count { it.optString("provider") == s.optString("provider") && it.optString("window") == s.optString("window") }
        val account = s.optString("account", "")
        val suffix = if (twins > 1 && account.isNotEmpty()) " ($account)" else ""
        WidgetChoice(s.getString("id"), "$provider · $title$suffix")
      }
    } catch (e: Exception) {
      emptyList()
    }
  }

  fun read(context: Context, now: Long = System.currentTimeMillis()): WidgetModel {
    val file = File(context.filesDir, CooldownWidget.DATA_FILE)
    if (!file.exists()) return WidgetModel(emptyList(), null, "No data yet. Pair with your PC.")
    return try {
      val payload = JSONObject(file.readText())
      val snapshots = payload.getJSONArray("snapshots")
      // When the PC's data arrived; older files without it fall back to when the file was written.
      val synced = parseIso(payload.optString("syncedAt", "")) ?: parseIso(payload.optString("generatedAt", "")) ?: file.lastModified()
      val chosen = WidgetConfig.get(context)?.toSet()
      val lines = (0 until snapshots.length())
        .map { snapshots.getJSONObject(it) }
        .filter { chosen == null || it.optString("id") in chosen }
        .map { line(context, it, now) }
      WidgetModel(
        lines,
        formatTime(context, synced),
        when {
          lines.isNotEmpty() -> null
          snapshots.length() == 0 -> "No limits are being tracked yet."
          else -> "None of the windows you chose are on your PC. Change them in Cooldown's settings."
        }
      )
    } catch (e: Exception) {
      WidgetModel(emptyList(), null, "Could not read the synced data.")
    }
  }

  private fun providerName(provider: String) = when (provider) {
    "claude" -> "Claude"
    "codex" -> "Codex"
    "openrouter" -> "OpenRouter"
    "deepseek" -> "DeepSeek"
    "xai" -> "xAI"
    else -> provider.replaceFirstChar { it.titlecase(Locale.getDefault()) }
  }

  /** The long name the app uses for a window ("5-hour session", "Credit balance"). */
  private fun windowTitle(provider: String, window: String) = when (window) {
    "session_5h" -> "5-hour session"
    "weekly" -> "Weekly"
    "monthly" -> "Monthly"
    "daily" -> "Daily"
    "balance" -> when (provider) {
      "openrouter" -> "Credit balance"
      "deepseek" -> "Top-up balance"
      "xai" -> "Prepaid balance"
      else -> "Balance"
    }
    else -> window
  }

  private fun line(context: Context, s: JSONObject, now: Long): WidgetLine {
    val provider = providerName(s.optString("provider", "?"))
    val label = "$provider ${windowName(s.optString("window", ""))}"
    val status = s.optString("status", "ok")
    val value = when {
      s.has("percentUsed") && !s.isNull("percentUsed") -> "${s.getDouble("percentUsed").toInt()}% used"
      s.has("remaining") && !s.isNull("remaining") -> remaining(s.optString("unit", ""), s.getDouble("remaining"))
      else -> "no data"
    }
    val resetsAt = parseIso(s.optString("resetsAt", ""))
    val reset = when {
      resetsAt == null -> ""
      resetsAt <= now -> "reset"
      else -> "resets ${formatTime(context, resetsAt)}"
    }
    val flag = if (status == "ok") "" else status
    return WidgetLine(label, value, listOf(reset, flag).filter { it.isNotEmpty() }.joinToString(" · "))
  }

  private fun windowName(window: String) = when (window) {
    "session_5h" -> "5h"
    "weekly" -> "week"
    "monthly" -> "month"
    "daily" -> "day"
    "balance" -> "balance"
    else -> window
  }

  private fun remaining(unit: String, value: Double): String = when (unit) {
    "usd" -> String.format(Locale.getDefault(), "$%.2f left", value)
    "cny" -> String.format(Locale.getDefault(), "CN¥%.2f left", value)
    "tokens" -> "${value.toLong()} tokens left"
    "requests" -> "${value.toLong()} left"
    else -> "${value.toInt()}% left"
  }

  /** Clock time today, otherwise weekday, date and time. */
  private fun formatTime(context: Context, millis: Long): String {
    val flags = if (DateUtils.isToday(millis)) DateUtils.FORMAT_SHOW_TIME
    else DateUtils.FORMAT_SHOW_TIME or DateUtils.FORMAT_SHOW_WEEKDAY or DateUtils.FORMAT_ABBREV_ALL
    return DateUtils.formatDateTime(context, millis, flags)
  }

  private val ISO = Regex("""^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(Z|[+-]\d{2}:\d{2})$""")

  /** RFC 3339 to epoch milliseconds, or null. Fractions of a second are dropped. */
  fun parseIso(text: String): Long? {
    val m = ISO.matchEntire(text) ?: return null
    val g = m.groupValues
    val cal = Calendar.getInstance(TimeZone.getTimeZone("UTC"))
    cal.clear()
    cal.set(g[1].toInt(), g[2].toInt() - 1, g[3].toInt(), g[4].toInt(), g[5].toInt(), g[6].toInt())
    var millis = cal.timeInMillis
    val zone = g[7]
    if (zone != "Z") {
      val sign = if (zone[0] == '-') -1 else 1
      val offset = sign * (zone.substring(1, 3).toInt() * 60 + zone.substring(4, 6).toInt()) * 60_000L
      millis -= offset
    }
    return millis
  }
}

class CooldownWidgetReceiver : GlanceAppWidgetReceiver() {
  override val glanceAppWidget: GlanceAppWidget = CooldownWidget()
}
