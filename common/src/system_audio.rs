//! The macOS permission to record this Mac's sound ("System Audio Recording"). The host's Core Audio
//! tap needs it. macOS gives it to an app: the cranvr app asks for it. The host inherits it as the app's
//! child.
//!
//! macOS has no public call that reads or requests this permission. Creating a tap is the only
//! public way. A tap shows the prompt in the name of whatever app started the process: a test host
//! started from a terminal would ask for the terminal. So this module calls the TCC framework,
//! as other apps that record the system sound do: `TCCAccessPreflight` reads the state and never
//! shows a prompt, `TCCAccessRequest` shows the prompt once. The framework is private, so it loads
//! at run time. Without it, the state reads as unknown.

use std::{
    ffi::{c_char, c_int, c_void, CStr},
    sync::OnceLock,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Granted,
    Denied,
    /// Not asked yet, or the state could not be read
    Unknown,
}

const TCC: &CStr = c"/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC";
const SERVICE: &CStr = c"kTCCServiceAudioCapture";
const RTLD_NOW: c_int = 2;
const UTF8: u32 = 0x0800_0100;

type Preflight = unsafe extern "C" fn(service: *const c_void, options: *const c_void) -> c_int;
type Request = unsafe extern "C" fn(service: *const c_void, options: *const c_void, done: *const c_void);

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(allocator: *const c_void, text: *const c_char, encoding: u32) -> *const c_void;
}

struct Framework {
    preflight: Option<Preflight>,
    request: Option<Request>,
    /// The service name as a CFString. It lives as long as the process.
    service: usize,
}

fn framework() -> &'static Framework {
    static FRAMEWORK: OnceLock<Framework> = OnceLock::new();
    FRAMEWORK.get_or_init(|| unsafe {
        let service = CFStringCreateWithCString(std::ptr::null(), SERVICE.as_ptr(), UTF8);
        // Without the service's name neither call can ask TCC: the framework then counts as missing
        let handle = if service.is_null() { std::ptr::null_mut() } else { dlopen(TCC.as_ptr(), RTLD_NOW) };
        let symbol = |name: &CStr| if handle.is_null() { std::ptr::null_mut() } else { dlsym(handle, name.as_ptr()) };
        let (preflight, request) = (symbol(c"TCCAccessPreflight"), symbol(c"TCCAccessRequest"));
        Framework {
            preflight: (!preflight.is_null()).then(|| std::mem::transmute::<*mut c_void, Preflight>(preflight)),
            request: (!request.is_null()).then(|| std::mem::transmute::<*mut c_void, Request>(request)),
            service: service as usize,
        }
    })
}

/// The state of the permission for the app that started this process. It never shows a prompt.
pub fn access() -> Access {
    let framework = framework();
    match framework.preflight {
        // TCC answers 0 for allowed, 1 for denied and 2 when nobody was asked yet
        Some(preflight) => match unsafe { preflight(framework.service as *const c_void, std::ptr::null()) } {
            0 => Access::Granted,
            1 => Access::Denied,
            _ => Access::Unknown,
        },
        None => Access::Unknown,
    }
}

/// Shows the system prompt for the permission. macOS shows it once per app: after an answer,
/// only the switch in System Settings changes it. The call returns at once; `access` tells the
/// answer later.
pub fn request() {
    let framework = framework();
    if let Some(request) = framework.request {
        let done: block2::RcBlock<dyn Fn(objc2::runtime::Bool)> = block2::RcBlock::new(|_granted: objc2::runtime::Bool| {});
        unsafe { request(framework.service as *const c_void, std::ptr::null(), block2::RcBlock::as_ptr(&done).cast()) };
    }
}
