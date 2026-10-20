import java.util.Properties
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

// Release signing comes from the environment, so no keystore or password is ever committed (see
// docs/releasing.md). CI writes the keystore to a temp file and sets these four variables. With
// none of them set (local builds, releases without secrets) the release build is signed with the
// throwaway debug key instead, so it still installs, but it is not a distributable identity.
val releaseKeystorePath: String? = System.getenv("ANDROID_KEYSTORE_PATH")?.takeIf { it.isNotBlank() }
val releaseKeyEnv = listOf("ANDROID_KEYSTORE_PASSWORD", "ANDROID_KEY_ALIAS", "ANDROID_KEY_PASSWORD")
    .associateWith { System.getenv(it)?.takeIf { v -> v.isNotBlank() } }
if (releaseKeystorePath != null && releaseKeyEnv.values.any { it == null }) {
    throw GradleException(
        "ANDROID_KEYSTORE_PATH is set but these are missing: " +
            releaseKeyEnv.filterValues { it == null }.keys.joinToString(", ")
    )
}

android {
    compileSdk = 37
    namespace = "app.cooldown.phone"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "app.cooldown.phone"
        minSdk = 24
        targetSdk = 37
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    signingConfigs {
        if (releaseKeystorePath != null) {
            create("release") {
                storeFile = file(releaseKeystorePath)
                storePassword = releaseKeyEnv["ANDROID_KEYSTORE_PASSWORD"]
                keyAlias = releaseKeyEnv["ANDROID_KEY_ALIAS"]
                keyPassword = releaseKeyEnv["ANDROID_KEY_PASSWORD"]
            }
        }
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {
                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            signingConfig = signingConfigs.findByName("release") ?: signingConfigs.getByName("debug")
            optimization {
               enable = true
            }
            proguardFiles(
                *fileTree(".") {
                  include("**/*.pro")
                  exclude("build/**")
                }.files.toTypedArray()
            )
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    buildFeatures {
        buildConfig = true
        compose = true
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    // The home-screen widget.
    implementation("androidx.glance:glance-appwidget:1.1.1")
    // The background sync (SyncWorker): a periodic job that survives the app being closed and a reboot.
    // Pinned above the 2.7 that Glance brings in.
    implementation("androidx.work:work-runtime-ktx:2.11.2")
    // QR scanning (ScanActivity): CameraX plus ML Kit with the model bundled in the app, so there is
    // no Google Play Services dependency and it works offline and on the emulator.
    val cameraX = "1.6.2"
    implementation("androidx.camera:camera-core:$cameraX")
    implementation("androidx.camera:camera-camera2:$cameraX")
    implementation("androidx.camera:camera-lifecycle:$cameraX")
    implementation("androidx.camera:camera-view:$cameraX")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")
}

apply(from = file("tauri.build.gradle.kts"))
