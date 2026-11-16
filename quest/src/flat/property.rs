// Settings of the window display from Android system properties, for A/B runs without a new
// build: `adb shell setprop debug.cranvr.NAME VALUE`. Properties under debug.* are open to the
// shell user.

pub fn get(name: &str) -> Option<String> {
    let name = std::ffi::CString::new(format!("debug.cranvr.{name}")).ok()?;
    let mut value = [0 as std::ffi::c_char; 92];
    let length = unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };
    if length <= 0 {
        return None;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) }.to_str().ok()?;
    Some(text.trim().to_string())
}

#[link(name = "c")]
extern "C" {
    fn __system_property_get(name: *const std::ffi::c_char, value: *mut std::ffi::c_char) -> i32;
}
