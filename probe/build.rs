// On Android, ALVR's client core brings C++ code (the oboe audio library). The probe links the
// NDK's static C++ library, so it runs as one file that adb pushes.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        println!("cargo:rustc-link-lib=c++_static");
        println!("cargo:rustc-link-lib=c++abi");
    }
}
