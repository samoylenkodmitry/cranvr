use super::{taken, ADB_ENTRY};
use cranpose::{LaunchArgValue, LaunchArgs};

// The arguments that would make the client forget a paired Mac and link to another one
fn redirect() -> LaunchArgs {
    LaunchArgs::new(
        [
            ("forget_host_key".to_string(), LaunchArgValue::Text("192.168.1.20:9950".into())),
            ("host".to_string(), LaunchArgValue::Text("192.168.1.66:9950".into())),
            ("connect".to_string(), LaunchArgValue::Bool(true)),
        ],
        false,
    )
}

// adb's shell starts the alias, which only a caller that holds DUMP may start. Another app on the
// headset can start only the plain activity, or name some other class of its own.
#[test]
fn only_a_launch_through_the_entry_for_adb_gives_its_arguments() {
    assert!(taken(&redirect().with_component_class_name(ADB_ENTRY)));
    assert!(!taken(&redirect().with_component_class_name("dev.cranpose.android.CranposeActivity")));
    assert!(!taken(&redirect().with_component_class_name("dev.cranvr.client.AdbStartX")));
    assert!(!taken(&redirect().with_component_class_name("")));
}

// A desktop build has no intent, and its command line is the user's own. On Android every launch of
// an activity names one, so a launch that names none gives nothing there.
#[test]
fn a_launch_that_names_no_activity_gives_its_arguments_only_outside_android() {
    assert_eq!(taken(&redirect()), !cfg!(target_os = "android"));
}
