// cranvr: the console's face, drawn by Cranpose on the CPU.
//
// The face is a Cranpose composition in a framebuffer: one millimeter on the face is one dp, at
// the bars' 1600 pixels per meter. The ray's point on the face moves the pointer, so the button
// under the ray lights up; a trigger press presses it. A pressed button reports its Action.
//
// The composition lives on one thread. On the Quest that is a thread of its own (ConsoleThread),
// which draws only when the picture changes; the render thread uploads the finished picture.

use crate::console::{self, Action, Cell, Kind, Menu};
use crate::panel::Look;
use alvr_common::error;
use cranpose::framebuffer::{Framebuffer, NextTick, PixelRect, SurfaceError, TouchPhase};
use cranpose::prelude::*;
use cranpose::text::{FontWeight, TextOverflow, TextUnit};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    time::{Duration, Instant},
};
#[cfg(target_os = "android")]
use {crate::bar::Quad, crate::panel::Panel, alvr_graphics::GraphicsContext, openxr as xr};

/// Pixels per millimeter of the face, as the bars draw: 1600 pixels per meter
const DENSITY: f32 = 1.6;
/// The radius of the face's corners in pixels
const CORNER: u32 = 32;
const FACE: Color = Color(0.09, 0.1, 0.12, 1.0);
const IDLE: Color = Color(0.13, 0.14, 0.17, 0.85);
const HOVER: Color = Color(0.36, 0.42, 0.55, 0.95);
const ACTIVE: Color = Color(0.2, 0.42, 0.8, 0.95);
const DIM: Color = Color(0.62, 0.65, 0.7, 1.0);
const WHITE: Color = Color(1.0, 1.0, 1.0, 1.0);

/// Millimeters, from meters
fn mm(meters: f32) -> f32 {
    meters * 1000.0
}

/// What the pointer does on the face, at millimeters from the face's top left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pointer {
    Move(f32, f32),
    Press(f32, f32),
    Release(f32, f32),
    /// The ray left the face
    Leave,
}

fn handle_color(look: Look) -> Color {
    match look {
        Look::Idle => Color(0.75, 0.77, 0.8, 0.6),
        Look::Hover => Color(1.0, 1.0, 1.0, 0.95),
        Look::Active => Color(0.45, 0.7, 1.0, 1.0),
    }
}

