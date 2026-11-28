use super::*;

fn room(facing: f32, head: Option<f32>) -> ([f32; 3], f32) {
    match place(Some(facing), head) {
        Place::Room { offset, turn_deg } => (offset, turn_deg),
        other => panic!("a screen with a direction stands in the room, not {other:?}"),
    }
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01)
}

#[test]
fn a_screen_at_the_game_s_forward_direction_stands_ahead_of_the_room_s_origin() {
    // The room's subspace starts a meter ahead of the origin, so 1.5 m ahead is half a meter beyond it
    let (offset, turn) = room(0.0, Some(0.12));
    assert!(close(offset, [0.0, 120.0, -500.0]), "{offset:?}");
    assert_eq!(turn, 0.0);
    // Without a tracked head the screen stands at the height of the head at the app's start
    assert!(close(room(0.0, None).0, [0.0, 0.0, -500.0]));
}

#[test]
fn a_turned_screen_stands_on_the_circle_and_faces_the_middle() {
    // A quarter turn toward the left: 1.5 m to the left of the origin, which is 1 m behind the subspace's
    let (offset, turn) = room(90.0, Some(0.0));
    assert!(close(offset, [-1500.0, 0.0, 1000.0]), "{offset:?}");
    assert_eq!(turn, 90.0, "the panel turns with its place, so its face looks at the middle");
    let (behind, _) = room(180.0, Some(0.0));
    assert!(close(behind, [0.0, 0.0, 2500.0]), "{behind:?}");
}

#[test]
fn a_screen_without_a_direction_stands_in_front_of_the_head() {
    // A subspace that follows the view starts a meter ahead of the head
    assert_eq!(place(None, Some(0.4)), Place::View { offset: [0.0, 0.0, -500.0] });
}
