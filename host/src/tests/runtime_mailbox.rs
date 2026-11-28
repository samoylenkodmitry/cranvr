//! A game's mailbox messages through the runtime link: the screen that reaches the headset, its
//! life across a restart of the game's session and its end with the game.

use super::*;
use std::sync::mpsc::Receiver;

const BEGIN: &str = r#"{"type": "begin_loading", "mapName": "a1_intro_world", "chapter": "", "tipText": "A tip of the test.", "loadingText": "LOADING", "facingAngle": 0.000000}"#;
const MESSAGE: &str = r#"{"type": "show_message", "text": "Press Trigger To Start"}"#;

fn message(kind: u16, payload: &[u8]) -> Vec<u8> {
    let mut m = (2 + payload.len() as u32).to_le_bytes().to_vec();
    m.extend_from_slice(&kind.to_le_bytes());
    m.extend_from_slice(payload);
    m
}

fn mailbox(address: &str, text: &str) -> Vec<u8> {
    let mut p = (address.len() as u16).to_le_bytes().to_vec();
    p.extend_from_slice(address.as_bytes());
    p.extend_from_slice(&(text.len() as u16).to_le_bytes());
    p.extend_from_slice(text.as_bytes());
    message(msg::MAILBOX_MESSAGE, &p)
}

/// Connects as a game's runtime and reads the host's greeting.
fn connect(link: &RuntimeLink) -> TcpStream {
    let text = fs::read_to_string(link.control_file()).unwrap();
    let fields: Vec<&str> = text.split_whitespace().collect();
    let mut runtime = TcpStream::connect(("127.0.0.1", fields[0].parse::<u16>().unwrap())).unwrap();
    let mut hello = PROTOCOL_VERSION.to_le_bytes().to_vec();
    hello.extend_from_slice(fields[1].as_bytes());
    hello.extend_from_slice(&7u64.to_le_bytes());
    hello.extend_from_slice(&0u16.to_le_bytes());
    runtime.write_all(&message(msg::HELLO, &hello)).unwrap();
    assert_eq!(read_message(&mut runtime).unwrap().0, msg::HELLO_ACK);
    for _ in 0..2 {
        assert_eq!(read_message(&mut runtime).unwrap().0, msg::INTERACTION_PROFILE);
    }
    runtime
}

fn next(screens: &Receiver<Option<GameScreen>>) -> Option<GameScreen> {
    screens.recv_timeout(Duration::from_secs(2)).expect("the screen changes")
}

fn start(name: &str) -> (Arc<RuntimeLink>, mpsc::Receiver<GameEvent>, Receiver<Option<GameScreen>>) {
    let dir = std::env::temp_dir().join(format!("cranvr-mailbox-test-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let (sender, screens) = mpsc::channel();
    let (link, events) = RuntimeLink::start(&dir.join("control.txt"), (1600, 1700), 120, |_| (), move |screen| sender.send(screen).unwrap()).unwrap();
    (link, events, screens)
}

#[test]
fn the_messages_reach_the_headset_in_order_and_the_screen_survives_a_session_restart() {
    let (link, _events, screens) = start("restart");
    let mut runtime = connect(&link);
    runtime.write_all(&mailbox("game/interstitials", BEGIN)).unwrap();
    let shown = next(&screens).expect("the load shows");
    assert_eq!(shown.loading.as_deref(), Some("LOADING"));
    assert_eq!(shown.lines, ["A tip of the test."]);
    // OpenComposite ends its first session at the game's first frame and makes a new one at once
    runtime.write_all(&message(msg::TEARDOWN, &[])).unwrap();
    drop(runtime);
    std::thread::sleep(Duration::from_millis(30));
    let mut runtime = connect(&link);
    runtime.write_all(&mailbox("game/interstitials", MESSAGE)).unwrap();
    let shown = next(&screens).expect("the load shows with its message");
    assert_eq!(shown.message.as_deref(), Some("Press Trigger To Start"));
    assert!(screens.recv_timeout(SCREEN_AFTER_DISCONNECT * 2).is_err(), "the restart hid nothing");
    // A message that is not about a screen changes nothing
    runtime.write_all(&mailbox("game/interstitials", r#"{"type": "set_paths", "webroot": "C:\\webui"}"#)).unwrap();
    runtime.write_all(&mailbox("game/interstitials", r#"{"type": "end_loading"}"#)).unwrap();
    assert_eq!(next(&screens), None, "the end of the load hides the screen");
    assert!(link.is_connected(), "the link stays up");
}

#[test]
fn the_screen_hides_when_the_game_is_gone() {
    let (link, _events, screens) = start("gone");
    let mut runtime = connect(&link);
    runtime.write_all(&mailbox("game/interstitials", BEGIN)).unwrap();
    runtime.write_all(&mailbox("game/interstitials", MESSAGE)).unwrap();
    assert!(next(&screens).is_some());
    assert!(next(&screens).is_some_and(|shown| shown.message.is_some()));
    drop(runtime);
    let gone = std::time::Instant::now();
    assert_eq!(next(&screens), None, "nothing shows once the game is gone");
    assert!(gone.elapsed() >= SCREEN_AFTER_DISCONNECT, "the screen waited for a new session");
}

#[test]
fn a_message_too_long_for_the_link_ends_nothing() {
    let (link, _events, screens) = start("long");
    let mut runtime = connect(&link);
    // The runtime drops a message that does not fit (BASALT_CONTROL_MAX_MESSAGE_LENGTH): the longest that fits
    // still reaches the host
    let tip = "x".repeat(MAX_MESSAGE as usize - 2 - 4 - "game/interstitials".len() - r#"{"type": "begin_loading", "tipText": ""}"#.len());
    let text = format!(r#"{{"type": "begin_loading", "tipText": "{tip}"}}"#);
    runtime.write_all(&mailbox("game/interstitials", &text)).unwrap();
    assert_eq!(next(&screens).expect("the load shows").lines, [tip]);
    assert!(link.is_connected());
}