fn style(size: f32, color: Color, bold: bool) -> TextStyle {
    TextStyle {
        span_style: SpanStyle {
            color: Some(color),
            font_size: TextUnit::Sp(size),
            font_weight: bold.then_some(FontWeight::BOLD),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Where the pressed buttons put their actions; one composition has one.
#[derive(Clone, Default)]
struct Actions(Rc<RefCell<Vec<Action>>>);

impl Actions {
    fn push(&self, action: Action) {
        self.0.borrow_mut().push(action);
    }
}

impl PartialEq for Actions {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// One line of text that ends with dots when it does not fit.
#[composable]
fn Words(text: String, style: TextStyle) {
    BasicText(text, Modifier::empty(), style, TextOverflow::Ellipsis, false, 1, 1);
}

/// A button: blue while `selected`, lighter while the ray is on it. A press reports `action`.
#[composable]
fn Choice(cell: Cell, height: f32, action: Action, selected: bool, actions: Actions) {
    let source = remember(MutableInteractionSource::new).with(|source| source.clone());
    let hovered = source.collectIsHoveredAsState().value();
    let color = if selected {
        ACTIVE
    } else if hovered {
        HOVER
    } else {
        IDLE
    };
    Box(
        Modifier::empty()
            .width(mm(cell.width))
            .height(height)
            .rounded_corners(0.3 * height)
            .background(color)
            .hoverable(source, true)
            .clickable_on_press(move |_| actions.push(action.clone()), |_| {}),
        BoxSpec::new().content_alignment(Alignment::CENTER),
        move || Words(cell.label.clone(), style(20.0, WHITE, false)),
    );
}

/// One item of a line: a title, a text or a button.
#[composable]
fn Item(cell: Cell, height: f32, actions: Actions) {
    match cell.kind.clone() {
        Kind::Button { action, selected } => Choice(cell, height, action, selected, actions),
        kind => {
            let style = match kind {
                Kind::Title => style(24.0, WHITE, true),
                _ => style(20.0, DIM, false),
            };
            Box(
                Modifier::empty().width(mm(cell.width)).height(height),
                BoxSpec::new().content_alignment(Alignment::CENTER_START),
                move || Words(cell.label.clone(), style.clone()),
            );
        }
    }
}

/// The handle's row: the handle in the middle, in its look.
#[composable]
fn Handle(look: Look) {
    // The lines' gap does not follow the handle's row: the row takes one gap less
    let row = mm(console::HANDLE_ROW);
    let (width, height) = (mm(console::HANDLE_WIDTH), mm(console::HANDLE_HEIGHT));
    Box(
        Modifier::empty().fill_max_width().height(row - mm(console::GAP)),
        BoxSpec::new().content_alignment(Alignment::TOP_CENTER),
        move || {
            Spacer(
                Modifier::empty()
                    .padding_each(0.0, (row - height) / 2.0, 0.0, 0.0)
                    .width(width)
                    .height(height)
                    .rounded_corners(height / 2.0)
                    .background(handle_color(look)),
            );
        },
    );
}

/// The whole face: the handle and the console's lines.
#[composable]
fn Face(menu: MutableState<Menu>, handle: MutableState<Look>, actions: Actions) {
    let lines = console::lines(&menu.value());
    let look = handle.value();
    let margin = mm(console::MARGIN);
    let gap = mm(console::GAP);
    Column(
        Modifier::empty()
            .width(mm(console::WIDTH))
            .background(FACE)
            .padding_each(margin, margin / 2.0, margin, margin / 2.0 + gap),
        ColumnSpec::default().vertical_arrangement(LinearArrangement::SpacedBy(gap)),
        move || {
            Handle(look);
            for line in lines.clone() {
                let actions = actions.clone();
                let height = mm(line.height);
                Row(
                    Modifier::empty().fill_max_width().height(height),
                    RowSpec::default().horizontal_arrangement(LinearArrangement::SpacedBy(gap)),
                    move || {
                        for cell in line.cells.clone() {
                            Item(cell, height, actions.clone());
                        }
                    },
                );
            }
        },
    );
}

/// The console's face as a composition and its picture.
pub struct ConsoleView {
    framebuffer: Framebuffer,
    menu: MutableState<Menu>,
    handle: MutableState<Look>,
    actions: Actions,
    /// The picture, rows from the top
    pixels: Vec<u8>,
    areas: Vec<PixelRect>,
    start: Instant,
}

/// The face's size in pixels for a face `height` meters tall.
fn pixel_size(height: f32) -> (u32, u32) {
    let px = |meters: f32| (mm(meters) * DENSITY).round() as u32;
    (px(console::WIDTH), px(height))
}

impl ConsoleView {
    pub fn new(menu: Menu) -> Result<Self, SurfaceError> {
        let (width, height) = pixel_size(console::height(&menu));
        let actions = Actions::default();
        let sink = actions.clone();
        let states = Rc::new(std::cell::Cell::new(None));
        let held = Rc::clone(&states);
        let first = menu.clone();
        let mut framebuffer = Framebuffer::new(width, height, DENSITY, SystemTheme::Dark, &[], move || {
            let menu = remember(|| mutableStateOf(first.clone())).with(|state| *state);
            let handle = remember(|| mutableStateOf(Look::Idle)).with(|state| *state);
            held.set(Some((menu, handle)));
            Face(menu, handle, sink.clone());
        })?;
        framebuffer.set_active(true);
        // The composition composes once as the framebuffer starts, which made its states
        let (menu, handle) = states.get().ok_or(SurfaceError)?;
        Ok(Self {
            framebuffer,
            menu,
            handle,
            actions,
            pixels: vec![0; (width * height * 4) as usize],
            areas: Vec::new(),
            start: Instant::now(),
        })
    }

    /// Shows `menu`; the face grows or shrinks with its rows.
    pub fn set_menu(&mut self, menu: Menu) -> Result<(), SurfaceError> {
        let (width, height) = pixel_size(console::height(&menu));
        if (width, height) != (self.framebuffer.width(), self.framebuffer.height()) {
            self.framebuffer.resize(width, height, DENSITY)?;
            self.pixels.resize((width * height * 4) as usize, 0);
        }
        self.menu.set(menu);
        Ok(())
    }

    pub fn set_handle(&mut self, look: Look) {
        self.handle.set(look);
    }

    pub fn pointer(&mut self, pointer: Pointer) {
        let (phase, x, y) = match pointer {
            Pointer::Move(x, y) => (TouchPhase::Move, x, y),
            Pointer::Press(x, y) => (TouchPhase::Down, x, y),
            Pointer::Release(x, y) => (TouchPhase::Up, x, y),
            // Far off the face, so no button stays under the pointer
            Pointer::Leave => (TouchPhase::Move, -1000.0, -1000.0),
        };
        self.framebuffer.touch(phase, x, y);
    }

    /// The actions of the buttons pressed since the last call.
    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut *self.actions.0.borrow_mut())
    }

    /// When the composition needs the next `picture` call.
    pub fn next_tick(&mut self) -> NextTick {
        self.framebuffer.next_tick()
    }

    /// The picture as straight alpha RGBA rows from the top, when it changed since the last call.
    pub fn picture(&mut self) -> Option<(u32, u32, &[u8])> {
        let nanos = self.start.elapsed().as_nanos() as u64;
        self.framebuffer.tick(nanos.max(1));
        self.framebuffer.changed_areas(&mut self.areas);
        if self.areas.is_empty() {
            return None;
        }
        let width = self.framebuffer.width();
        let row = width as usize * 4;
        for area in &self.areas {
            // Whole rows: a row of the area is a part of a row of the picture
            let rows = PixelRect { x: 0, y: area.y, width, height: area.height };
            let bytes = area.y as usize * row..(area.y + area.height) as usize * row;
            self.framebuffer.draw_area(rows, &mut self.pixels[bytes]);
        }
        let height = self.framebuffer.height();
        round_corners(&mut self.pixels, width, height);
        Some((width, height, &self.pixels))
    }
}

/// What the console's thread takes.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Menu(Menu),
    Handle(Look),
    Pointer(Pointer),
}

/// What the console's thread gives.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// A new picture: RGBA rows from the bottom, as OpenGL reads them
    Picture { width: u32, height: u32, rows: Vec<u8> },
    /// A pressed button's action
    Action(Action),
}

