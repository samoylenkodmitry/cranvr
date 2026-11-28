//! The launch arguments that the client takes.
//!
//! On Android they are the extras of the launching intent. Any app on the headset can start the
//! client's exported activity with extras, and some extras change what the client trusts:
//! `forget_host_key` drops a paired Mac's key, `host` with `connect` links to another address. So
//! the client takes the extras only from a launch through its entry for adb, the activity alias
//! `dev.cranvr.client.AdbStart`. Both manifests declare it with the permission DUMP, which only the
//! shell user of adb and the system hold. Another app that starts the alias fails, and its start of
//! the plain activity counts as a start without arguments. A desktop build takes its command line,
//! which is the user's own.

use cranpose::{LaunchArgs, LaunchArgsRef};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};

/// The class of the entry for adb, as `client/android/app/src/*/AndroidManifest.xml` declare it
pub const ADB_ENTRY: &str = "dev.cranvr.client.AdbStart";

/// The arguments of this launch, or none for a launch that did not come through the entry for adb
pub fn args() -> LaunchArgsRef {
    #[expect(clippy::disallowed_methods, reason = "the one place that reads the launch's arguments and checks their source")]
    let args = cranpose::launch_args();
    if taken(&args) {
        return args;
    }
    static TOLD: AtomicBool = AtomicBool::new(false);
    if !args.is_empty() && !TOLD.swap(true, Ordering::Relaxed) {
        log::warn!(
            "the launch through {} carries {} arguments, which count only through {ADB_ENTRY}",
            args.component_class_name().unwrap_or("no named activity"),
            args.len()
        );
    }
    static NONE: OnceLock<LaunchArgsRef> = OnceLock::new();
    Arc::clone(NONE.get_or_init(|| Arc::new(LaunchArgs::default())))
}

/// Whether the client takes the arguments of a launch
fn taken(args: &LaunchArgs) -> bool {
    match args.component_class_name() {
        Some(class) => class == ADB_ENTRY,
        // Every launch of an Android activity names one. A desktop build has none.
        None => !cfg!(target_os = "android"),
    }
}

#[cfg(test)]
#[path = "tests/launch.rs"]
mod tests;
