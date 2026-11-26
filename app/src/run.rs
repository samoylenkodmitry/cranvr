//! The app's work between the window and the host: one refresh of the state, a press of either app
//! with its effects, and a run with no window for a test beside the user's app.
//!
//! The window's refresh (`ui::refresh`) and the run with no window both call [`tick`]. The window's
//! buttons and the headset's requests both call [`act`], which applies the request through
//! `pages::apply` and does what the outcome asks: it saves the settings, restarts the host, starts a
//! game or stops one. Nothing here holds Cranpose state.

use crate::{
    backend::{self, Model, Settings},
    black_eye,
    pages::{self, Effect},
    pending::Pending,
    steam::{self, SteamState},
    system::{self, Permission},
    usb,
};
use cranvr_common::menu::Request;
use std::{
    thread,
    time::{Duration, Instant},
};

pub const PERMISSIONS: [Permission; 3] = [Permission::ScreenRecording, Permission::SystemAudio, Permission::Accessibility];

/// Applies a press of the window or of the headset, as `pages::apply` reads it, and does its effects.
pub fn act(settings: &mut Settings, model: &mut Model, request: &Request) {
    if model.games.is_empty() {
        model.games = steam::games();
    }
    let outcome = pages::apply(settings, model, request);
    if let Some(why) = outcome.refused {
        model.note_message(why);
        return;
    }
    if outcome.changed {
        settings.save();
        if outcome.restart && backend::host_running() {
            if let Err(e) = backend::start_host(settings) {
                model.message = Some(e);
            }
        }
    }
    if let Some(request) = outcome.pending {
        model.pending = Some(Pending::new(request, model, Instant::now()));
    }
    for effect in outcome.effects {
        match effect {
            Effect::Play(game) => {
                // Games see the headset only while the host runs
                if !backend::host_running() {
                    match backend::start_host(settings) {
                        Ok(()) => model.running = true,
                        Err(e) => model.message = Some(e),
                    }
                }
                // Steam may need seconds to start: keep the window responsive
                thread::spawn(move || {
                    if let Err(e) = steam::play(&game) {
                        backend::notify(e);
                    }
                });
            }
            Effect::StopAll => {
                thread::spawn(steam::stop_all);
            }
            Effect::ForgetSizes => {
                backend::forget_game_sizes();
                model.learned_sizes = backend::learned_game_sizes();
            }
            Effect::Setup => crate::setup::start(),
            Effect::TellConnection(connection) => backend::tell_host_connection(connection),
            Effect::DownloadUsb => usb::download_tools(),
            Effect::RestartHost => {
                model.network.clear();
                model.audio = None;
                match backend::start_host(settings) {
                    Ok(()) => model.running = true,
                    Err(e) => model.message = Some(e),
                }
            }
            Effect::AskPermission(permission) => permission.request(),
            Effect::ToHeadset(request) => backend::send_headset_request(&request),
            Effect::AddProgram(path) => {
                let result = steam::add_game(settings, &path, &steam::steam_games());
                match result {
                    Ok(()) => {
                        settings.save();
                        model.games = steam::games();
                        model.found_programs = steam::found_programs(&model.games, crate::pages::FOUND_PROGRAMS);
                    }
                    Err(e) => model.note_message(e),
                }
            }
            Effect::RunProgram(path) => backend::choose(backend::Chosen::Run(path)),
            Effect::WatchLog => model.log_until = Some(Instant::now() + crate::pages::LOG_TIME),
            Effect::PermissionSettings(permission) => permission.open_settings(),
            Effect::OpenSteam => {
                // Steam may need seconds to start: keep the window responsive
                thread::spawn(|| {
                    if let Err(e) = steam::start() {
                        backend::notify(e);
                    }
                });
            }
        }
    }
}

/// One refresh: the host's output, the headset's requests, the game runs' verdicts and the programs
/// that the user chose. A slow refresh, every 2 s, also reads the state of the Mac: Steam, the
/// permissions, the devices. Each refresh writes the pages for the headset (`vr-menu.json`) when
/// they changed.
pub fn tick(settings: &mut Settings, model: &mut Model, slow: bool) {
    backend::pump(model);
    if let Some(line) = backend::watch_host(settings, model) {
        eprintln!("{line}");
        model.log(line);
        model.running = backend::host_running();
    }
    model.settings_problem = backend::settings_problem();
    usb::set_choice(settings.connection);
    model.usb = usb::status();
    for request in std::mem::take(&mut model.vr_requests) {
        match Request::from_json(&request) {
            Some(request) => act(settings, model, &request),
            None => model.note_message(format!("The headset asked for {request}, which this app does not know")),
        }
    }
    let reports = std::mem::take(&mut model.black_eyes);
    black_eye_reports(settings, model, reports);
    vr_system_verdicts(settings, model, crate::game_run::take_verdicts());
    for chosen in backend::take_chosen() {
        chosen_program(settings, model, chosen);
    }
    // A finished setup changed what the Play tab shows: read the state now, not in 2 s
    let setup_ended = model.setup_was_running && !model.setup.as_ref().is_some_and(crate::setup::Setup::running);
    model.setup_was_running = model.setup.as_ref().is_some_and(crate::setup::Setup::running);
    if slow || setup_ended {
        model.lan = system::lan_addresses();
        model.permissions = PERMISSIONS.map(Permission::granted);
        model.input_devices = crate::microphone::input_devices();
        model.steam = steam::state();
        model.rosetta = steam::rosetta_ready();
        model.game_support = steam::game_support_installed();
        model.game_support_outdated = steam::game_support_outdated();
        model.steam_running = model.steam == SteamState::Ready && steam::running();
        model.steam_signed_in = steam::signed_in();
        model.games = steam::games();
        model.learned_sizes = backend::learned_game_sizes();
        model.found_programs = steam::found_programs(&model.games, crate::pages::FOUND_PROGRAMS);
    }
    if model.log_until.is_some_and(|until| until <= Instant::now()) {
        model.log_until = None;
    }
    if model.notice.as_ref().is_some_and(|(_, until)| *until <= Instant::now()) {
        model.notice = None;
    }
    backend::write_vr_menu(settings, model);
}

