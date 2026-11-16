// Lines of the window display go straight to logcat under the tag "cranvr". ALVR's logger
// drops info lines while a stream runs: the server sets the client's log level, which is errors
// only by default.

use std::ffi::CString;

const ANDROID_LOG_INFO: i32 = 4;

#[link(name = "log")]
extern "C" {
    fn __android_log_write(priority: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32;
}

pub fn line(text: &str) {
    if let Ok(text) = CString::new(text) {
        unsafe { __android_log_write(ANDROID_LOG_INFO, c"cranvr".as_ptr(), text.as_ptr()) };
    }
}
