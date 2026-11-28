//! The probe as a client of several displays (`--displays N`) and of displays that show inside a
//! game for a while (`--displays-in-game`).
//!
//! The probe adds displays until it has N, wishes the video of each, decodes each stream with its
//! own decoder, plays the pointer script on each and, near the end, removes the last display that it
//! added and checks that the host's list shrinks.
//!
//! `--displays-in-game` plays a script against a game that runs: for the first seconds the probe
//! wishes no display, so the host must send nothing and the probe must receive nothing. Then it
//! shows its displays for a while. Then it hides them again and the pictures must stop. The probe
//! counts the units that arrive in each part and prints the counts for the e2e's verdict.

use crate::native_desktop::DesktopProbe;
use cranvr_proto::{
    link::ClientLink,
    message::{ClientMsg, DisplayList, DisplayRequest, DESKTOP_STREAM},
};
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc, Mutex,
};

/// What the probe does with the displays.
#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayPlan {
    /// How many displays the probe has in all, the first one included
    pub wanted: u8,
    /// The script of a game's run: show for a while, then hide (see the module's text)
    pub in_game: bool,
    /// The most pictures a second that every wish asks for, in place of the client's own rule
    pub max_fps: Option<u8>,
    /// The widest picture that every wish asks for, in place of the client's own rule
    pub max_width: Option<u32>,
}

pub struct DisplaySet {
    plan: DisplayPlan,
    /// The displays after the first, by stream
    extras: Mutex<Vec<Arc<DesktopProbe>>>,
    list: Mutex<Option<DisplayList>>,
    /// An Add that the host has not answered with a longer list yet
    adding: AtomicBool,
    /// Whether the displays are wished now
    wished: AtomicBool,
    /// How many displays the host's last list held, and the most that it held
    seen: AtomicU8,
    most: AtomicU8,
    removed: AtomicBool,
    /// The probes of displays that left the list: their numbers count at the end
    gone: Mutex<Vec<Arc<DesktopProbe>>>,
}

impl DisplaySet {
    pub fn new(plan: DisplayPlan) -> Arc<Self> {
        Arc::new(Self {
            plan,
            extras: Mutex::new(Vec::new()),
            list: Mutex::new(None),
            adding: AtomicBool::new(false),
            wished: AtomicBool::new(!plan.in_game),
            seen: AtomicU8::new(0),
            most: AtomicU8::new(0),
            removed: AtomicBool::new(false),
            gone: Mutex::new(Vec::new()),
        })
    }

    /// The probe of the display on `stream`, other than the first.
    pub fn extra(&self, stream: u8) -> Option<Arc<DesktopProbe>> {
        self.extras.lock().unwrap().iter().find(|p| p.stream == stream).cloned()
    }

    /// The host's list of displays came. The probe makes a probe for each new display, wishes its
    /// video and starts its pointer script, and asks for one more display while it has too few.
    pub fn on_list(&self, list: DisplayList, link: &ClientLink, start_pointer_script: &dyn Fn(u8)) {
        println!(
            "probe: the host lists {} displays ({}){}{}",
            list.displays.len(),
            list.displays.iter().map(|d| format!("stream {} {} {}x{} points{}", d.stream, d.name, d.points[0], d.points[1], if d.removable { ", removable" } else { "" })).collect::<Vec<_>>().join("; "),
            if list.can_add { ", it can add one" } else { "" },
            list.notice.as_deref().map_or(String::new(), |n| format!(", it says: {n}")),
        );
        let count = list.displays.len() as u8;
        let grew = count > self.seen.swap(count, Ordering::Relaxed);
        self.most.fetch_max(count, Ordering::Relaxed);
        if grew {
            self.adding.store(false, Ordering::Relaxed);
        }
        for info in &list.displays {
            if info.stream == DESKTOP_STREAM || self.extra(info.stream).is_some() {
                continue;
            }
            let probe = DesktopProbe::new(info.stream, None);
            self.extras.lock().unwrap().push(Arc::clone(&probe));
            if self.wished.load(Ordering::Relaxed) {
                probe.wish(link, true, self.wish_size());
                println!("probe: asked the host for {} at up to {} pixels and {} pictures a second", probe.name(), self.wish_size().0, self.wish_size().1);
            }
            start_pointer_script(info.stream);
        }
        // A display that left the list is gone for good: forget its probe, so a later display on its
        // stream starts a decoder of its own
        let mut extras = self.extras.lock().unwrap();
        let (kept, gone): (Vec<_>, Vec<_>) = extras.drain(..).partition(|p| list.displays.iter().any(|d| d.stream == p.stream));
        *extras = kept;
        self.gone.lock().unwrap().extend(gone);
        drop(extras);
        *self.list.lock().unwrap() = Some(list.clone());
        if usize::from(self.plan.wanted) > list.displays.len() && list.can_add && !self.removed.load(Ordering::Relaxed) && !self.adding.swap(true, Ordering::Relaxed) {
            println!("probe: asks the host to add a display");
            link.duplex.send_control(&ClientMsg::Display(DisplayRequest::Add)).ok();
        }
    }