/// Answers the host's reports of a black picture (see black_eye.rs): the program's option "Each
/// eye in its own pass" turns on once, and the game starts again with it. A report with the option
/// on already only shows a notice.
fn black_eye_reports(settings: &mut Settings, model: &mut Model, reports: Vec<serde_json::Value>) {
    for report in reports {
        let black_eye::Response { notice, restart } = black_eye::respond(&mut settings.launch, &model.games, report["program"].as_str(), report["eye"].as_str().unwrap_or("right"));
        model.notice = Some((notice, Instant::now() + black_eye::NOTICE_TIME));
        if let Some(program) = restart {
            // The new start reads the saved options
            settings.save();
            thread::spawn(move || {
                if let Err(e) = steam::restart(&program) {
                    backend::notify(e);
                }
            });
        }
    }
}

/// Answers the verdicts of game runs that drew nothing through OpenComposite (see vr_system.rs): a game that
/// follows the setting gets SteamVR and starts again with it.
fn vr_system_verdicts(settings: &mut Settings, model: &mut Model, verdicts: Vec<crate::game_run::Verdict>) {
    if verdicts.is_empty() {
        return;
    }
    let steamvr = steam::steamvr().is_some();
    for verdict in verdicts {
        let games = if model.games.is_empty() { steam::games() } else { model.games.clone() };
        let crate::vr_system::Response { notice, restart } = crate::vr_system::respond(&mut settings.launch, &games, &verdict.program, verdict.why, steamvr);
        model.log(notice.window.clone());
        model.notice = Some((notice, Instant::now() + black_eye::NOTICE_TIME));
        if let Some(program) = restart {
            settings.save();
            thread::spawn(move || {
                if let Err(e) = steam::restart(&program) {
                    backend::notify(e);
                }
            });
        }
    }
}

/// Uses a program that the user chose: adds it to the list of games, or starts it once.
fn chosen_program(settings: &mut Settings, model: &mut Model, chosen: backend::Chosen) {
    match chosen {
        backend::Chosen::Add(path) => {
            let result = steam::add_game(settings, &path, &steam::steam_games());
            if result.is_ok() {
                settings.save();
                // The list shows the game now, with its options open: they hold what cranvr filled in
                model.games = steam::games();
                model.open_options = Some(steam::mac_to_windows(&path));
            }
            model.message = result.err();
        }
        backend::Chosen::Run(path) => {
            // Games see the headset only while the host runs
            if !backend::host_running() {
                match backend::start_host(settings) {
                    Ok(()) => model.running = true,
                    Err(e) => model.message = Some(e),
                }
            }
            let result = steam::run_program(&path);
            settings.game = path.display().to_string();
            settings.save();
            model.message = result.err();
        }
    }
}

/// Runs the app with no window, for a test beside the user's app: `CRANVR_HOME` names its folder,
/// `CRANVR_NATIVE_PORT` its port. It starts the host, answers the headset's requests as the window
/// does and saves the settings in its own `app.json`. Each line of its input is a request as the
/// window's press makes it (`{"action": "run", "id": "headset.recenter"}`), so a test presses the
/// window's buttons too. It watches no USB cable, asks macOS for no permission and moves no window,
/// so the user's app keeps all of that. It ends with SIGTERM, or when the host ended and the app's
/// watch does not start it again (see `backend::watch_host`).
pub fn headless() {
    let mut settings = Settings::load();
    let mut model = Model::default();
    let (presses, pressed) = std::sync::mpsc::channel();
    thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            match serde_json::from_str(&line).ok().as_ref().and_then(Request::from_json) {
                Some(request) => drop(presses.send(request)),
                None => eprintln!("cranvr: no request: {line}"),
            }
        }
    });
    if let Err(e) = backend::start_host(&settings) {
        eprintln!("cranvr: the host does not start: {e}");
        std::process::exit(1);
    }
    eprintln!("cranvr: running with no window in {} on port {}", backend::config_dir().display(), backend::native_port());
    let mut last_slow = Instant::now() - Duration::from_secs(2);
    loop {
        thread::sleep(Duration::from_millis(150));
        let slow = last_slow.elapsed() >= Duration::from_secs(2);
        if slow {
            last_slow = Instant::now();
        }
        tick(&mut settings, &mut model, slow);
        for request in pressed.try_iter() {
            eprintln!("cranvr: the window's press {}", request.to_json());
            act(&mut settings, &mut model, &request);
        }
        if !backend::host_wanted() {
            eprintln!("cranvr: the host ended");
            return;
        }
    }
}
