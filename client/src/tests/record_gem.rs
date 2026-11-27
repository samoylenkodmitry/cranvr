use super::{blocked, look, GREEN, RED};
use cranpose::prelude::Color;
use cranpose_xr::{GemGlow, HologramColors};
use cranvr_proto::message::Recording;

const GLASS: Color = HologramColors::CYAN.glass;

// The four looks of the gem: the theme's glass, green under a pointer, red and pulsing while a
// recording runs, dim while no recording can start
#[test]
fn the_gem_says_what_a_press_does_by_its_color() {
    let ready = Recording::default();
    assert_eq!(look(Some(&ready), false, GLASS).color, GLASS);
    assert_eq!(look(Some(&ready), true, GLASS).color, GREEN);
    let running = Recording { running: true, ..Default::default() };
    for hovered in [false, true] {
        let seen = look(Some(&running), hovered, GLASS);
        assert_eq!((seen.color, seen.glow), (RED, GemGlow::Pulse), "red whether pointed at or not");
    }
    let cable = Recording { blocked: Some("Recording needs the headset on the USB cable with USB debugging".into()), ..Default::default() };
    for state in [Some(&cable), None] {
        for hovered in [false, true] {
            let dim = look(state, hovered, GLASS).color;
            assert!(dim.2 < GLASS.2 * 0.5 && dim.0 <= GLASS.0, "dim, pointed at or not: {dim:?}");
        }
    }
}

// A press tells why it cannot start; a running recording always stops
#[test]
fn a_press_while_nothing_can_start_names_the_reason() {
    assert_eq!(blocked(None).as_deref(), Some("Connect to the Mac to record"));
    let cable = Recording { blocked: Some("Recording needs the USB cable".into()), ..Default::default() };
    assert_eq!(blocked(Some(&cable)).as_deref(), Some("Recording needs the USB cable"));
    let running = Recording { running: true, blocked: Some("stale".into()), ..Default::default() };
    assert_eq!(blocked(Some(&running)), None);
    assert_eq!(blocked(Some(&Recording::default())), None);
}