    /// What the probe asks of each display, as the headset's client does (`client/src/display_wish.rs`):
    /// one display gets the full size at 60 pictures a second, two get the full size at 30, more get 2560
    /// pixels across at 30, and displays that show over a game get 2560 at 30. The Mac then encodes no
    /// more pixels a second than it does for one display.
    pub fn wish_size(&self) -> (u32, u8) {
        let (width, fps) = match (self.plan.in_game, self.plan.wanted) {
            (true, _) => (2560, 30),
            (false, 0..=1) => (3840, 60),
            (false, 2) => (3840, 30),
            (false, _) => (2560, 30),
        };
        (self.plan.max_width.unwrap_or(width), self.plan.max_fps.unwrap_or(fps))
    }

    /// Wishes or ends the wish of every display.
    pub fn set_wished(&self, link: &ClientLink, first: Option<&Arc<DesktopProbe>>, enabled: bool) {
        self.wished.store(enabled, Ordering::Relaxed);
        let size = self.wish_size();
        if let Some(first) = first {
            first.wish(link, enabled, size);
        }
        for probe in self.extras.lock().unwrap().iter() {
            probe.wish(link, enabled, size);
        }
        println!("probe: {} the video of every display", if enabled { "wishes" } else { "ends the wish for" });
    }

    /// Units that arrived on every display stream, and pictures decoded.
    pub fn counts(&self, first: Option<&Arc<DesktopProbe>>) -> (u64, u64) {
        let mut sum = first.map_or((0, 0), |p| p.counts());
        for probe in self.extras.lock().unwrap().iter().chain(self.gone.lock().unwrap().iter()) {
            let (received, pictures) = probe.counts();
            sum = (sum.0 + received, sum.1 + pictures);
        }
        sum
    }

    /// Removes the last display that the probe added.
    pub fn remove_last(&self, link: &ClientLink) {
        let last = self.extras.lock().unwrap().last().map(|p| p.stream);
        if let Some(stream) = last {
            println!("probe: asks the host to remove display {}", stream - DESKTOP_STREAM + 1);
            self.removed.store(true, Ordering::Relaxed);
            link.duplex.send_control(&ClientMsg::Display(DisplayRequest::Remove { stream })).ok();
        }
    }

    /// The summary of the extra displays, and whether they all got pictures.
    pub fn summary(&self) -> bool {
        let mut extras = self.extras.lock().unwrap().clone();
        extras.extend(self.gone.lock().unwrap().iter().cloned());
        let mut ok = true;
        for probe in &extras {
            ok &= probe.summary();
        }
        let most = self.most.load(Ordering::Relaxed);
        if self.plan.wanted > 1 {
            let kept = self.seen.load(Ordering::Relaxed);
            println!(
                "probe: displays: the host listed up to {most} of the {} wanted; the last list held {kept}; the probe asked for a removal: {}",
                self.plan.wanted,
                self.removed.load(Ordering::Relaxed)
            );
            ok &= most >= self.plan.wanted;
            if self.removed.load(Ordering::Relaxed) {
                // The list must have shrunk by one after the removal
                ok &= kept + 1 == most;
            }
        }
        ok
    }
}
