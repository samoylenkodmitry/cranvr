//! Which panels of the app each hand's pointer is over.
//!
//! The rule for the hands that the app holds back from the game (`hold.rs`) needs to know which
//! hand's ray is on a panel of the app. Cranpose gives each hand a pointer of its own. A pointer event
//! carries the id of its pointer: `PointerId(1)` is the left hand and `PointerId(2)` the right hand. A
//! panel that holds composables marks its root with [`hover_modifier`], which reads the events of the
//! panel's pointers. A surface (a display) reports the same from its own pointer events through
//! [`set`].
//!
//! An event with another id comes from the window's mouse or from a Cranpose that has one pointer.
//! It counts for both hands: the app cannot tell which hand points, so it holds back both.
//!
//! A grab of a panel's handle (the sphere under it, the sphere at its corner) is not a hover: Cranpose
//! reports the grab's start and end only, and not the hand. The panels note them with [`set_handle`],
//! and the hold takes the hands whose triggers are down for the hands that carry. The pull that starts a
//! grab still leaks to the game: the trigger rises before the grab's start, and the app is told of no
//! ray on a handle. That is a gap of Cranpose's API (see `hold.rs`).
//!
//! The registry is a set of bits in atomics, so the headset's frame loop reads it without a lock.

use cranpose::{prelude::*, PointerEventKind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The names of the panels, as bit numbers
pub const CONSOLE: u32 = 0;
pub const CONNECTION: u32 = 1;
pub const STATS: u32 = 2;
pub const MENU: u32 = 3;
pub const STRIP: u32 = 4;
/// The ball on an open palm (`palm.rs`)
pub const PALM: u32 = 5;
/// The first bit of the displays: the display of stream `s` has bit `DISPLAY + s`
pub const DISPLAY: u32 = 8;

/// The panels that each pointer is over: the left hand's, the right hand's and the one that names no
/// hand
static HOVERED: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
/// A controller holds a panel by its handle
static HANDLE: AtomicBool = AtomicBool::new(false);
/// When a pointer was last over a panel, in milliseconds on the headset's clock, or 0
static LAST_MS: AtomicU64 = AtomicU64::new(0);

/// The slot of a pointer: 0 for the left hand, 1 for the right hand and 2 for any other pointer.
fn slot_of(pointer: u64) -> usize {
    match pointer {
        1 => 0,
        2 => 1,
        _ => 2,
    }
}

/// Marks the panel `id` as hovered by the pointer `pointer` (a `PointerId`'s number) or not.
pub fn set(id: u32, pointer: u64, hovered: bool) {
    let bit = 1u64 << id;
    let slot = &HOVERED[slot_of(pointer)];
    if hovered {
        slot.fetch_or(bit, Ordering::Relaxed);
    } else {
        slot.fetch_and(!bit, Ordering::Relaxed);
    }
}

/// Ends the hover of the panel `id` for every pointer: the panel left the composition.
pub fn clear_panel(id: u32) {
    for slot in &HOVERED {
        slot.fetch_and(!(1u64 << id), Ordering::Relaxed);
    }
}

/// Notes that a controller holds a panel by its handle, or lets go.
pub fn set_handle(held: bool) {
    HANDLE.store(held, Ordering::Relaxed);
}

/// Whether a controller holds a panel by its handle.
pub fn handle_held() -> bool {
    HANDLE.load(Ordering::Relaxed)
}

/// Notes that a pointer is over a panel at `now_ms`. The frame loop calls it for each frame in which
/// a panel is hovered.
pub fn mark(now_ms: u64) {
    LAST_MS.store(now_ms.max(1), Ordering::Relaxed);
}

/// How long no pointer has been over a panel, or `None` before the first time.
pub fn idle_for(now_ms: u64) -> Option<std::time::Duration> {
    match LAST_MS.load(Ordering::Relaxed) {
        0 => None,
        last => Some(std::time::Duration::from_millis(now_ms.saturating_sub(last))),
    }
}

/// Whether the left hand's and the right hand's pointers are over a panel of the app. A pointer that
/// names no hand counts for both.
pub fn hands() -> [bool; 2] {
    let (left, right, other) = (HOVERED[0].load(Ordering::Relaxed), HOVERED[1].load(Ordering::Relaxed), HOVERED[2].load(Ordering::Relaxed));
    [left | other != 0, right | other != 0]
}

/// The bits of the hand menu and of its balls
const MENU_BITS: u64 = 1 << MENU | 1 << STRIP | 1 << PALM;

/// Whether the left hand's and the right hand's pointers are over a panel of the app other than the
/// hand menu and its ball. A pointer that names no hand counts for both.
pub fn hands_off_menu() -> [bool; 2] {
    let other = HOVERED[2].load(Ordering::Relaxed) & !MENU_BITS;
    [0, 1].map(|slot| (HOVERED[slot].load(Ordering::Relaxed) & !MENU_BITS) | other != 0)
}

/// Whether any pointer is over the hand menu or its ball.
pub fn on_menu() -> bool {
    HOVERED.iter().any(|slot| slot.load(Ordering::Relaxed) & MENU_BITS != 0)
}

/// Forgets every hover: the app lost the input focus, so no pointer points at anything.
pub fn clear() {
    for slot in &HOVERED {
        slot.store(0, Ordering::Relaxed);
    }
    HANDLE.store(false, Ordering::Relaxed);
}

/// What a pointer event does to the hover of a panel: `Some(true)` while the pointer is over it,
/// `Some(false)` when it left, and `None` for an event that says nothing of it.
pub fn hover_of(kind: PointerEventKind) -> Option<bool> {
    match kind {
        PointerEventKind::Exit => Some(false),
        PointerEventKind::Enter | PointerEventKind::Move | PointerEventKind::Down | PointerEventKind::Up => Some(true),
        _ => None,
    }
}

/// The modifier that reports which hands' pointers are over a panel's root. The panel puts it on its
/// root composable. A panel that leaves the composition while hovered ends its hover.
#[composable]
pub fn hover_modifier(id: u32) -> Modifier {
    DisposableEffect(id, move |_| DisposableEffectResult::new(move || clear_panel(id)));
    Modifier::empty().pointer_input(id, move |scope| async move {
        scope
            .await_pointer_event_scope(move |events| async move {
                loop {
                    let event = events.await_pointer_event().await;
                    if let Some(hovered) = hover_of(event.kind) {
                        set(id, event.id.0, hovered);
                    }
                    // The log says where a ray enters the hand menu or its ball and where it presses
                    // them, so a test reads the panel's place from the point that the ray was aimed at
                    let verb = match event.kind {
                        PointerEventKind::Enter => Some("enters"),
                        PointerEventKind::Down => Some("presses"),
                        _ => None,
                    };
                    if let Some(verb) = verb.filter(|_| [MENU, STRIP, PALM].contains(&id)) {
                        let hand = ["the left", "the right", "a"][slot_of(event.id.0)];
                        let what = if id == MENU { "menu" } else { "ball" };
                        log::info!("{hand} pointer {verb} the hand {what} at {:.0}, {:.0} dp", event.position.x, event.position.y);
                    }
                    #[cfg(all(feature = "scripted-hand", target_os = "android"))]
                    if id == MENU {
                        crate::scripted::note_menu_point(event.kind, event.position);
                    }
                }
            })
            .await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_hand_has_its_own_hover_and_an_unnamed_pointer_counts_for_both() {
        clear();
        assert_eq!(hands(), [false, false]);
        // The right hand's pointer is over the menu
        set(MENU, 2, true);
        assert_eq!(hands(), [false, true]);
        // The left hand's pointer is over a display
        set(DISPLAY + 2, 1, true);
        assert_eq!(hands(), [true, true]);
        set(MENU, 2, false);
        assert_eq!(hands(), [true, false]);
        set(DISPLAY + 2, 1, false);
        assert_eq!(hands(), [false, false]);
        // The window's mouse names no hand: both hands count as hovering while it is over a panel
        set(STRIP, 0, true);
        assert_eq!(hands(), [true, true]);
        clear_panel(STRIP);
        assert_eq!(hands(), [false, false]);
        set(STATS, 2, true);
        clear();
        assert_eq!(hands(), [false, false]);
        // The menu and its ball do not count as other panels
        set(MENU, 1, true);
        assert!(on_menu() && hands_off_menu() == [false, false] && hands() == [true, false]);
        set(CONNECTION, 2, true);
        assert_eq!(hands_off_menu(), [false, true]);
        clear();
        assert!(!on_menu());
        assert_eq!(idle_for(5000), None);
        mark(1000);
        assert_eq!(idle_for(5000), Some(std::time::Duration::from_secs(4)));
    }

    #[test]
    fn a_handle_that_a_controller_holds_is_noted_apart() {
        clear();
        assert!(!handle_held());
        set_handle(true);
        assert!(handle_held() && hands() == [false, false]);
        clear();
        assert!(!handle_held());
    }

    #[test]
    fn an_event_says_that_the_pointer_is_over_the_panel_until_it_exits() {
        assert_eq!(hover_of(PointerEventKind::Enter), Some(true));
        assert_eq!(hover_of(PointerEventKind::Move), Some(true));
        assert_eq!(hover_of(PointerEventKind::Down), Some(true));
        assert_eq!(hover_of(PointerEventKind::Exit), Some(false));
        assert_eq!(hover_of(PointerEventKind::Scroll), None);
    }
}
