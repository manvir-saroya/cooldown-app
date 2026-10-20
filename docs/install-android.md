# Install Cooldown on Android

The Android app is a companion to the Windows app. It shows the limits your PC reads, and it **rings on the phone** when a limit resets, even if the PC is asleep.

> **The phone app does not read anything by itself.** It gets its numbers from Cooldown on your PC. Set up the PC first: [Install Cooldown on Windows](install-windows.md).

You need an Android phone running **Android 7.0 or newer**. The download is built for modern 64-bit (arm64) phones, which is nearly every phone made in the last several years.

**Contents:** [Download](#1-download-the-apk) · [Install](#2-install-it-sideloading) · [Allow permissions](#3-allow-the-permissions) · [Pair](#pair-with-your-pc) · [Bells](#bells) · [Widget](#the-home-screen-widget) · [Background sync](#background-sync) · [Battery settings](#make-sure-android-does-not-stop-the-alarms) · [Limits](#what-it-cannot-do-yet) · [Unpair or uninstall](#unpair-update-or-uninstall)

## 1. Download the APK

An APK is an Android app file. Cooldown is not on Google Play, so you install the file yourself. This is called *sideloading*.

1. Open the [Releases page](../../../releases) in the phone's browser, or on your PC.
2. Under the newest release, open **Assets** and download:
   - `Cooldown_<version>_android.apk`
   - `SHA256SUMS.txt`, if you want to check the file
3. *(Recommended)* Check the file. It is easiest on the PC, in PowerShell, as described in [Check the file](install-windows.md#2-check-the-file-recommended). Use the APK's file name:

   ```powershell
   Get-FileHash .\Cooldown_0.1.0_android.apk -Algorithm SHA256
   ```

   The result must match the line for the APK in `SHA256SUMS.txt`.
4. If you downloaded on the PC, copy the file to the phone (USB cable, or any file-sharing app you already trust).

Only download Cooldown from the Releases page.

## 2. Install it (sideloading)

1. On the phone, open the APK file (from the **Downloads** folder, **Files** app, or the browser's download notification).
2. Android says it cannot install apps from this source. This is normal for a file from outside Google Play.
   1. Choose **Settings**.
   2. Turn on **Allow from this source** (the wording differs between phones). It applies to the app you opened the file from, such as your browser or **Files**.
   3. Go back.
3. Choose **Install**.
4. If **Google Play Protect** says the app is unknown or blocked, choose **More details**, then **Install anyway**. <!-- TODO: verify the Play Protect behaviour with the release-signed APK -->
5. Choose **Open**.

Afterwards you can turn **Allow from this source** off again.

The app is called **Cooldown**. Its bottom menu has **Overview**, **Alerts**, **Pair** and **Settings**.

## 3. Allow the permissions

Cooldown asks for permissions only for things it really needs. Open **Settings**, section **Permissions**, and choose **Grant permissions**. Each permission shows **Allowed** or **Off**.

| Permission | Why | If you say no |
|---|---|---|
| **Exact alarms** | So a bell rings at the exact reset time. On newer Android versions this is a switch in a special settings page, **Alarms & reminders**. Cooldown opens that page for you; turn Cooldown on. | Android may ring a bell minutes late. |
| **Notifications** | So the bell can appear on your screen. Android 13 and newer ask first. | A bell cannot show up, so you would not know your limit is back. |
| **Local network** | So the phone can reach your PC on your Wi-Fi. Android 17 and newer ask first. | The phone cannot sync with your PC. |
| **Camera** | Only to scan the pairing QR code. Android asks the first time you scan. | You cannot scan. Use **Paste code instead** (see the notes below). |

Android asks one question after another: first notifications, then local network, then it opens the **Alarms & reminders** page if exact alarms are still off. Older Android versions do not have some of these questions, and then there is nothing to allow.

If you refused one by mistake, open Android's own settings: **Settings** > **Apps** > **Cooldown** > **Permissions**.

Cooldown also uses the **internet** permission, because Android requires it for any network traffic, including your home network. It only ever talks to your own PC. It does not contact any server of ours, because there isn't one.

## Pair with your PC

Pairing links the phone to your PC. You do it once. Do it on your **home or office network**, not on a public or shared one.

You need both devices on the **same Wi-Fi network**, with Cooldown running on the PC.

1. **On the PC:** open Cooldown and choose **Pair**. The page is called **Pair a device**. A QR code appears with a countdown. (If Windows Firewall asks a question, see [the Windows guide](install-windows.md#your-phone-optional).)
2. **On the phone:** open Cooldown and choose **Pair**, then **Scan code**.
3. Allow the camera if Android asks, and point it at the QR code on the PC screen.
4. The phone says **Connecting to your PC…** and then **Confirm it on your PC**. It shows **six digits**.
5. **On the PC:** the screen asks **Pair “Cooldown phone”?** and shows six digits too.
6. **Compare the digits.** If they are the same, choose **It matches, pair** on the PC. If they differ, choose **It does not match**. Someone else may be trying to pair.
7. The phone says **Paired with** and the name of your PC. Choose **See your limits**.

Notes:

- The QR code works for about **two minutes**, once. If it expires, choose **Get a new code** on the PC.
- You have about a minute to confirm the digits on the PC.
- Five failed attempts make a code stop working.
- If the camera cannot read the code, use the code as text. On the PC, under **Cannot scan? Copy the code**, select **Copy code** and send the text to the phone (for example as a message to yourself). On the phone, choose **Paste code instead**, paste it and choose **Pair**. The text starts with `CD1:` and is about 130 characters long, so copying it is far easier than typing it. It is the same one-time secret as the QR code: it works once, for about two minutes, so do not post it anywhere, and delete the message afterwards.
- If pairing fails, see [Troubleshooting: my phone cannot find my PC](troubleshooting.md#my-phone-cannot-find-my-pc).

After pairing, the phone connects to the PC by itself whenever it can reach it. **Overview** fills with your limits, and the card **Settings** > **Your PC** shows **Connected** and **Last synced**.

## Bells

A *bell* is an alert that rings when a limit resets.

- Choose **Notify me when it resets** on any limit, on the phone or on the PC. You can see and pause all bells under **Alerts**.
- **Bells are kept on your PC.** If you set or pause one on the phone, the phone sends the change to the PC, so the phone must be connected at that moment. If it is not, the phone says *Not connected to your PC* and you can try again when the PC is on.
- The phone then sets its **own alarm** for the reset time. When it rings, you get a notification such as **Claude 5-hour limit is back**, *"Your usage limit has reset. You can work again."*
- The alarm is kept by Android, so it rings **even if the app is closed and even if the PC is asleep or off**. Alarms are restored after the phone restarts.

One catch: the phone only learns about changes (a moved reset time, a new bell) when it syncs with the PC. It syncs when you open the app, and in the background about every 15 minutes while it is on the same Wi-Fi as the PC (see [Background sync](#background-sync)). If the reset time moved while the phone was away from the PC, it rings at the time it last knew.

## The home-screen widget

The widget shows your limits on the home screen, with the time the data is from ("as of ..."). By default it shows **every window** (every limit and balance the PC sends), and it scrolls when there are more than fit. You can choose which ones it shows.

1. Press and hold an empty spot on the home screen.
2. Choose **Widgets**.
3. Find **Cooldown** and drag it onto the home screen.
4. A screen called **Add the Cooldown widget** asks which limits and balances to keep on it. Leave **Show every window** on to show all of them (including any your PC adds later), or switch it off and tick the ones you want (at least one). Then choose **Add widget**. Before the phone has synced anything there is nothing to pick from, so only **Show every window** applies. (If you add the widget with the **Add widget to home screen** button in the app instead, some launchers skip this screen. The widget then shows every window until you change it in Settings.)

To change the choice later, open the app and go to **Settings** > **Home-screen widget**. It has the same **Show every window** switch and list of windows, and an **Add widget to home screen** button when your launcher lets an app do that (otherwise it tells you to add the widget by hand, as above). Some launchers also offer **Reconfigure** when you press and hold the widget. The choice is one setting for every Cooldown widget on your home screen.

The widget catches up with the PC about every 15 minutes in the background ([see below](#background-sync)) and whenever the app syncs. Tapping it opens the app. Before you pair, it says "No data yet. Pair with your PC." <!-- TODO: verify the widget screens and wording on a real phone; so far they are checked on an emulator only -->

## Background sync

With the app closed, the phone still catches up with the PC. Android runs a short job about **every 15 minutes** (the shortest interval it allows for a repeating job), but only while the phone has a network connection and a PC is paired. Each run connects to the PC, takes its newest readings, sets the alarms again from them, refreshes the widget and disconnects. Nothing stays running in between, and there is no permanent notification.

What to expect:

- It only works when the PC is reachable: on, awake, and on the same Wi-Fi network as the phone. Away from home the run just fails quietly and tries again later. The phone keeps the last numbers and alarms it has, and the widget shows when they are from.
- Android has the last word on timing. With battery saver on, or when the phone idles in a pocket (Doze), runs can be late or skipped. A run can be a few minutes late even in good conditions.
- Your alarms do not depend on it. They are set with Android's alarm manager and ring on their own; background sync only keeps them current.
- It stops by itself when you unpair the PC.
- This has been tested on an emulator, not yet on real phones.

## Make sure Android does not stop the alarms

Android is generally good at keeping alarms. Some phone makers add extra "battery saver" systems that stop apps you have not opened for a while, and that can stop alarms too.

To be safe, do this for Cooldown. The menu names differ between brands, so use the search box in your phone's Settings if you cannot find them.

1. **Settings** > **Apps** > **Cooldown** > **Battery**, and set it to **Unrestricted** (or "Don't optimize").
2. Turn **off** "Pause app activity if unused" (also called "Remove permissions and free up space") for Cooldown.
3. On **Samsung**, make sure Cooldown is not in **Sleeping apps** or **Deep sleeping apps** (Settings > Battery).
4. On **Xiaomi, Huawei, Oppo, Vivo, OnePlus** and similar, turn on **Autostart** for Cooldown and lock it in the recent-apps list if your phone offers that.
5. Keep **Exact alarms** allowed (see above).

The site [dontkillmyapp.com](https://dontkillmyapp.com) lists the steps for each brand.

The same battery systems can also delay or skip the background sync described above. So far Cooldown has been tested only on an Android emulator, not on a range of real phones, so it has not been proven how each maker's battery system behaves. Please tell us what you see on your phone by opening an [issue](../../../issues/new/choose). A good test: set a bell for a limit that resets soon, close the app, lock the phone, and see whether it rings.

## What it cannot do yet

- **Same network.** The phone and the PC must be on the same Wi-Fi (or the same private network). It does not work over mobile data or through the internet, because there is no server in between. Some guest networks and some routers block devices from talking to each other ("client isolation" or "AP isolation"), and then pairing and syncing fail.
- **The PC must be on for fresh numbers.** Only the PC reads Claude, Codex and the provider balances. If the PC is off or asleep, the phone shows the last numbers it received and says how old they are.
- **The Claude numbers need Claude Code running on the PC.** See [How it works](how-it-works.md).
- **Alarms still ring** if the PC sleeps or is off, using the reset times the phone already has.
- **Background sync is best effort.** The phone connects to the PC when you open the app and keeps trying while it runs. With the app closed it syncs about every 15 minutes (see [Background sync](#background-sync)), but only on the same Wi-Fi as the PC, and Android decides the exact timing. Bells you already set still ring either way.
- **No history on the phone.** Charts of past readings are on the PC. The phone only holds the latest reading.
- **No API keys on the phone.** Keys stay on the PC. The phone cannot change them.
- **Android only.** There is no iPhone version.
- **Nothing moves to a new phone by backup.** For safety, the pairing key is excluded from Android backups. On a new phone, install the app and pair again.

## Unpair, update or uninstall

- **Unpair on the phone:** **Settings** > **Your PC** > **Unpair**, then confirm. This also cancels that PC's alarms and removes its readings from the phone, and stops the background sync.
- **Unpair on the PC:** **Settings** > **Paired devices** > **Revoke**. Use this if you lose the phone.
- **Update:** download the newer APK and install it over the old one, as in step 2. If Android refuses with a message like *App not installed* or *conflicts with an existing package*, uninstall Cooldown first, install the new file, and pair again. <!-- TODO: verify that release APKs are signed with one stable key so that in-place updates work -->
- **Uninstall:** press and hold the app icon and choose **Uninstall**. If you do not also revoke the phone on the PC, the PC keeps a stale entry in **Paired devices** until you do.

## Having trouble?

See [Troubleshooting](troubleshooting.md). If you cannot solve it, the same page explains how to report a bug.
