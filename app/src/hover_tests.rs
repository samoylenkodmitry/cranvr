//! The window's buttons under the mouse: a headless shell moves the mouse over a button, presses it
//! and moves away, and the test reads the fills that the button draws in each state.

use std::{cell::RefCell, rc::Rc};

use cranpose::prelude::*;
use cranpose::{HeadlessRenderer, RecordedRenderScene};
use cranpose_core::{MemoryApplier, NodeId};
use cranpose_render_common::{Renderer, SceneUpdates};
use cranpose_testing::robot::{RobotTestRule, TestRenderer};
use cranpose_ui_graphics::{Brush, DrawPrimitive};

use super::{Styled, INDICATION, PRIMARY};

// The headless shell's renderer, which also keeps what the window draws after each change.
struct Recording {
    shell: TestRenderer,
    drawn: Rc<RefCell<RecordedRenderScene>>,
}

impl Renderer for Recording {
    type Scene = <TestRenderer as Renderer>::Scene;
    type Error = ();

    fn scene(&self) -> &Self::Scene {
        self.shell.scene()
    }

    fn scene_mut(&mut self) -> &mut Self::Scene {
        self.shell.scene_mut()
    }

    fn rebuild_scene_from_applier(&mut self, applier: &mut MemoryApplier, root: NodeId, viewport: Size) -> Result<(), ()> {
        self.shell.rebuild_scene_from_applier(applier, root, viewport)?;
        *self.drawn.borrow_mut() = HeadlessRenderer::new().render_from_applier(applier, root);
        Ok(())
    }

    fn update_scene_from_applier(&mut self, applier: &mut MemoryApplier, root: NodeId, viewport: Size, updates: SceneUpdates<'_>) -> Result<(), ()> {
        self.shell.update_scene_from_applier(applier, root, viewport, updates)?;
        *self.drawn.borrow_mut() = HeadlessRenderer::new().render_from_applier(applier, root);
        Ok(())
    }
}

// The solid fills that the window draws, in paint order, as 8-bit colors with alpha.
fn fills(drawn: &RecordedRenderScene) -> Vec<[u8; 4]> {
    drawn
        .operations()
        .iter()
        .filter_map(|op| match op {
            cranpose::RenderOp::Primitive { primitive: DrawPrimitive::Rect { brush: Brush::Solid(color), .. } | DrawPrimitive::RoundRect { brush: Brush::Solid(color), .. }, .. } => Some(*color),
            _ => None,
        })
        .map(|Color(r, g, b, a)| [r, g, b, a].map(|channel| (channel * 255.0).round() as u8))
        .collect()
}

// What a fill of `over` on `face` shows, both 8-bit, the face opaque.
fn blend(face: [u8; 4], over: [u8; 4]) -> [u8; 3] {
    let a = f32::from(over[3]) / 255.0;
    [0, 1, 2].map(|i| (f32::from(face[i]) * (1.0 - a) + f32::from(over[i]) * a).round() as u8)
}

#[test]
fn a_button_shows_a_hover_under_the_mouse_and_a_press_while_held() {
    let drawn = Rc::new(RefCell::new(RecordedRenderScene::default()));
    let renderer = Recording { shell: TestRenderer::default(), drawn: Rc::clone(&drawn) };
    let clicks = Rc::new(RefCell::new(0));
    let counted = Rc::clone(&clicks);
    let mut robot = RobotTestRule::new(240, 80, renderer, move || {
        let counted = Rc::clone(&counted);
        CompositionLocalProvider([local_indication().provides(INDICATION)], move || {
            let counted = Rc::clone(&counted);
            Styled("Play".into(), PRIMARY, move || *counted.borrow_mut() += 1);
        });
    });
    robot.wait_for_idle();
    let face = [PRIMARY.0, PRIMARY.1, PRIMARY.2, 255];
    let idle = fills(&drawn.borrow());
    robot.move_to(20.0, 18.0);
    robot.wait_for_idle();
    let hovered = fills(&drawn.borrow());
    robot.mouse_down();
    robot.wait_for_idle();
    let pressed = fills(&drawn.borrow());
    robot.mouse_up();
    robot.move_to(230.0, 75.0);
    robot.wait_for_idle();
    let left = fills(&drawn.borrow());
    println!("idle {idle:?}; hovered {hovered:?} shows {:?}; pressed {pressed:?} shows {:?}; left {left:?}", hovered.last().map(|over| blend(face, *over)), pressed.last().map(|over| blend(face, *over)));
    assert_eq!(idle, [face], "the idle button draws its face alone");
    assert_eq!(hovered, [face, [255, 255, 255, 31]], "the hover lays white at 12% over the face");
    assert_eq!(pressed, [face, [255, 255, 255, 77]], "the press lays white at 30% over the face");
    assert_eq!(left, [face], "the hover goes with the mouse");
    assert_eq!(*clicks.borrow(), 1);
}
