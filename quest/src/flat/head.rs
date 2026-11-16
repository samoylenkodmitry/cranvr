// The head pose from the device's motion sensors. The device is the head: its screen faces the
// user, so the view looks into the screen. The game rotation vector gives the orientation without
// the compass. The gyroscope runs faster (416 Hz on a Galaxy Tab S7+ against 200 Hz), so its
// samples carry the orientation forward between two rotation vectors. Each gyroscope sample
// becomes a pose that the host gets.

use super::clock;
use alvr_client_core::ClientCoreContext;
use alvr_common::{
    glam::{Quat, Vec3},
    DeviceMotion, Pose, HEAD_ID,
};
use std::{
    ffi::{c_char, c_void},
    ptr,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

/// The newest head pose and when its sensor sample was taken.
#[derive(Clone, Copy)]
pub struct HeadSample {
    pub orientation: Quat,
    /// In the world frame, in radians per second
    pub angular_velocity: Vec3,
    /// CLOCK_MONOTONIC
    pub sample_time: Duration,
}

/// Where the head sits. The device gives no position.
pub const HEAD_POSITION: Vec3 = Vec3::new(0.0, 1.5, 0.0);

// The host draws for every pose it gets. A pose every 4 ms keeps the newest pose fresh at a
// 120 Hz refresh without sending every gyroscope sample.
const SEND_INTERVAL: Duration = Duration::from_millis(4);

// How much each rotation vector pulls the gyroscope's orientation back, against drift
const CORRECTION: f32 = 0.02;

const TYPE_GYROSCOPE: i32 = 4;
const TYPE_GAME_ROTATION_VECTOR: i32 = 15;

#[repr(C)]
struct SensorEvent {
    version: i32,
    sensor: i32,
    kind: i32,
    reserved0: i32,
    timestamp: i64,
    data: [f32; 16],
    flags: u32,
    reserved1: [i32; 3],
}

#[link(name = "android")]
extern "C" {
    fn ASensorManager_getInstanceForPackage(package: *const c_char) -> *mut c_void;
    fn ASensorManager_getDefaultSensor(manager: *mut c_void, kind: i32) -> *const c_void;
    fn ASensorManager_createEventQueue(
        manager: *mut c_void,
        looper: *mut c_void,
        ident: i32,
        callback: *const c_void,
        data: *mut c_void,
    ) -> *mut c_void;
    fn ASensorEventQueue_registerSensor(
        queue: *mut c_void,
        sensor: *const c_void,
        sampling_period_us: i32,
        max_batch_report_latency_us: i64,
    ) -> i32;
    fn ASensorEventQueue_getEvents(queue: *mut c_void, events: *mut SensorEvent, count: usize) -> isize;
    fn ASensor_getMinDelay(sensor: *const c_void) -> i32;
    fn ALooper_prepare(options: i32) -> *mut c_void;
    fn ALooper_pollOnce(timeout_ms: i32, fd: *mut i32, events: *mut i32, data: *mut *mut c_void) -> i32;
}

pub struct Head {
    latest: Arc<Mutex<Option<HeadSample>>>,
}

impl Head {
    /// Starts reading the sensors on a thread of its own. Each pose goes to the host.
    pub fn start(core: Arc<ClientCoreContext>) -> Self {
        let latest = Arc::new(Mutex::new(None));
        let shared = Arc::clone(&latest);
        thread::Builder::new()
            .name("head".into())
            .spawn(move || unsafe { read_sensors(&core, &shared) })
            .expect("head thread");
        Self { latest }
    }

    pub fn latest(&self) -> Option<HeadSample> {
        *self.latest.lock().unwrap()
    }
}

/// The sensor frame has Z up; ALVR's world has Y up and looks along -Z.
fn sensor_to_world() -> Quat {
    Quat::from_rotation_x(-std::f32::consts::FRAC_PI_2)
}

/// The screen's axes in the device's sensor axes. The sensors follow the device's natural
/// orientation; a Galaxy Tab S7+ panel is portrait by nature, so the landscape window turns
/// by the display rotation (Display.getRotation(): 0, 1, 2 or 3 quarter turns). ROTATION_90
/// means the device turned a quarter counterclockwise: the screen's right is the device's -Y
/// and its up is the device's +X, a turn of -90 degrees about Z.
fn screen_in_device() -> Quat {
    let quarter_turns = display_rotation().unwrap_or(0);
    Quat::from_rotation_z(-(quarter_turns as f32) * std::f32::consts::FRAC_PI_2)
}

fn display_rotation() -> Option<i32> {
    use jni::objects::JObject;
    let context = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    // The context here is the application, so the window manager comes from getSystemService
    let app = unsafe { JObject::from_raw(context.context().cast()) };
    let rotation = (|| {
        let name = env.new_string("window").ok()?;
        let manager = env
            .call_method(&app, "getSystemService", "(Ljava/lang/String;)Ljava/lang/Object;", &[(&name).into()])
            .ok()?
            .l()
            .ok()?;
        let display = env.call_method(&manager, "getDefaultDisplay", "()Landroid/view/Display;", &[]).ok()?.l().ok()?;
        env.call_method(&display, "getRotation", "()I", &[]).ok()?.i().ok()
    })();
    if env.exception_check().unwrap_or(false) {
        env.exception_clear().ok();
    }
    // `app` wraps the application's own reference: it has no Drop, so the reference stays
    rotation
}

unsafe fn read_sensors(core: &ClientCoreContext, latest: &Mutex<Option<HeadSample>>) {
    let manager = ASensorManager_getInstanceForPackage(c"dev.cranvr.client".as_ptr());
    let gyroscope = ASensorManager_getDefaultSensor(manager, TYPE_GYROSCOPE);
    let rotation = ASensorManager_getDefaultSensor(manager, TYPE_GAME_ROTATION_VECTOR);
    if manager.is_null() || gyroscope.is_null() || rotation.is_null() {
        super::log::line("cranvr-flat: this device has no gyroscope or game rotation vector");
        return;
    }
    // ALOOPER_PREPARE_ALLOW_NON_CALLBACKS: without it the looper never reports the queue's
    // events, and the thread reads them only when a poll times out
    let looper = ALooper_prepare(1);
    let queue = ASensorManager_createEventQueue(manager, looper, 1, ptr::null(), ptr::null_mut());
    for sensor in [gyroscope, rotation] {
        // The fastest rate each sensor has, with no batching
        ASensorEventQueue_registerSensor(queue, sensor, ASensor_getMinDelay(sensor), 0);
    }

    // A test of the whole path: the head turns in the world by this yaw, for the host and for
    // the warp. A world-fixed line in the picture must move right for a turn to the left
    // (scripts/e2e-flat.sh --warp-check).
    let world_yaw = super::property::get("world_yaw_deg").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
    let world_turn = Quat::from_rotation_y(world_yaw.to_radians());
    let to_world = sensor_to_world();
    let screen = screen_in_device();
    super::log::line(&format!("cranvr-flat: display rotation {:?}", display_rotation()));
    let mut orientation: Option<Quat> = None;
    let mut last_gyro_ns = 0i64;
    let mut yaw_offset: Option<Quat> = None;
    let mut last_sent = Duration::ZERO;
    let mut events: [SensorEvent; 32] = std::mem::zeroed();
    loop {
        ALooper_pollOnce(-1, ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
        loop {
            let count = ASensorEventQueue_getEvents(queue, events.as_mut_ptr(), events.len());
            if count <= 0 {
                break;
            }
            for event in &events[..count as usize] {
                match event.kind {
                    TYPE_GAME_ROTATION_VECTOR => {
                        let [x, y, z, w] = [event.data[0], event.data[1], event.data[2], event.data[3]];
                        let measured = to_world * Quat::from_xyzw(x, y, z, w).normalize();
                        orientation = Some(match orientation {
                            Some(current) => current.slerp(measured, CORRECTION),
                            None => measured,
                        });
                    }
                    TYPE_GYROSCOPE => {
                        let Some(current) = orientation else { continue };
                        // The rate is in the device frame: it turns the orientation from the right
                        let rate = Vec3::new(event.data[0], event.data[1], event.data[2]);
                        if last_gyro_ns != 0 {
                            let dt = (event.timestamp - last_gyro_ns) as f32 * 1e-9;
                            if dt > 0.0 && dt < 0.1 {
                                let turn = rate * dt;
                                let next = (current * Quat::from_scaled_axis(turn)).normalize();
                                orientation = Some(next);
                            }
                        }
                        last_gyro_ns = event.timestamp;
                        // The orientation of the screen, which the view looks through
                        let device = orientation.unwrap();
                        let current = device * screen;
                        // The first pose looks straight ahead: the world turns with the start yaw
                        let offset = *yaw_offset.get_or_insert_with(|| {
                            let forward = current * -Vec3::Z;
                            Quat::from_rotation_y(-f32::atan2(-forward.x, -forward.z))
                        });
                        let sample = HeadSample {
                            orientation: world_turn * offset * current,
                            // The rate is in the device frame
                            angular_velocity: world_turn * offset * device * rate,
                            sample_time: clock::boottime_to_monotonic(event.timestamp),
                        };
                        *latest.lock().unwrap() = Some(sample);
                        if sample.sample_time.saturating_sub(last_sent) >= SEND_INTERVAL {
                            last_sent = sample.sample_time;
                            send(core, &sample);
                        }
                    }
                    _ => (),
                }
            }
        }
    }
}

fn send(core: &ClientCoreContext, sample: &HeadSample) {
    let motion = DeviceMotion {
        pose: Pose { orientation: sample.orientation, position: HEAD_POSITION },
        linear_velocity: Vec3::ZERO,
        angular_velocity: sample.angular_velocity,
    };
    // The sample time names the frame: the host's frame for this pose comes back with it
    core.send_tracking(sample.sample_time, vec![(*HEAD_ID, motion)], [None, None], Default::default());
}
