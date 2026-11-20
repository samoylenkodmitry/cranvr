//! Which panels of the app have the pointer over them.
//!
//! The rule for the hands that the app holds back from the game (`hold.rs`) needs to know whether
//! a controller's ray is on a panel of the app. Cranpose gives the app that knowledge only through
//! the pointer events of its panels. A panel that holds composables marks its root with
//! [`hover_modifier`]: Cranpose's `hoverable` reports the pointer's entry and exit. A surface (a
//! display) reports the same from its pointer events through [`set`].
//!
//! A grab of a panel's handle (the sphere under it, the sphere at its corner) is not a hover: Cranpose
//! reports the grab's start and end only. The panels note them with [`set`] on [`HANDLE`]. The pull
//! that starts a grab still leaks to the game: the trigger rises before the grab's start. That is a gap
//! of Cranpose's API (see `hold.rs`).
//!
//! The registry is a bit set in an atomic, so the headset's frame loop reads it without a lock.

use cranpose::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// The names of the panels, as bit numbers
pub const CONSOLE: u32 = 0;
pub const CONNECTION: u32 = 1;
pub const STATS: u32 = 2;
pub const MENU: u32 = 3;
pub const STRIP: u32 = 4;
/// A controller holds a panel by its handle, to carry it or to resize it. The ray is on the handle,
/// which gives the app no hover, so the grab itself stands for it from its start to its end.
pub const HANDLE: u32 = 5;
/// The first bit of the displays: the display of stream `s` has bit `DISPLAY + s`
pub const DISPLAY: u32 = 8;

static HOVERED: AtomicU64 = AtomicU64::new(0);
/// When a pointer was last over a panel, in milliseconds on the headset's clock, or 0
static LAST_MS: AtomicU64 = AtomicU64::new(0);

/// Marks the panel `id` as hovered or not.
pub fn set(id: u32, hovered: bool) {
    let bit = 1u64 << id;
    if hovered {
        HOVERED.fetch_or(bit, Ordering::Relaxed);
    } else {
        HOVERED.fetch_and(!bit, Ordering::Relaxed);
    }
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

/// Whether a pointer is over any panel of the app.
pub fn any() -> bool {
    HOVERED.load(Ordering::Relaxed) != 0
}

/// Forgets every hover: the app lost the input focus, so no pointer points at anything.
pub fn clear() {
    HOVERED.store(0, Ordering::Relaxed);
}

/// The modifier that reports whether the pointer is over a panel's root. The panel puts it on its
/// root composable. A panel that leaves the composition while hovered ends its hover.
#[composable]
pub fn hover_modifier(id: u32) -> Modifier {
    let source = rememberMutableInteractionSource();
    let hovered = collect_is_hovered_as_state(&source).get();
    // The effect runs again when the hover changes: it ends the old hover and notes the new one. It
    // also ends the hover of a panel that leaves the composition.
    DisposableEffect((id, hovered), move |_| {
        set(id, hovered);
        DisposableEffectResult::new(move || set(id, false))
    });
    Modifier::empty().hoverable(source, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panel_is_hovered_between_its_enter_and_its_exit() {
        clear();
        assert!(!any());
        set(MENU, true);
        set(DISPLAY + 2, true);
        assert!(any());
        set(MENU, false);
        assert!(any());
        set(DISPLAY + 2, false);
        assert!(!any());
        set(STRIP, true);
        clear();
        assert!(!any());
        assert_eq!(idle_for(5000), None);
        mark(1000);
        assert_eq!(idle_for(5000), Some(std::time::Duration::from_secs(4)));
    }
}
