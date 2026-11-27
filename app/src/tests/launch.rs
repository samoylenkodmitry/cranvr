use super::*;

#[test]
fn a_reason_without_its_option_leaves_the_options_at_their_defaults() {
    let reason = GameOptions { eye_per_pass_reason: "because".into(), ..Default::default() };
    assert!(reason.is_default());
    assert!(!GameOptions { eye_per_pass: true, ..reason.clone() }.is_default());
    assert!(!GameOptions { args: "-x".into(), ..reason }.is_default());
}

#[test]
fn a_program_follows_the_choice_for_every_program_until_it_has_its_own() {
    let default = WineOptions::default();
    let steamvr = WineOptions { openvr: OpenVr::SteamVr, ..Default::default() };
    let own = |choice| GameOptions { openvr: choice, ..Default::default() };
    assert_eq!(default.openvr, OpenVr::OpenComposite, "OpenComposite is the default");
    assert_eq!(openvr(&default, &own(None)), OpenVr::OpenComposite);
    assert_eq!(openvr(&steamvr, &own(None)), OpenVr::SteamVr);
    assert_eq!(openvr(&steamvr, &own(Some(OpenVr::Off))), OpenVr::Off);
    assert_eq!(openvr(&default, &own(Some(OpenVr::SteamVr))), OpenVr::SteamVr);
    assert!(own(None).is_default());
    assert!(!own(Some(OpenVr::SteamVr)).is_default());
    // The choice reaches no environment variable: the OpenVR path file carries it
    assert_eq!(environment(&steamvr, &own(Some(OpenVr::Off))), environment(&default, &own(Some(OpenVr::SteamVr))));
}