/// Writes a line to the log. ALVR's logger drops info lines while a stream runs: on the Quest the
/// line goes to logcat directly (tag cranvr).
fn note(text: &str) {
    #[cfg(target_os = "android")]
    crate::flat::log::line(text);
    #[cfg(not(target_os = "android"))]
    alvr_common::info!("{text}");
}

/// The wait for the next frame while the composition changes from frame to frame
const FRAME: Duration = Duration::from_millis(11);

/// The console's composition on a thread of its own, which draws only when the picture changes.
/// The thread ends when this ends.
pub struct ConsoleThread {
    input: Sender<Input>,
    output: Receiver<Output>,
}

impl ConsoleThread {
    pub fn start(menu: Menu) -> std::io::Result<Self> {
        let (input, inputs) = mpsc::channel();
        let (outputs, output) = mpsc::channel();
        std::thread::Builder::new().name("console".into()).spawn(move || run(menu, &inputs, &outputs))?;
        Ok(Self { input, output })
    }

    pub fn send(&self, input: Input) {
        self.input.send(input).ok();
    }

    /// The pictures and actions so far.
    pub fn outputs(&self) -> mpsc::TryIter<'_, Output> {
        self.output.try_iter()
    }

    /// Waits up to `timeout` for the next picture or action.
    #[cfg(test)]
    fn wait(&self, timeout: Duration) -> Option<Output> {
        self.output.recv_timeout(timeout).ok()
    }
}

