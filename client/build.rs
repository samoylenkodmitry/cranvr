// Tells the Cranpose Android build which services and permissions the app declares. The Quest's
// app needs none beyond the `xr` and `network` services that the Gradle module lists. The phone's
// app reads the device's rotation from its motion sensors. That asks the person for nothing. The
// declaration brings the sensor's Java class and the permission for fast sampling.
use cranpose_capabilities::{declare, Use};

fn main() {
    if std::env::var_os("CARGO_FEATURE_PHONE").is_some() {
        declare(&[Use::device_motion()]).emit();
    } else {
        declare(&[]).emit();
    }
}