#[test]
fn the_choice_is_saved_by_name_and_a_missing_one_is_the_default() {
    let game = GameOptions { openvr: Some(OpenVr::SteamVr), ..Default::default() };
    let text = serde_json::to_string(&game).unwrap();
    assert!(text.contains(r#""openvr":"steamvr""#), "{text}");
    assert_eq!(serde_json::from_str::<GameOptions>(&text).unwrap(), game);
    let wine: WineOptions = serde_json::from_str(r#"{"mac_window": true}"#).unwrap();
    assert!(wine.mac_window && wine.openvr == OpenVr::OpenComposite);
    let game: GameOptions = serde_json::from_str(r#"{"args": "-x"}"#).unwrap();
    assert_eq!(game.openvr, None);
    // The words of app.json are the console's words
    for choice in OpenVr::ALL {
        assert_eq!(serde_json::to_string(&choice).unwrap(), format!("\"{}\"", choice.key()));
        assert_eq!(OpenVr::from_key(choice.key()), Some(choice));
    }
    assert_eq!(OpenVr::from_key("SteamVR"), None);
    // A reason without a choice of the game's own is a leftover
    assert!(GameOptions { openvr_reason: "because".into(), ..Default::default() }.is_default());
}

#[test]
fn default_options_only_keep_the_mac_window_empty() {
    assert_eq!(environment(&WineOptions::default(), &GameOptions::default()), [("DXMT_SKIP_PRESENT".to_owned(), "1".to_owned())]);
    let wine = WineOptions { mac_window: true, ..Default::default() };
    assert!(environment(&wine, &GameOptions::default()).is_empty());
    assert_eq!(arguments(&wine, &GameOptions::default(), Some(UNITY)).count(), 0);
}

#[test]
fn both_eyes_in_one_pass_reaches_the_environment_and_a_reason_without_it_is_a_leftover() {
    let game = GameOptions { eye_one_pass: true, eye_one_pass_reason: "because".into(), ..Default::default() };
    let vars = environment(&WineOptions::default(), &game);
    assert!(vars.contains(&("DXMT_RT_ARRAY_INDEX_FROM_ANY_SHADER".to_owned(), "1".to_owned())));
    assert!(!vars.iter().any(|(name, _)| name == "BASALT_MAX_ARRAY_SIZE"));
    assert!(!game.is_default());
    let reason = GameOptions { eye_one_pass_reason: "because".into(), ..Default::default() };
    assert!(reason.is_default());
}

#[test]
fn a_game_s_options_reach_its_environment_and_arguments() {
    let game = GameOptions {
        eye_per_pass: true,
        dll_overrides: "winhttp=n,b".into(),
        env: "FOO=1  BAR=two =skipped noequals".into(),
        args: "fpfc  --verbose".into(),
        ..Default::default()
    };
    let vars = environment(&WineOptions { mac_window: true, ..Default::default() }, &game);
    assert_eq!(
        vars,
        [
            ("BASALT_MAX_ARRAY_SIZE".to_owned(), "1".to_owned()),
            ("WINEDLLOVERRIDES".to_owned(), "winhttp=n,b".to_owned()),
            ("FOO".to_owned(), "1".to_owned()),
            ("BAR".to_owned(), "two".to_owned()),
        ]
    );
    assert_eq!(arguments(&WineOptions::default(), &game, None).collect::<Vec<_>>(), ["fpfc", "--verbose"]);
}

#[test]
fn overrides_for_every_program_and_for_one_compose_and_its_variables_come_last() {
    let wine = WineOptions { mac_window: true, dll_overrides: "dxgi=n".into(), env: "LEVEL=1".into(), ..Default::default() };
    let game = GameOptions { dll_overrides: " winhttp=n,b ".into(), env: "LEVEL=2".into(), ..Default::default() };
    let vars = environment(&wine, &game);
    assert_eq!(vars[0], ("WINEDLLOVERRIDES".to_owned(), "dxgi=n;winhttp=n,b".to_owned()));
    assert_eq!(vars.iter().rev().find(|(name, _)| name == "LEVEL").map(|(_, value)| value.as_str()), Some("2"));
    assert_eq!(summary(&wine, &game, None), "WINEDLLOVERRIDES=dxgi=n;winhttp=n,b LEVEL=1 LEVEL=2");
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("cranvr-launch-{name}-{}", std::process::id()));
    fs::remove_dir_all(&dir).ok();
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn a_mod_loader_s_dll_in_the_folder_gets_no_override() {
    let dir = scratch("loader");
    fs::write(dir.join("Game.exe"), b"x").unwrap();
    fs::write(dir.join("winhttp.dll"), b"x").unwrap();
    assert_eq!(detect(&dir.join("Game.exe"), None), GameOptions::default());
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_steam_app_id_comes_from_the_folder_first_then_from_the_copied_game() {
    let dir = scratch("appid");
    let program = dir.join("Game.exe");
    fs::write(&program, b"x").unwrap();
    let from_copy = detect(&program, Some(("Some Game", "123")));
    assert_eq!(from_copy.steam_app_id, "123");
    assert!(from_copy.steam_app_id_reason.contains("Some Game"));
    fs::write(dir.join("steam_appid.txt"), "480\r\n").unwrap();
    let from_file = detect(&program, Some(("Some Game", "123")));
    assert_eq!(from_file.steam_app_id, "480");
    assert!(from_file.steam_app_id_reason.contains("steam_appid.txt"));
    // A file that holds no number is no ID
    fs::write(dir.join("steam_appid.txt"), "not a number").unwrap();
    assert_eq!(steam_appid_file(&dir), None);
    fs::write(dir.join("steam_appid.txt"), "").unwrap();
    assert_eq!(steam_appid_file(&dir), None);
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_graphics_choice_adds_the_game_s_own_arguments_else_its_engine_s() {
    let unity = Engine { small_window: None, ..UNITY };
    let wine = WineOptions::default();
    let vulkan = GameOptions { graphics: Some(Graphics::Vulkan), args: "fpfc".into(), ..Default::default() };
    assert_eq!(arguments(&wine, &vulkan, Some(unity)).collect::<Vec<_>>(), ["fpfc", "-force-vulkan"]);
    // A game of no known engine gets nothing until the user types its argument
    assert_eq!(arguments(&wine, &vulkan, None).collect::<Vec<_>>(), ["fpfc"]);
    let typed = GameOptions { vulkan_args: " +gfx_strAPI VLK ".into(), ..vulkan.clone() };
    assert_eq!(arguments(&wine, &typed, Some(unity)).collect::<Vec<_>>(), ["fpfc", "+gfx_strAPI", "VLK"]);
    let direct3d = GameOptions { graphics: Some(Graphics::Direct3D), ..typed.clone() };
    assert_eq!(graphics_arguments(&direct3d, Some(unity)), "-force-d3d11");
    // The game's own choice adds no argument, and typed arguments wait for their choice
    let own = GameOptions { graphics: None, ..typed };
    assert_eq!(arguments(&wine, &own, Some(unity)).collect::<Vec<_>>(), ["fpfc"]);
    assert!(!own.is_default(), "typed arguments are an option of their own");
    assert!(!GameOptions { graphics: Some(Graphics::Vulkan), ..Default::default() }.is_default());
    assert_eq!(summary(&WineOptions { mac_window: true, ..Default::default() }, &vulkan, Some(unity)), "fpfc -force-vulkan");
    // The words of app.json are the console's words, and a missing choice is the game's own
    let text = serde_json::to_string(&vulkan).unwrap();
    assert!(text.contains(r#""graphics":"vulkan""#), "{text}");
    assert_eq!(serde_json::from_str::<GameOptions>(r#"{"args": "-x"}"#).unwrap().graphics, None);
    for choice in Graphics::ALL {
        assert_eq!(serde_json::to_string(&choice).unwrap(), format!("\"{}\"", choice.key()));
        assert_eq!(Graphics::from_key(choice.key()), Some(choice));
    }
}

#[test]
fn the_engine_comes_from_its_files_next_to_the_program() {
    let dir = scratch("engine");
    let program = dir.join("Game.exe");
    fs::write(&program, b"x").unwrap();
    assert_eq!(engine(&program), None);
    fs::create_dir_all(dir.join("Game_Data")).unwrap();
    assert_eq!(engine(&program).map(|e| e.vulkan), Some("-force-vulkan"));
    fs::remove_dir_all(dir.join("Game_Data")).unwrap();
    fs::write(dir.join("UnityPlayer.dll"), b"x").unwrap();
    assert_eq!(engine(&program).map(|e| e.name), Some("Unity"));
    fs::remove_file(dir.join("UnityPlayer.dll")).unwrap();
    // Unreal: the launcher beside Engine/, and the game's own program three folders down
    fs::create_dir_all(dir.join("Engine/Binaries")).unwrap();
    assert_eq!(engine(&program).map(|e| e.name), Some("Unreal Engine"));
    let inner = dir.join("Project/Binaries/Win64");
    fs::create_dir_all(&inner).unwrap();
    fs::write(inner.join("Project-Win64-Shipping.exe"), b"x").unwrap();
    assert_eq!(engine(&inner.join("Project-Win64-Shipping.exe")).map(|e| e.direct3d), Some("-d3d11"));
    fs::remove_dir_all(&dir).ok();
    // Serious Engine: Bin/x64/<program> and Bin/<program> with the Vulkan shaders in Content/
    let dir = scratch("serious");
    let wide = dir.join("Bin/x64");
    fs::create_dir_all(&wide).unwrap();
    fs::create_dir_all(dir.join("Content")).unwrap();
    fs::write(wide.join("Game.exe"), b"x").unwrap();
    fs::write(dir.join("Bin/Game.exe"), b"x").unwrap();
    assert_eq!(engine(&wide.join("Game.exe")), None);
    fs::write(dir.join("Content/CachedShaders_VLK.gro"), b"x").unwrap();
    assert_eq!(engine(&wide.join("Game.exe")).map(|e| e.vulkan), Some("+gfx_strAPI VLK"));
    assert_eq!(engine(&dir.join("Bin/Game.exe")).map(|e| e.name), Some("Serious Engine"));
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_steam_app_id_reaches_the_environment_and_a_reason_without_it_is_a_leftover() {
    let game = GameOptions { steam_app_id: " 620 ".into(), steam_app_id_reason: "because".into(), ..Default::default() };
    let vars = environment(&WineOptions::default(), &game);
    assert!(vars.contains(&("SteamAppId".to_owned(), "620".to_owned())));
    assert!(vars.contains(&("SteamGameId".to_owned(), "620".to_owned())));
    assert!(!game.is_default());
    assert!(GameOptions { steam_app_id_reason: "because".into(), ..Default::default() }.is_default());
    assert!(!GameOptions { dll_overrides: "winhttp=n,b".into(), ..Default::default() }.is_default());
    let old: GameOptions = serde_json::from_str(r#"{"dll_overrides": "winhttp=n,b"}"#).unwrap();
    assert!(old.steam_app_id.is_empty() && old.steam_app_id_reason.is_empty());
}

#[test]
fn a_game_of_a_known_engine_gets_a_small_window_while_the_mac_window_shows_no_picture() {
    let small = UNITY.small_window.unwrap();
    let empty = WineOptions::default();
    let mirror = WineOptions { mac_window: true, ..Default::default() };
    let follows = GameOptions::default();
    assert_eq!(window(&empty, &follows), Window::Small);
    assert_eq!(window_arguments(&empty, &follows, Some(UNITY)), small);
    assert_eq!(arguments(&empty, &follows, Some(UNITY)).collect::<Vec<_>>().join(" "), small);
    assert!(summary(&empty, &follows, Some(UNITY)).ends_with(small));
    // The default is no entry of app.json
    assert!(follows.is_default());
    // A user who wants the game's picture on the Mac keeps the game's own window
    assert_eq!(window(&mirror, &follows), Window::Own);
    assert_eq!(arguments(&mirror, &follows, Some(UNITY)).count(), 0);
    // The game's own choice wins over both
    let own = GameOptions { window: Some(Window::Own), ..Default::default() };
    assert_eq!(arguments(&empty, &own, Some(UNITY)).count(), 0);
    assert!(!own.is_default());
    let chosen = GameOptions { window: Some(Window::Small), args: "fpfc".into(), ..Default::default() };
    assert_eq!(arguments(&mirror, &chosen, Some(UNITY)).next(), Some("fpfc"));
    assert_eq!(window_arguments(&mirror, &chosen, Some(UNITY)), small);
    // An engine without measured arguments, or no engine, adds nothing
    for engine in [None, Some(UNREAL), Some(SERIOUS)] {
        assert_eq!(arguments(&empty, &follows, engine).count(), 0);
    }
    // The words of app.json are the console's words
    let text = serde_json::to_string(&own).unwrap();
    assert!(text.contains(r#""window":"own""#), "{text}");
    assert_eq!(serde_json::from_str::<GameOptions>(r#"{"args": "-x"}"#).unwrap().window, None);
    for choice in Window::ALL {
        assert_eq!(serde_json::to_string(&choice).unwrap(), format!("\"{}\"", choice.key()));
        assert_eq!(Window::from_key(choice.key()), Some(choice));
    }
}
