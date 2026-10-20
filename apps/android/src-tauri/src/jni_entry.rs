//! The JNI door the background sync worker calls (`SyncBridge` in `SyncWorker.kt`).
//!
//! This is the only place in the crate that needs `unsafe_code` allowed: exporting a function under
//! a fixed C name is what `#[no_mangle]` does, and the compiler files that under that lint. There is
//! no `unsafe` block here. The function does only what cannot be tested without a JVM (turning Java
//! values into Rust ones and back); everything else is [`crate::background::run_headless`].
//!
//! Java side: `object SyncBridge { @JvmStatic external fun runSyncRound(dataDir: String, key: ByteArray): String }`.
#![allow(unsafe_code)]

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use jni::objects::{JByteArray, JClass, JString};
use jni::sys::jstring;
use jni::JNIEnv;
use zeroize::Zeroizing;

use crate::background::{run_headless, Report, Status, REGISTRY};

fn failed(message: &str) -> String {
    Report::bare(Status::Failed, message).to_json()
}

/// Run one round and return the report as JSON. `key` is the phone's 32-byte private key as the
/// worker just opened it with the Keystore; it is wiped here when the round is over.
pub fn run(data_dir: &str, key: Zeroizing<Vec<u8>>) -> String {
    let Ok(raw) = <[u8; 32]>::try_from(key.as_slice()) else {
        return failed("The phone key has the wrong size.");
    };
    run_headless(&REGISTRY, Path::new(data_dir), Zeroizing::new(raw)).to_json()
}

/// `SyncBridge.runSyncRound(dataDir, key)`: the report as a JSON string, never an exception. A
/// panic inside the round is caught here, because unwinding into Java would abort the app.
#[no_mangle]
pub extern "system" fn Java_app_cooldown_phone_SyncBridge_runSyncRound<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    data_dir: JString<'local>,
    key: JByteArray<'local>,
) -> jstring {
    let reply = catch_unwind(AssertUnwindSafe(|| {
        let Ok(dir) = env.get_string(&data_dir).map(String::from) else {
            return failed("The data folder is not text.");
        };
        let Ok(key) = env.convert_byte_array(&key).map(Zeroizing::new) else {
            return failed("The phone key could not be read.");
        };
        run(&dir, key)
    }))
    .unwrap_or_else(|_| failed("The sync stopped unexpectedly."));
    env.new_string(reply)
        .map_or(std::ptr::null_mut(), JString::into_raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_of_the_wrong_size_is_refused_before_anything_is_opened() {
        let dir = tempfile::tempdir().unwrap();
        for len in [0, 31, 33] {
            let reply = run(dir.path().to_str().unwrap(), Zeroizing::new(vec![1u8; len]));
            let value: serde_json::Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(value["status"], "failed");
        }
        assert!(
            std::fs::read_dir(dir.path()).unwrap().next().is_none(),
            "nothing was created"
        );
    }

    #[test]
    fn a_good_key_and_an_empty_folder_report_nothing_paired() {
        let dir = tempfile::tempdir().unwrap();
        let reply = run(dir.path().to_str().unwrap(), Zeroizing::new(vec![9u8; 32]));
        let value: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(value["status"], "no_pc");
        assert_eq!(value["alarms"], serde_json::json!([]));
        assert_eq!(value["widget"], "");
    }
}
