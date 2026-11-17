// Tells the Cranpose Android build which services and permissions the app declares. The app
// needs none beyond the `xr` and `network` services that the Gradle module lists.
use cranpose_capabilities::declare;

fn main() {
    declare(&[]).emit();
}
