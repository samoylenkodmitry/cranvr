use super::*;

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|i| (a[i] - b[i]).abs() < 1e-5)
}

/// A turn of `degrees` about an axis, as a quaternion
fn about(axis: [f32; 3], degrees: f32) -> Quaternion {
    let (sine, cosine) = (degrees.to_radians() / 2.0).sin_cos();
    Quaternion::new(axis[0] * sine, axis[1] * sine, axis[2] * sine, cosine)
}

fn look(q: Quaternion) -> [f32; 3] {
    forward([q.x, q.y, q.z, q.w])
}

#[test]
fn a_tablet_that_lies_face_up_looks_down() {
    // The identity sample is a device that lies face up: its back looks at the floor. The
    // Galaxy Tab on the desk reports this, in any orientation of the display.
    for rotation in [
        DisplayRotation::Rotation0,
        DisplayRotation::Rotation90,
        DisplayRotation::Rotation180,
        DisplayRotation::Rotation270,
    ] {
        let head = head_in_room(Quaternion::IDENTITY, rotation);
        assert!(
            near(look(head), [0.0, -1.0, 0.0]),
            "{rotation:?}: {:?}",
            look(head)
        );
    }
}

#[test]
fn a_portrait_device_held_upright_looks_along_the_horizon_with_its_top_up() {
    // The device's y axis points up and its back faces away from the person
    let head = head_in_room(about([1.0, 0.0, 0.0], 90.0), DisplayRotation::Rotation0);
    assert!(near(look(head), [0.0, 0.0, -1.0]));
    assert!(near(head.rotate([0.0, 1.0, 0.0]), [0.0, 1.0, 0.0]));
    assert!(near(head.rotate([1.0, 0.0, 0.0]), [1.0, 0.0, 0.0]));
}

#[test]
fn a_device_turned_to_landscape_looks_along_the_horizon_too() {
    // Held upright with its natural top edge at the left (a quarter turn counter-clockwise):
    // the device's x axis points up and its y axis points to the viewer's left
    let device = about([1.0, 0.0, 0.0], 90.0) * about([0.0, 0.0, 1.0], 90.0);
    let head = head_in_room(device, DisplayRotation::Rotation90);
    assert!(near(look(head), [0.0, 0.0, -1.0]), "{:?}", look(head));
    assert!(near(head.rotate([0.0, 1.0, 0.0]), [0.0, 1.0, 0.0]));
    assert!(near(head.rotate([1.0, 0.0, 0.0]), [1.0, 0.0, 0.0]));
    // Read with the wrong display rotation, the head rolls by a quarter turn
    let wrong = head_in_room(device, DisplayRotation::Rotation0);
    assert!(
        near(wrong.rotate([0.0, 1.0, 0.0]), [1.0, 0.0, 0.0])
            || near(wrong.rotate([0.0, 1.0, 0.0]), [-1.0, 0.0, 0.0])
    );
}

#[test]
fn a_turn_of_the_device_to_the_left_turns_the_head_to_the_left() {
    // The person turns the upright device about the vertical, counter-clockwise from above:
    // the view moves from -z toward -x
    let upright = about([1.0, 0.0, 0.0], 90.0);
    let turned = about([0.0, 0.0, 1.0], 30.0) * upright;
    let head = head_in_room(turned, DisplayRotation::Rotation0);
    let [x, y, z] = look(head);
    assert!(x < -0.49 && y.abs() < 1e-5 && z < -0.86, "{x} {y} {z}");
}

#[test]
fn the_first_look_along_the_horizon_becomes_the_front() {
    // The sensors' heading is arbitrary: here the device looks along the reference frame's x
    let device = about([0.0, 0.0, 1.0], -90.0) * about([1.0, 0.0, 0.0], 90.0);
    let head = head_in_room(device, DisplayRotation::Rotation0);
    assert!(look(head)[0].abs() > 0.99);
    let fix = heading_fix(head).unwrap();
    assert!(near(look((fix * head).normalized()), [0.0, 0.0, -1.0]));
    // A head that looks at the floor has no heading to fix
    assert!(
        heading_fix(head_in_room(
            Quaternion::IDENTITY,
            DisplayRotation::Rotation0
        ))
        .is_none()
    );
}

#[test]
fn the_views_have_the_screens_shape_and_ninety_degrees_across() {
    let views = phone_views([2800, 1752]);
    assert_eq!(views.fov[0], views.fov[1]);
    assert_eq!(views.eye_pose[0].position, [0.0; 3]);
    let fov = views.fov[0];
    assert!((fov.right - FRAC_PI_4).abs() < 1e-6 && (fov.left + FRAC_PI_4).abs() < 1e-6);
    // The tangent ratio is the aspect ratio of the screen
    let aspect = fov.right.tan() / fov.up.tan();
    assert!((aspect - 2800.0 / 1752.0).abs() < 1e-4, "{aspect}");
    // A screen in portrait gives the same landscape views
    assert_eq!(phone_views([1752, 2800]), views);
}

#[test]
fn the_eye_keeps_the_screens_aspect_with_even_sides_and_a_capped_width() {
    let [w, h] = eye_size([2800, 1752]);
    assert_eq!((w, h % 2, w % 2), (1920, 0, 0));
    assert!(
        (w as f32 / h as f32 - 2800.0 / 1752.0).abs() < 0.01,
        "{w}x{h}"
    );
    // A small screen keeps its own size
    assert_eq!(eye_size([1280, 720]), [1280, 720]);
    assert_eq!(eye_size([720, 1280]), [1280, 720]);
}

