// Tells the Cranpose Android build which services and permissions the app declares. The Quest's
// app needs the `xr` and `network` services that the Gradle module lists. Its build also asks for
// the four kinds of tracking, because the host's games may read the user's hands, body, face and
// eyes (`src/tracked.rs`): an OpenXR instance fixes its extensions before the app runs, so the
// declaration cannot wait for a game. It also declares the controller models, so the Quest's own
// models of its controllers load (`src/ui.rs` shows them while a panel does). The declaration writes the permissions of each kind to the
// manifest. Cranpose asks the user for them before the OpenXR instance exists, and the headset
// offers a kind to the host only after the user allowed it. The phone's app reads the device's
// rotation from its motion sensors. That asks the person for nothing. The declaration brings the
// sensor's Java class and the permission for fast sampling. The headset's app asks for its USB
// network to the Mac (`src/usb_network.rs`), which brings the class of Cranpose's network requests
// and the permission to change the network state.
use cranpose_capabilities::{declare, Use};

fn main() {
    if std::env::var_os("CARGO_FEATURE_PHONE").is_some() {
        declare(&[Use::device_motion()]).emit();
    } else {
        declare(&[
            Use::hand_tracking(),
            Use::body_tracking(),
            Use::face_tracking(),
            Use::eye_tracking(),
            Use::controller_models(),
            Use::network_request(),
        ])
        .emit();
    }
}
