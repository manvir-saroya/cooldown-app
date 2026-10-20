package app.cooldown.phone

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.os.Bundle
import android.util.Log
import android.util.Size
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.FrameLayout
import android.widget.TextView
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ExperimentalGetImage
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import com.google.mlkit.vision.barcode.BarcodeScanner
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Hands the scanned pairing text from [ScanActivity] to [NativePlugin] inside this process only.
 * The QR text holds the one-time pairing secret, so it never goes into an Intent (which would pass
 * through the system server) and is cleared the moment it is read.
 */
object ScanResultHolder {
  @Volatile private var text: String? = null

  fun put(value: String) { text = value }

  fun take(): String? {
    val value = text
    text = null
    return value
  }
}

/**
 * A minimal full-screen QR scanner: CameraX preview + ML Kit (bundled model, no Google Play
 * Services). Not exported; started only by [NativePlugin.scanQr].
 *
 * Results: `RESULT_OK` with the text in [ScanResultHolder]; `RESULT_CANCELED` without an error extra
 * means the user cancelled; with [EXTRA_ERROR] it carries a message for the user.
 */
class ScanActivity : ComponentActivity() {
  private lateinit var previewView: PreviewView
  private var cameraProvider: ProcessCameraProvider? = null
  private val analysisExecutor: ExecutorService = Executors.newSingleThreadExecutor()
  private val finishing = AtomicBoolean(false)
  // Created on the first frame (not when the permission is denied), closed in onDestroy.
  @Volatile private var scannerInstance: BarcodeScanner? = null
  private val scanner: BarcodeScanner
    get() = scannerInstance ?: BarcodeScanning.getClient(
      BarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build()
    ).also { scannerInstance = it }

  private val askCamera =
    registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
      if (granted) startCamera()
      else fail("Cooldown needs the camera to scan the code. You can allow it in Settings, Apps, Cooldown, Permissions.")
    }

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
      navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT)
    )
    super.onCreate(savedInstanceState)
    setContentView(buildUi())
    if (ContextCompat.checkSelfPermission(this, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
      startCamera()
    } else {
      askCamera.launch(Manifest.permission.CAMERA)
    }
  }

  private fun buildUi(): View {
    val density = resources.displayMetrics.density
    fun dp(v: Int) = (v * density).toInt()

    val root = FrameLayout(this).apply {
      setBackgroundColor(Color.BLACK)
      // A confirm-type screen: ignore touches while another app draws over it.
      filterTouchesWhenObscured = true
    }
    previewView = PreviewView(this).apply {
      implementationMode = PreviewView.ImplementationMode.COMPATIBLE
      scaleType = PreviewView.ScaleType.FILL_CENTER
      keepScreenOn = true
      contentDescription = "Camera view"
    }
    root.addView(previewView, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))

    val hint = TextView(this).apply {
      text = "Point the camera at the pairing code on your PC"
      setTextColor(Color.WHITE)
      textSize = 17f
      gravity = Gravity.CENTER
      setPadding(dp(16), dp(10), dp(16), dp(10))
      setBackgroundColor(0x99000000.toInt())
      accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
    }
    val hintParams = FrameLayout.LayoutParams(
      ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP or Gravity.CENTER_HORIZONTAL
    ).apply { topMargin = dp(24); marginStart = dp(16); marginEnd = dp(16) }
    root.addView(hint, hintParams)

    val cancel = Button(this).apply {
      text = "Cancel"
      setOnClickListener { cancelScan() }
    }
    val cancelParams = FrameLayout.LayoutParams(
      dp(200), dp(52), Gravity.BOTTOM or Gravity.CENTER_HORIZONTAL
    ).apply { bottomMargin = dp(32) }
    root.addView(cancel, cancelParams)

    // Edge-to-edge: keep the controls clear of the status bar, cutout and navigation bar.
    ViewCompat.setOnApplyWindowInsetsListener(root) { _, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      hintParams.topMargin = bars.top + dp(24)
      cancelParams.bottomMargin = bars.bottom + dp(32)
      hint.layoutParams = hintParams
      cancel.layoutParams = cancelParams
      insets
    }
    return root
  }

  private fun startCamera() {
    val future = ProcessCameraProvider.getInstance(this)
    future.addListener({
      val provider = try {
        future.get()
      } catch (e: Exception) {
        fail("The camera could not be opened.")
        return@addListener
      }
      cameraProvider = provider
      val preview = Preview.Builder().build().also { it.surfaceProvider = previewView.surfaceProvider }
      val analysis = ImageAnalysis.Builder()
        .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
        .setResolutionSelector(
          ResolutionSelector.Builder()
            .setResolutionStrategy(
              ResolutionStrategy(Size(1280, 720), ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER)
            )
            .build()
        )
        .build()
        .also { it.setAnalyzer(analysisExecutor, ::analyze) }
      try {
        provider.unbindAll()
        provider.bindToLifecycle(this, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
      } catch (e: Exception) {
        Log.w(TAG, "Could not bind the camera: ${e.javaClass.simpleName}")
        fail("No camera is available on this phone.")
      }
    }, ContextCompat.getMainExecutor(this))
  }

  @androidx.annotation.OptIn(ExperimentalGetImage::class)
  private fun analyze(proxy: ImageProxy) {
    val media = proxy.image
    if (media == null || finishing.get()) {
      proxy.close()
      return
    }
    try {
      scanner.process(InputImage.fromMediaImage(media, proxy.imageInfo.rotationDegrees))
        .addOnSuccessListener { codes ->
          val text = codes.firstNotNullOfOrNull { it.rawValue }
          if (text != null) onQr(text)
        }
        .addOnCompleteListener { proxy.close() }
    } catch (e: Exception) {
      proxy.close()
    }
  }

  /** On the main thread (ML Kit's default listener executor). */
  private fun onQr(text: String) {
    if (!finishing.compareAndSet(false, true)) return
    if (!text.startsWith(PREFIX)) {
      // Do not log or echo what was scanned: it might be someone else's code.
      setResult(RESULT_CANCELED, Intent().putExtra(EXTRA_ERROR, "That is not a Cooldown pairing code"))
    } else {
      ScanResultHolder.put(text)
      setResult(RESULT_OK)
    }
    finish()
  }

  private fun fail(message: String) {
    if (!finishing.compareAndSet(false, true)) return
    setResult(RESULT_CANCELED, Intent().putExtra(EXTRA_ERROR, message))
    finish()
  }

  private fun cancelScan() {
    if (!finishing.compareAndSet(false, true)) return
    setResult(RESULT_CANCELED)
    finish()
  }

  override fun onDestroy() {
    super.onDestroy()
    finishing.set(true)
    cameraProvider?.unbindAll()
    analysisExecutor.shutdown()
    scannerInstance?.close()
  }

  companion object {
    private const val TAG = "CooldownScan"
    const val PREFIX = "CD1:"
    const val EXTRA_ERROR = "error"
  }
}
