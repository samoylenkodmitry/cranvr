use super::*;

/// The messages of Half-Life: Alyx at the start of its first level, in the form that OpenComposite logged, with made-up
/// lines in place of the game's tip and description
const BEGIN: &str = "{\n\t\"type\": \"begin_loading\",\n\t\"mapName\": \"a1_intro_world\",\n\t\"chapter\": \"\",\n\t\"tipText\": \"TIP: This line stands for a game's tip. It is long enough to wrap on the panel in the headset.\",\n\t\"description\": \"This line stands for the description of the level. A game sends a few sentences here while it loads, so the panel shows three or four lines of it.\",\n\t\"loadingText\": \"LOADING\",\n\t\"facingAngle\": 0.000000\n}";
const MESSAGE: &str = "{\n\t\"type\": \"show_message\",\n\t\"text\": \"Press Trigger To Start\"\n}";
const END: &str = "{\n\t\"type\": \"end_loading\"\n}";
const PATHS: &str = "{\n\t\"type\": \"set_paths\",\n\t\"webroot\": \"C:\\\\Game\\\\webui\",\n\t\"models\": \"C:\\\\Game\\\\models\\\\ui\\\\\"\n}";

fn shown(messages: &[&str]) -> Option<GameScreen> {
    let mut screen = Screen::default();
    for message in messages {
        screen.apply(read(message));
    }
    screen.shown().cloned()
}

#[test]
fn begin_loading_shows_the_words_the_tip_the_description_and_the_angle() {
    let screen = shown(&[BEGIN]).expect("a load shows");
    assert_eq!(screen.loading.as_deref(), Some("LOADING"));
    assert_eq!(screen.lines.len(), 2);
    assert!(screen.lines[0].starts_with("TIP: This line"), "{:?}", screen.lines);
    assert!(screen.lines[1].starts_with("This line stands for the description"), "{:?}", screen.lines);
    assert_eq!(screen.message, None);
    assert_eq!(screen.facing_deg, Some(0.0));
    // The empty chapter and a load with no words of its own
    let bare = shown(&[r#"{"type": "begin_loading", "mapName": "startup", "chapter": "", "tipText": "", "loadingText": ""}"#]).unwrap();
    assert_eq!(bare.loading.as_deref(), Some(LOADING));
    assert!(bare.lines.is_empty());
    assert_eq!(bare.facing_deg, None, "no angle puts the screen in front of the user");
}

#[test]
fn show_message_adds_the_message_to_the_load_and_a_new_load_drops_it() {
    let screen = shown(&[BEGIN, MESSAGE]).unwrap();
    assert_eq!(screen.message.as_deref(), Some("Press Trigger To Start"));
    assert_eq!(screen.loading.as_deref(), Some("LOADING"), "the load's words stay");
    assert_eq!(shown(&[BEGIN, MESSAGE, BEGIN]).unwrap().message, None);
    // An empty text hides the message
    assert_eq!(shown(&[BEGIN, MESSAGE, r#"{"type": "show_message", "text": ""}"#]).unwrap().message, None);
    // The game's page shows a message only during a load
    assert_eq!(shown(&[MESSAGE]), None);
}

#[test]
fn end_loading_hides_the_screen() {
    assert_eq!(shown(&[BEGIN, MESSAGE, END]), None);
    let mut screen = Screen::default();
    assert!(!screen.apply(read(END)), "an end with nothing shown changes nothing");
}

#[test]
fn set_paths_and_unknown_messages_change_nothing() {
    let mut screen = Screen::default();
    assert!(screen.apply(read(BEGIN)));
    for message in [PATHS, r#"{"type": "progress", "value": 0.5}"#, r#"{"text": "no type"}"#, "not json", "[1, 2]", ""] {
        assert_eq!(read(message), Change::Nothing, "{message}");
        assert!(!screen.apply(read(message)), "{message}");
    }
    assert!(screen.shown().is_some(), "the load still shows");
    assert!(!screen.apply(read(BEGIN)), "the same load again changes nothing");
}

#[test]
fn the_message_shows_as_plain_text() {
    let screen = shown(&[BEGIN, r#"{"type": "show_message", "text": "Press <b>Trigger</b> &amp; wait<br/>then go &lt;now&gt;"}"#]).unwrap();
    assert_eq!(screen.message.as_deref(), Some("Press Trigger & wait\nthen go <now>"));
    let screen = shown(&[BEGIN, r#"{"type": "show_message", "text": "a < b"}"#]).unwrap();
    assert_eq!(screen.message.as_deref(), Some("a < b"), "a lone angle bracket is text");
}

#[test]
fn the_log_line_names_what_shows() {
    assert_eq!(describe(None), "nothing shows");
    let line = describe(shown(&[BEGIN, MESSAGE]).as_ref());
    assert_eq!(line, "\"LOADING\", 2 lines, the message \"Press Trigger To Start\", at 0 degrees");
}
