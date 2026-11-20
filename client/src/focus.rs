//! How much of the headset the app has, as the host needs to know it.
//!
//! Cranpose publishes the headset session's state as the app's lifecycle (`cranpose::current_lifecycle_state`):
//! `Resumed` while the app has the input focus, `Paused` while it is on display without it (under
//! the system's menu, or beside another app's panel), `Stopped` while the headset shows none of
//! it. The backend itself follows Meta's focus rule for the picture: the panels, the streamed scene
//! and the surfaces keep drawing, the controllers and the hands point at nothing and press
//! nothing. This file maps the lifecycle to the protocol's [`Focus`]. The sender of the tracking
//! (`tracking.rs`) stops its tracking and input while the app is not focused and tells the host,
//! which pauses the stream. The link stays up.
//!
//! A flat window never has the headset's focus rules: a window of the client on a Mac keeps
//! sending its placeholder samples, whichever window the user works in.

use cranpose::{current_lifecycle_state, LifecycleState};
use cranvr_proto::message::Focus;

/// The focus that a lifecycle state means. `Created` is the app before its first show: it counts as
/// focused, so a start that never leaves it (a window, a test) keeps its link as before.
pub fn of_lifecycle(state: LifecycleState) -> Focus {
    match state {
        LifecycleState::Resumed | LifecycleState::Created => Focus::Focused,
        LifecycleState::Started | LifecycleState::Paused => Focus::Visible,
        LifecycleState::Stopped | LifecycleState::Destroyed => Focus::Hidden,
    }
}

/// The focus now. Any thread may ask: the lifecycle is an atomic.
pub fn current() -> Focus {
    of_lifecycle(current_lifecycle_state())
}

/// The words for the log and the stats panel.
pub fn describe(focus: Focus) -> &'static str {
    match focus {
        Focus::Focused => "the app has the focus",
        Focus::Visible => "the app is on display without the focus (the system's menu or another panel has it)",
        Focus::Hidden => "the headset shows none of the app",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lifecycle_of_a_headset_session_gives_the_focus() {
        // FOCUSED is Resumed, VISIBLE is Paused, and READY, SYNCHRONIZED, STOPPING and IDLE are Stopped
        assert_eq!(of_lifecycle(LifecycleState::Resumed), Focus::Focused);
        assert_eq!(of_lifecycle(LifecycleState::Paused), Focus::Visible);
        assert_eq!(of_lifecycle(LifecycleState::Stopped), Focus::Hidden);
        assert_eq!(of_lifecycle(LifecycleState::Destroyed), Focus::Hidden);
        // Before the first show, and in a window that never changes it, the link keeps running
        assert_eq!(of_lifecycle(LifecycleState::Created), Focus::Focused);
        assert!(of_lifecycle(LifecycleState::Started) != Focus::Focused);
    }
}
