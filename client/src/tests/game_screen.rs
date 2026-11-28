use super::*;

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01)
}

#[test]
fn the_screen_stands_ahead_of_the_head() {
    // The subspace's origin lies a meter ahead of the head, so 1.5 m ahead is half a meter beyond it
    let (offset, turn) = place(Some(0.0));
    assert!(close(offset, [0.0, 0.0, -500.0]), "{offset:?}");
    assert_eq!(turn, 0.0);
    assert_eq!(place(None), place(Some(0.0)), "a screen without a direction stands in front of the head");
}

#[test]
fn a_turned_screen_stands_on_the_circle_around_the_head_and_faces_it() {
    // A quarter turn toward the left: 1.5 m to the left of the head, which is 1 m behind the origin
    let (offset, turn) = place(Some(90.0));
    assert!(close(offset, [-1500.0, 0.0, 1000.0]), "{offset:?}");
    assert_eq!(turn, 90.0, "the panel turns with its place, so its face looks at the head");
    let (behind, _) = place(Some(180.0));
    assert!(close(behind, [0.0, 0.0, 2500.0]), "{behind:?}");
}
