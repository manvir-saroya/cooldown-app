# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile

# Cooldown: Tauri finds NativePlugin's commands and argument classes by reflection (names and
# annotations), so release builds (R8) must not rename or strip them.
-keepattributes *Annotation*, Signature
-keep @app.tauri.annotation.TauriPlugin class * { *; }
-keep @app.tauri.annotation.InvokeArg class * { *; }
-keepclassmembers class * {
  @app.tauri.annotation.Command <methods>;
  @app.tauri.annotation.ActivityCallback <methods>;
  @app.tauri.annotation.PermissionCallback <methods>;
}

# Glance pulls in WorkManager, which opens its Room database through reflection (WorkDatabase_Impl's
# no-argument constructor). R8 full mode removes that constructor unless it is kept; without this rule a
# release build crashes at startup with "Failed to create an instance of androidx.work.impl.WorkDatabase".
-keep class * extends androidx.room.RoomDatabase { <init>(); }

# JNI: the background sync calls into Rust through SyncBridge.runSyncRound. The Rust side exports it by
# its Java name, so R8 must keep native methods and their class names exactly as declared.
-keepclasseswithmembernames class * { native <methods>; }
