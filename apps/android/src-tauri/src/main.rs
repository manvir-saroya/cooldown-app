// Desktop entry, for running the phone app on a computer. Android starts through `run()` directly.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    cooldown_android_lib::run();
}
