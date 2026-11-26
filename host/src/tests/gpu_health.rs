use super::*;
use objc2_metal::MTLCommandBufferError;

fn error(code: MTLCommandBufferError, text: &str) -> GpuError {
    GpuError { code: code.0, text: text.into(), wall_ms: 900.0, gpu_ms: 880.0 }
}

fn stall() -> GpuError {
    // The code that Metal gives "Impacting Interactivity" is not the point here: anything but
    // access revoked is a stall of one frame
    error(MTLCommandBufferError::Timeout, "Impacting Interactivity (0000000e:kIOGPUCommandBufferCallbackErrorImpactingInteractivity)")
}

fn ignored() -> GpuError {
    error(MTLCommandBufferError::AccessRevoked, "Ignored (for causing prior/excessive GPU errors) (00000004:kIOGPUCommandBufferCallbackErrorSubmissionsIgnored)")
}

/// The stalls of the log of 2026-11-26: one or two failed frames between frames that drew. The
/// host goes on drawing through all of them.
#[test]
fn stalls_between_frames_that_draw_keep_the_host() {
    let mut health = GpuHealth::default();
    let start = Instant::now();
    for round in 0..100u64 {
        let now = start + Duration::from_secs(6 * round);
        assert_eq!(health.failed(&stall(), now).0, Verdict::Continue);
        assert_eq!(health.failed(&stall(), now + Duration::from_millis(800)).0, Verdict::Continue);
        health.drew(1.4, 1.3, now + Duration::from_secs(1));
    }
}

/// An ignored host fails every frame at once. It tries new queues once, then ends.
#[test]
fn an_ignored_host_tries_new_queues_once_and_then_ends() {
    let mut health = GpuHealth::default();
    let now = Instant::now();
    assert_eq!(health.failed(&ignored(), now).0, Verdict::NewQueues);
    assert!(matches!(health.failed(&ignored(), now).0, Verdict::Exit(_)));
}

/// Frames that fail in a row with another code end the host too, after a quarter of a second at
/// 120 Hz: macOS may name the state of an ignored host otherwise.
#[test]
fn thirty_failed_frames_in_a_row_end_the_host() {
    let mut health = GpuHealth::default();
    let now = Instant::now();
    for _ in 0..29 {
        assert_eq!(health.failed(&stall(), now).0, Verdict::Continue);
    }
    assert!(matches!(health.failed(&stall(), now).0, Verdict::Exit(_)));
}

/// The log gets one error line a second, which counts the lines left out.
#[test]
fn errors_get_one_line_a_second() {
    let mut health = GpuHealth::default();
    let start = Instant::now();
    let lines: Vec<String> = (0..20u64).filter_map(|i| health.failed(&stall(), start + Duration::from_millis(100 * i)).1).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains("Impacting Interactivity") && lines[0].contains("900 ms"), "{}", lines[0]);
    assert!(lines[1].ends_with("(9 more since the last line)"), "{}", lines[1]);
}

/// A slow frame that drew is logged, at most once in 5 s: it shows that the frame waited behind
/// another process's work.
#[test]
fn slow_frames_get_one_line_in_five_seconds() {
    let mut health = GpuHealth::default();
    let start = Instant::now();
    assert!(health.drew(1.5, 1.4, start).is_none());
    let line = health.drew(850.0, 849.0, start).expect("a line for the slow frame");
    assert!(line.contains("850 ms"), "{line}");
    assert!(health.drew(400.0, 399.0, start + Duration::from_secs(1)).is_none());
    assert!(health.drew(400.0, 399.0, start + Duration::from_secs(6)).unwrap().ends_with("(1 more since the last line)"));
}
