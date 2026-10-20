package app.cooldown.phone

import android.app.Activity
import android.appwidget.AppWidgetManager
import android.content.Intent
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.LinearLayout
import android.widget.ScrollView
import androidx.activity.enableEdgeToEdge
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import com.google.android.material.button.MaterialButton
import com.google.android.material.checkbox.MaterialCheckBox
import com.google.android.material.materialswitch.MaterialSwitch
import com.google.android.material.textview.MaterialTextView

/**
 * Asked by the launcher when a Cooldown widget is added (and again from "Reconfigure" on launchers
 * that offer it): which limits and balances should the widget show? The same choice can be changed
 * later in the app's settings; both write [WidgetConfig].
 *
 * Backing out cancels adding the widget, as Android expects. The list comes from the data the app
 * last saved; with nothing synced yet it offers only "every window", which also covers whatever the
 * PC sends later.
 */
class WidgetConfigActivity : AppCompatActivity() {
  private var widgetId = AppWidgetManager.INVALID_APPWIDGET_ID
  private var showAll = true
  private val picked = linkedSetOf<String>()
  private lateinit var choices: List<WidgetChoice>
  private lateinit var list: LinearLayout
  private lateinit var save: MaterialButton

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    widgetId = intent?.extras?.getInt(AppWidgetManager.EXTRA_APPWIDGET_ID, AppWidgetManager.INVALID_APPWIDGET_ID)
      ?: AppWidgetManager.INVALID_APPWIDGET_ID
    // Until the user says yes, the answer is "no widget".
    setResult(Activity.RESULT_CANCELED, resultIntent())

    choices = WidgetData.choices(this)
    val saved = savedInstanceState?.getStringArrayList(STATE_PICKED)
    if (saved != null) {
      showAll = savedInstanceState.getBoolean(STATE_ALL, true)
      picked.addAll(saved)
    } else {
      val stored = WidgetConfig.get(this)
      showAll = stored == null
      // Start from what is chosen now; with "every window" on, the list begins with all of them ticked.
      picked.addAll(stored ?: choices.map { it.id })
    }
    setContentView(buildView())
    render()
  }

  override fun onSaveInstanceState(outState: Bundle) {
    super.onSaveInstanceState(outState)
    outState.putBoolean(STATE_ALL, showAll)
    outState.putStringArrayList(STATE_PICKED, ArrayList(picked))
  }

  private fun resultIntent() = Intent().putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, widgetId)

  private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

  private fun buildView(): View {
    val column = LinearLayout(this).apply {
      orientation = LinearLayout.VERTICAL
      setPadding(dp(24), dp(24), dp(24), dp(24))
    }
    column.addView(
      MaterialTextView(this, null, com.google.android.material.R.attr.textAppearanceHeadlineSmall).apply {
        setText(if (widgetId == AppWidgetManager.INVALID_APPWIDGET_ID) "Widget windows" else "Add the Cooldown widget")
        setTextAppearance(com.google.android.material.R.style.TextAppearance_Material3_HeadlineSmall)
        ViewCompat.setAccessibilityHeading(this, true)
      }
    )
    column.addView(
      MaterialTextView(this).apply {
        text = "Choose the limits and balances to keep on your home screen. The widget catches up with your PC about every 15 minutes."
        setTextAppearance(com.google.android.material.R.style.TextAppearance_Material3_BodyMedium)
        setPadding(0, dp(8), 0, dp(16))
      }
    )

    val everything = MaterialSwitch(this).apply {
      text = "Show every window"
      isChecked = showAll
      setOnCheckedChangeListener { _, on ->
        showAll = on
        render()
      }
    }
    column.addView(everything, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
    column.addView(
      MaterialTextView(this).apply {
        text = "On, it also shows windows your PC adds later."
        setTextAppearance(com.google.android.material.R.style.TextAppearance_Material3_BodySmall)
        setPadding(dp(0), dp(2), 0, dp(8))
      }
    )

    list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
    if (choices.isEmpty()) {
      list.addView(
        MaterialTextView(this).apply {
          text = "Nothing has synced from your PC yet, so there are no windows to pick from. The widget will show every window once they arrive."
          setTextAppearance(com.google.android.material.R.style.TextAppearance_Material3_BodyMedium)
          setPadding(0, dp(8), 0, dp(8))
        }
      )
    }
    for (choice in choices) {
      list.addView(
        MaterialCheckBox(this).apply {
          text = choice.label
          tag = choice.id
          minimumHeight = dp(48)
          isChecked = choice.id in picked
          setOnCheckedChangeListener { _, on ->
            if (on) picked.add(choice.id) else picked.remove(choice.id)
            render()
          }
        }
      )
    }
    column.addView(list)

    val buttons = LinearLayout(this).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.END
      setPadding(0, dp(24), 0, 0)
    }
    buttons.addView(
      MaterialButton(this, null, com.google.android.material.R.attr.borderlessButtonStyle).apply {
        text = "Cancel"
        setOnClickListener { finish() }
      }
    )
    save = MaterialButton(this).apply {
      text = if (widgetId == AppWidgetManager.INVALID_APPWIDGET_ID) "Save" else "Add widget"
      setOnClickListener { save() }
    }
    buttons.addView(save, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { marginStart = dp(8) })
    column.addView(buttons)

    return ScrollView(this).apply {
      isFillViewport = true
      addView(column)
      ViewCompat.setOnApplyWindowInsetsListener(this) { view, insets ->
        val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
        view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
        insets
      }
    }
  }

  /** The list is greyed while "every window" is on; Save needs at least one window otherwise. */
  private fun render() {
    for (i in 0 until list.childCount) {
      val box = list.getChildAt(i) as? MaterialCheckBox ?: continue
      box.isEnabled = !showAll
      val should = box.tag as String in picked
      if (box.isChecked != should) box.isChecked = should
    }
    save.isEnabled = showAll || choices.any { it.id in picked }
  }

  private fun save() {
    // In the order the app lists them, which is the order of the data.
    val chosen = choices.map { it.id }.filter { it in picked }
    WidgetConfig.set(this, if (showAll || chosen.isEmpty()) null else chosen)
    setResult(Activity.RESULT_OK, resultIntent())
    finish()
  }

  companion object {
    private const val STATE_ALL = "all"
    private const val STATE_PICKED = "picked"
  }
}