#[test]
fn a_measured_rate_snaps_to_the_rate_of_the_screen() {
    assert_eq!(snap_rate(59.6), 60.0);
    assert_eq!(snap_rate(57.0), 60.0);
    assert_eq!(snap_rate(119.3), 120.0);
    assert_eq!(snap_rate(90.4), 90.0);
    assert_eq!(snap_rate(80.0), 80.0);
}

fn input(now: Nanos, sample: Option<SensorSample>) -> FrameInput {
    FrameInput {
        now,
        sample,
        rotation: DisplayRotation::Rotation0,
        screen: Some([2800, 1752]),
        recenter: false,
    }
}

#[test]
fn the_rate_comes_from_the_frame_intervals_and_the_views_wait_for_it() {
    let mut reader = PhoneReader::default();
    let mut now = 1_000_000_000;
    let mut first = None;
    for frame in 0..60 {
        let read = reader.read(&input(now, None));
        if read.views.is_some() && first.is_none() {
            first = Some(frame);
        }
        now += 8_333_333;
    }
    // 45 intervals need 46 frames
    assert_eq!(first, Some(MIN_INTERVALS));
    let info = reader.info().unwrap();
    assert_eq!((info.refresh_hz, info.screen), (120.0, [2800, 1752]));
    let read = reader.read(&input(now, None));
    assert_eq!(read.display.period, 8_333_333);
    assert_eq!(read.display.time, now + 8_333_333);
}

#[test]
fn one_long_pause_does_not_change_the_rate() {
    let mut reader = PhoneReader::default();
    let mut now = 0;
    for frame in 0..60 {
        reader.read(&input(now, None));
        now += if frame == 40 { 900_000_000 } else { 16_666_667 };
    }
    assert_eq!(reader.refresh_hz(), Some(60.0));
}

#[test]
fn the_sample_time_is_the_sensors_and_the_pose_is_the_heads_at_the_display() {
    let mut reader = PhoneReader::default();
    // A device that turns about the vertical at 0.5 rad/s, with a sample every 4 ms
    let upright = about([1.0, 0.0, 0.0], 90.0);
    let rotation_at =
        |taken: Nanos| about([0.0, 0.0, 1.0], (0.5 * taken as f32 / 1e9).to_degrees()) * upright;
    let mut last = None;
    for step in 0..20u64 {
        let taken = step * 4_000_000;
        let sample = SensorSample {
            rotation: rotation_at(taken),
            taken,
        };
        last = Some(reader.read(&input(taken + 3_000_000, Some(sample))));
    }
    let frame = last.unwrap().frame.unwrap();
    assert_eq!(frame.sample_time, 76_000_000);
    assert_eq!(frame.head.pose.position, [0.0, HEAD_HEIGHT, 0.0]);
    // A turn about +z of the reference frame is a turn about +y of the room
    let w = frame.head.angular_velocity;
    assert!(
        (w[1] - 0.5).abs() < 0.02 && w[0].abs() < 0.02 && w[2].abs() < 0.02,
        "{w:?}"
    );
    // The sent pose lies ahead of the sample by the time to the display: yaw grows as the
    // device turns to the left
    let yaw_of = |orientation: [f32; 4]| {
        let [x, _, z] = forward(orientation);
        f32::atan2(-x, -z)
    };
    let at_sample = {
        let q = head_in_room(rotation_at(76_000_000), DisplayRotation::Rotation0);
        yaw_of([q.x, q.y, q.z, q.w])
    };
    let lead = (frame.display_time - frame.sample_time) as f32 / 1e9;
    assert!(lead > 0.015 && lead < MAX_PREDICT);
    assert!((yaw_of(frame.head.pose.orientation) - at_sample - 0.5 * lead).abs() < 0.01);
}

#[test]
fn a_recenter_puts_the_front_where_the_screen_points() {
    let mut reader = PhoneReader::default();
    let upright = about([1.0, 0.0, 0.0], 90.0);
    let sample = |rotation: Quaternion, taken| Some(SensorSample { rotation, taken });
    reader.read(&input(10_000_000, sample(upright, 9_000_000)));
    // The person turns the device by 40 degrees about the vertical
    let turned = about([0.0, 0.0, 1.0], 40.0) * upright;
    let read = reader.read(&input(20_000_000, sample(turned, 19_000_000)));
    let [x, _, z] = forward(read.frame.unwrap().head.pose.orientation);
    assert!(x < -0.5 && z > -0.9, "{x} {z}");
    let mut recenter = input(30_000_000, sample(turned, 29_000_000));
    recenter.recenter = true;
    let read = reader.read(&recenter);
    assert!(near(
        forward(read.frame.unwrap().head.pose.orientation),
        [0.0, 0.0, -1.0]
    ));
}

#[test]
fn a_device_with_no_sensor_looks_straight_ahead_and_keeps_the_hosts_frame_running() {
    let mut reader = PhoneReader::default();
    let read = reader.read(&input(5_000_000, None));
    let frame = read.frame.unwrap();
    assert_eq!(frame.sample_time, 5_000_000);
    assert!(near(forward(frame.head.pose.orientation), [0.0, 0.0, -1.0]));
    assert_eq!(frame.head.angular_velocity, [0.0; 3]);
    assert!(frame.left_grip.is_none() && frame.right_aim.is_none());
}