/// The console thread's loop: it draws a changed picture, sleeps until input comes or the
/// composition needs a frame, and applies all the input that came.
fn run(menu: Menu, inputs: &Receiver<Input>, outputs: &Sender<Output>) {
    let mut view = match ConsoleView::new(menu) {
        Ok(view) => view,
        Err(e) => return error!("cranvr console: {e}"),
    };
    let mut pictures = 0u32;
    loop {
        let start = Instant::now();
        if let Some((width, height, pixels)) = view.picture() {
            let rows = rows_from_bottom(pixels, width);
            let took = start.elapsed();
            if pictures == 0 || took > Duration::from_millis(8) {
                note(&format!("cranvr console: picture {pictures} of {width}x{height} took {took:?}"));
            }
            pictures += 1;
            if outputs.send(Output::Picture { width, height, rows }).is_err() {
                return;
            }
        }
        let first = match view.next_tick() {
            NextTick::NextFrame => inputs.recv_timeout(FRAME),
            NextTick::At(at) => inputs.recv_timeout(at.saturating_duration_since(Instant::now())),
            NextTick::OnWake => inputs.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let first = match first {
            Ok(input) => Some(input),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        for input in first.into_iter().chain(inputs.try_iter()) {
            match input {
                Input::Menu(menu) => {
                    if let Err(e) = view.set_menu(menu) {
                        return error!("cranvr console: {e}");
                    }
                }
                Input::Handle(look) => view.set_handle(look),
                Input::Pointer(pointer) => view.pointer(pointer),
            }
        }
        for action in view.take_actions() {
            if outputs.send(Output::Action(action)).is_err() {
                return;
            }
        }
    }
}

/// The picture's rows in the other order.
fn rows_from_bottom(pixels: &[u8], width: u32) -> Vec<u8> {
    let mut rows = Vec::with_capacity(pixels.len());
    for row in pixels.chunks_exact(width as usize * 4).rev() {
        rows.extend_from_slice(row);
    }
    rows
}

/// The console's quad layer: the thread that draws its face, and the newest picture.
#[cfg(target_os = "android")]
pub struct ConsoleLayer {
    thread: ConsoleThread,
    picture: Option<Quad>,
    /// What the thread last heard
    menu: Menu,
    look: Look,
}

#[cfg(target_os = "android")]
impl ConsoleLayer {
    pub fn start(menu: &Menu) -> std::io::Result<Self> {
        Ok(Self { thread: ConsoleThread::start(menu.clone())?, picture: None, menu: menu.clone(), look: Look::Idle })
    }

    /// Tells the face what changed and what the ray does on it, and shows its newest picture.
    /// The actions of the buttons pressed meanwhile.
    pub fn update(
        &mut self,
        menu: &Menu,
        look: Look,
        pointers: &[Pointer],
        session: &xr::Session<xr::OpenGlEs>,
        gfx: &GraphicsContext,
    ) -> Vec<Action> {
        if *menu != self.menu {
            self.menu = menu.clone();
            self.thread.send(Input::Menu(menu.clone()));
        }
        if look != self.look {
            self.look = look;
            self.thread.send(Input::Handle(look));
        }
        for pointer in pointers {
            self.thread.send(Input::Pointer(*pointer));
        }
        let mut actions = Vec::new();
        let mut newest = None;
        for output in self.thread.outputs() {
            match output {
                Output::Picture { width, height, rows } => newest = Some((width, height, rows)),
                Output::Action(action) => actions.push(action),
            }
        }
        if let Some((width, height, rows)) = newest {
            let meters = (width as f32 / DENSITY / 1000.0, height as f32 / DENSITY / 1000.0);
            match Quad::show(self.picture.take(), session, gfx, (width, height), &rows, meters) {
                Ok(quad) => self.picture = Some(quad),
                Err(e) => error!("cranvr console: {e}"),
            }
        }
        actions
    }

    /// The quad layer at the console's place, once a picture is drawn.
    pub fn layer<'a>(
        &'a self,
        space: &'a xr::Space,
        settings: Option<&'a xr::sys::CompositionLayerSettingsFB>,
        panel: &Panel,
    ) -> Option<xr::CompositionLayerQuad<'a, xr::OpenGlEs>> {
        Some(self.picture.as_ref()?.layer(space, settings, panel.center, panel.orientation))
    }
}

