//! Which hands show Cranpose's models of the controllers.
//!
//! A game draws its own controllers. The headset's model then stands at the same place as a second
//! copy. The Controls setting "Controller models during a game" (`ModelsInGame`, kept by the Mac with
//! the button mapping) decides what shows while a game runs: both models, the model of each hand
//! whose ray points at a cranvr panel, or none. Outside a game the models show with the rays, as
//! the panels need them.
//!
//! The rule names no game. It reads the host's word that a game runs, the hover of each hand's
//! pointer (`hover.rs`) and the setting.

use cranvr_common::controls::ModelsInGame;

/// What decides the models in one frame of the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scene {
    /// The host says that a game runs
    pub game_runs: bool,
    /// The rays show (see `controls::beams_wanted`)
    pub beams: bool,
    /// Whether the left hand's and the right hand's rays point at a panel of the app
    pub pointing: [bool; 2],
    /// A controller holds a panel by its handle. A carry is no hover, so the hands keep their models.
    pub carrying: bool,
}

/// Which hands show their model, left then right, given the setting `choice`, what the frame shows
/// and what the hands showed before.
pub fn shown(choice: ModelsInGame, scene: Scene, before: [bool; 2]) -> [bool; 2] {
    if !scene.game_runs {
        return [scene.beams; 2];
    }
    match choice {
        ModelsInGame::Always => [true; 2],
        ModelsInGame::Never => [false; 2],
        ModelsInGame::WhilePointing if scene.carrying => before,
        ModelsInGame::WhilePointing => scene.pointing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_game(pointing: [bool; 2]) -> Scene {
        Scene { game_runs: true, beams: true, pointing, carrying: false }
    }

    #[test]
    fn outside_a_game_both_models_show_with_the_rays_whatever_the_setting() {
        for choice in ModelsInGame::ALL {
            let scene = Scene { game_runs: false, beams: true, pointing: [false; 2], carrying: false };
            assert_eq!(shown(choice, scene, [false; 2]), [true; 2]);
            assert_eq!(shown(choice, Scene { beams: false, ..scene }, [true; 2]), [false; 2]);
        }
    }

    #[test]
    fn in_a_game_a_hand_shows_its_model_while_its_ray_points_at_a_panel() {
        let choice = ModelsInGame::WhilePointing;
        assert_eq!(shown(choice, in_game([false, false]), [true, true]), [false, false]);
        assert_eq!(shown(choice, in_game([false, true]), [false, false]), [false, true]);
        assert_eq!(shown(choice, in_game([true, true]), [false, true]), [true, true]);
        // The rays hidden, no panel shows and no hand points
        assert_eq!(shown(choice, Scene { beams: false, ..in_game([false; 2]) }, [true; 2]), [false; 2]);
    }

    #[test]
    fn a_hand_that_carries_a_panel_keeps_what_it_showed() {
        let carrying = Scene { carrying: true, ..in_game([false; 2]) };
        assert_eq!(shown(ModelsInGame::WhilePointing, carrying, [false, true]), [false, true]);
        assert_eq!(shown(ModelsInGame::Never, carrying, [false, true]), [false, false]);
    }

    #[test]
    fn in_a_game_always_shows_both_and_never_shows_none() {
        assert_eq!(shown(ModelsInGame::Always, in_game([false; 2]), [false; 2]), [true; 2]);
        assert_eq!(shown(ModelsInGame::Always, Scene { beams: false, ..in_game([false; 2]) }, [false; 2]), [true; 2]);
        assert_eq!(shown(ModelsInGame::Never, in_game([true; 2]), [true; 2]), [false; 2]);
    }
}