/// Clears the picture outside its round corners. Cranpose's CPU renderer fills the picture with an
/// opaque color before it draws, so the face's corners cannot be clear from the composition.
fn round_corners(pixels: &mut [u8], width: u32, height: u32) {
    let r = CORNER.min(width / 2).min(height / 2);
    for y in 0..r {
        for x in 0..r {
            // The distance from the corner circle's center, at the pixel's middle
            let (dx, dy) = (r as f32 - x as f32 - 0.5, r as f32 - y as f32 - 0.5);
            let coverage = (r as f32 - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            for (cx, cy) in [(x, y), (width - 1 - x, y), (x, height - 1 - y), (width - 1 - x, height - 1 - y)] {
                let alpha = &mut pixels[((cy * width + cx) * 4 + 3) as usize];
                *alpha = (*alpha as f32 * coverage).round() as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::Game;

    /// The point in millimeters from the face's top left corner at the middle of the button with
    /// `action`
    fn button(menu: &Menu, action: &Action) -> (f32, f32) {
        let (height, items) = console::layout(menu);
        let item = items.iter().find(|i| matches!(&i.kind, Kind::Button { action: a, .. } if a == action)).unwrap();
        console::face_point(height, item.center)
    }

    fn menu(games: usize, running: Option<usize>, room: Option<bool>) -> Menu {
        let games: Vec<Game> =
            (0..games).map(|i| Game { name: format!("Game {i}"), program: format!("game{i}.exe") }).collect();
        let mut menu = Menu { games: games.clone(), fps: 90, game_scale: 1.0, room, ..Default::default() };
        menu.status.game = running.map(|i| games[i].program.clone());
        menu
    }

    fn pixel(view: &mut ConsoleView, (x, y): (f32, f32)) -> [u8; 4] {
        let (width, _, bytes) = view.picture().map(|(w, h, b)| (w, h, b.to_vec())).unwrap_or_else(|| {
            (view.framebuffer.width(), view.framebuffer.height(), view.pixels.clone())
        });
        let i = (((y * DENSITY) as u32 * width + (x * DENSITY) as u32) * 4) as usize;
        bytes[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn the_face_is_opaque_inside_and_clear_in_its_corners() {
        let menu = menu(2, None, Some(true));
        let mut view = ConsoleView::new(menu.clone()).unwrap();
        let (width, height, _) = view.picture().unwrap();
        let (w, h) = pixel_size(console::height(&menu));
        assert_eq!((width, height), (w, h));
        let middle = pixel(&mut view, (300.0, mm(console::height(&menu)) / 2.0));
        assert_eq!(middle[3], 255, "{middle:?}");
        assert_eq!(pixel(&mut view, (0.5, 0.5))[3], 0);
    }

    #[test]
    fn the_face_ends_at_its_height() {
        for menu in [menu(0, None, None), menu(2, Some(1), Some(false)), menu(7, Some(0), Some(true))] {
            let (width, face) = pixel_size(console::height(&menu));
            let mut view = ConsoleView::new(menu.clone()).unwrap();
            // A taller picture shows where the face ends: its color in the left margin stops
            let rows = face + 160;
            view.framebuffer.resize(width, rows, DENSITY).unwrap();
            view.pixels.resize((width * rows * 4) as usize, 0);
            let bytes = view.picture().unwrap().2.to_vec();
            let at = |y: u32| &bytes[((y * width + 4) * 4) as usize..][..4];
            let color = at(face / 2).to_vec();
            let last = (0..rows).rev().find(|&y| at(y) == color.as_slice()).unwrap();
            assert!(last + 2 >= face && last <= face, "the face ends at row {last}, its height is {face} rows");
        }
    }

    #[test]
    fn the_ray_lights_a_button_and_a_press_reports_its_action() {
        let menu = menu(2, None, None);
        let mut view = ConsoleView::new(menu.clone()).unwrap();
        view.picture();
        // 72 Hz on the refresh row; the menu's refresh is 90 Hz
        let point = button(&menu, &Action::Fps(72));
        let before = pixel(&mut view, point);
        view.pointer(Pointer::Move(point.0, point.1));
        let lit = pixel(&mut view, point);
        assert_ne!(before, lit, "the button under the ray lights up");
        view.pointer(Pointer::Leave);
        assert_eq!(pixel(&mut view, point), before, "the button goes dark when the ray leaves");

        let exit = button(&menu, &Action::Exit);
        view.pointer(Pointer::Move(point.0, point.1));
        view.pointer(Pointer::Press(point.0, point.1));
        view.pointer(Pointer::Release(point.0, point.1));
        view.pointer(Pointer::Move(exit.0, exit.1));
        view.pointer(Pointer::Press(exit.0, exit.1));
        assert_eq!(view.take_actions(), [Action::Fps(72), Action::Exit]);
        view.pointer(Pointer::Release(exit.0, exit.1));
        assert!(view.take_actions().is_empty(), "a release reports nothing");
    }

    #[test]
    fn the_thread_draws_on_change_and_reports_presses() {
        let menu = menu(2, None, None);
        let thread = ConsoleThread::start(menu.clone()).unwrap();
        let second = Duration::from_secs(5);
        let Some(Output::Picture { width, height, rows }) = thread.wait(second) else { panic!("no first picture") };
        assert_eq!((width, height), pixel_size(console::height(&menu)));
        // rows from the bottom: the bottom row's corner is clear, its middle is the face
        assert_eq!(rows[3], 0);
        assert_eq!(rows[(width / 2 * 4 + 3) as usize], 255);
        // a still face draws nothing more
        assert_eq!(thread.wait(Duration::from_millis(100)), None);
        let (x, y) = button(&menu, &Action::Fps(72));
        thread.send(Input::Pointer(Pointer::Move(x, y)));
        assert!(matches!(thread.wait(second), Some(Output::Picture { .. })), "the lit button draws");
        thread.send(Input::Pointer(Pointer::Press(x, y)));
        thread.send(Input::Pointer(Pointer::Release(x, y)));
        let outputs: Vec<Output> = std::iter::from_fn(|| thread.wait(Duration::from_millis(300))).collect();
        assert!(outputs.contains(&Output::Action(Action::Fps(72))), "{:?}", outputs.len());
    }

    #[test]
    fn a_new_menu_shows_a_running_game_with_stop() {
        let mut view = ConsoleView::new(menu(2, None, None)).unwrap();
        view.picture();
        let running = menu(2, Some(0), None);
        view.set_menu(running.clone()).unwrap();
        view.picture();
        let stop = button(&running, &Action::Stop);
        view.pointer(Pointer::Move(stop.0, stop.1));
        view.pointer(Pointer::Press(stop.0, stop.1));
        assert_eq!(view.take_actions(), [Action::Stop]);
    }
}
