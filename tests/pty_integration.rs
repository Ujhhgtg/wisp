#![cfg(unix)]

use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use libghostty_vt::{key, mouse};
use portable_pty::CommandBuilder;
use wisp::config::Config;
use wisp::terminal::{Command, Session, SessionEvent};

fn spawn_probe(scenario: &str) -> Session {
    let mut command = CommandBuilder::new("python3");
    command.arg("-u");
    command.arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pty_probe.py"));
    command.arg(scenario);
    let session = Session::spawn_command(Config::default(), 80, 24, command).unwrap();
    session.send(Command::Resize {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 20,
        pixel_width: 640,
        pixel_height: 480,
    });
    assert_eq!(wait_title(&session, "probe-ready"), "probe-ready");
    session
}

fn wait_title(session: &Session, prefix: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Title(title) if title.starts_with(prefix) => return title,
                SessionEvent::Error(error) => panic!("PTY session failed: {error}"),
                SessionEvent::Exited => panic!("PTY exited before {prefix}"),
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    panic!("timed out waiting for {prefix}");
}

fn finish(session: &Session) -> Vec<u8> {
    session.send(Command::Input(vec![4]));
    let title = wait_title(session, "probe-result:");
    let hex = title.strip_prefix("probe-result:").unwrap();
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn wait_clipboard(session: &Session) -> String {
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Clipboard(text) => return text,
                SessionEvent::Error(error) => panic!("PTY session failed: {error}"),
                SessionEvent::Exited => panic!("PTY exited before copy completed"),
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    panic!("timed out waiting for copied selection");
}

fn key(session: &Session, key: key::Key, mods: key::Mods, action: key::Action, text: Option<&str>) {
    session.send(Command::Key {
        key,
        mods,
        action,
        text: text.map(str::to_owned),
        unshifted: None,
    });
}

fn pointer(
    session: &Session,
    action: mouse::Action,
    button: Option<mouse::Button>,
    x: f64,
    y: f64,
) {
    session.send(Command::Mouse {
        action,
        button,
        mods: key::Mods::empty(),
        x,
        y,
    });
}

#[test]
fn ordinary_raw_program_receives_no_mouse_or_focus_sequences() {
    let session = spawn_probe("plain");
    session.send(Command::Focus(true));
    session.send(Command::Focus(false));
    pointer(
        &session,
        mouse::Action::Press,
        Some(mouse::Button::Left),
        16.0,
        20.0,
    );
    pointer(&session, mouse::Action::Motion, None, 40.0, 20.0);
    pointer(
        &session,
        mouse::Action::Release,
        Some(mouse::Button::Left),
        40.0,
        20.0,
    );
    pointer(
        &session,
        mouse::Action::Press,
        Some(mouse::Button::Four),
        40.0,
        20.0,
    );
    pointer(
        &session,
        mouse::Action::Press,
        Some(mouse::Button::Five),
        40.0,
        20.0,
    );
    assert_eq!(finish(&session), b"");
}

#[test]
fn enabled_sgr_mouse_uses_kitty_button_and_cell_coordinates() {
    let session = spawn_probe("mouse");
    pointer(
        &session,
        mouse::Action::Press,
        Some(mouse::Button::Left),
        16.0,
        20.0,
    );
    pointer(&session, mouse::Action::Motion, None, 40.0, 20.0);
    pointer(
        &session,
        mouse::Action::Release,
        Some(mouse::Button::Left),
        40.0,
        20.0,
    );
    pointer(
        &session,
        mouse::Action::Press,
        Some(mouse::Button::Four),
        40.0,
        20.0,
    );
    pointer(
        &session,
        mouse::Action::Press,
        Some(mouse::Button::Five),
        40.0,
        20.0,
    );
    assert_eq!(
        finish(&session),
        b"\x1b[<0;3;2M\x1b[<0;6;2m\x1b[<64;6;2M\x1b[<65;6;2M"
    );
}

#[test]
fn button_tracking_reports_drag_and_stops_after_release() {
    let session = spawn_probe("drag");
    pointer(&session, mouse::Action::Motion, None, 8.0, 0.0);
    pointer(
        &session,
        mouse::Action::Press,
        Some(mouse::Button::Left),
        16.0,
        20.0,
    );
    pointer(&session, mouse::Action::Motion, None, 40.0, 20.0);
    pointer(
        &session,
        mouse::Action::Release,
        Some(mouse::Button::Left),
        40.0,
        20.0,
    );
    pointer(&session, mouse::Action::Motion, None, 80.0, 40.0);
    assert_eq!(finish(&session), b"\x1b[<0;3;2M\x1b[<32;6;2M\x1b[<0;6;2m");
}

#[test]
fn focus_reports_only_when_child_negotiates_focus_mode() {
    let session = spawn_probe("focus");
    session.send(Command::Focus(true));
    session.send(Command::Focus(false));
    assert_eq!(finish(&session), b"\x1b[I\x1b[O");
}

#[test]
fn legacy_keyboard_delivers_printable_modified_and_function_keys() {
    let session = spawn_probe("plain");
    key(
        &session,
        key::Key::A,
        key::Mods::empty(),
        key::Action::Press,
        Some("a"),
    );
    key(
        &session,
        key::Key::A,
        key::Mods::empty(),
        key::Action::Release,
        None,
    );
    key(
        &session,
        key::Key::C,
        key::Mods::CTRL,
        key::Action::Press,
        Some("c"),
    );
    key(
        &session,
        key::Key::Enter,
        key::Mods::empty(),
        key::Action::Press,
        None,
    );
    key(
        &session,
        key::Key::Tab,
        key::Mods::empty(),
        key::Action::Press,
        None,
    );
    key(
        &session,
        key::Key::Backspace,
        key::Mods::empty(),
        key::Action::Press,
        None,
    );
    key(
        &session,
        key::Key::ArrowUp,
        key::Mods::empty(),
        key::Action::Press,
        None,
    );
    assert_eq!(finish(&session), b"a\x03\r\t\x7f\x1b[A");
}

#[test]
fn kitty_keyboard_negotiation_reports_control_key_release() {
    let session = spawn_probe("kitty");
    key(
        &session,
        key::Key::C,
        key::Mods::CTRL,
        key::Action::Press,
        Some("c"),
    );
    key(
        &session,
        key::Key::C,
        key::Mods::CTRL,
        key::Action::Release,
        None,
    );
    assert_eq!(finish(&session), b"\x1b[99;5u\x1b[99;5:3u");
}

#[test]
fn bracketed_paste_is_negotiated_and_control_bytes_cannot_end_it() {
    let session = spawn_probe("paste");
    session.send(Command::Paste("first\nsecond\x1b[201~".to_owned()));
    assert_eq!(finish(&session), b"\x1b[200~first\nsecond [201~\x1b[201~");
}

#[test]
fn ordinary_paste_converts_newlines_to_carriage_returns() {
    let session = spawn_probe("plain");
    session.send(Command::Paste("first\nsecond".to_owned()));
    assert_eq!(finish(&session), b"first\rsecond");
}

#[test]
fn dangerous_user_paste_waits_for_approval_and_sanitizes_like_kitty() {
    let session = spawn_probe("plain");
    session.send(Command::RequestPaste("one\n\x03two".to_owned()));
    let deadline = Instant::now() + Duration::from_secs(4);
    let sanitized = loop {
        if let Some(SessionEvent::PasteWarning {
            original,
            sanitized,
        }) = session.try_event()
        {
            assert_eq!(original, "one\n\x03two");
            break sanitized;
        }
        assert!(Instant::now() < deadline, "paste warning missing");
        thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(sanitized, "one\x1bE␃two");
    session.send(Command::PasteApproved(sanitized));
    assert_eq!(finish(&session), "one\x1bE␃two".as_bytes());
}

#[test]
fn bracketed_newlines_do_not_warn_and_approved_paste_strips_only_terminators() {
    let session = spawn_probe("paste");
    session.send(Command::RequestPaste("one\ntwo".to_owned()));
    assert_eq!(finish(&session), b"\x1b[200~one\ntwo\x1b[201~");
    let session = spawn_probe("paste");
    session.send(Command::PasteApproved(
        "\x03\x1b[201~\x1b[20\x1b[201~1~ok".to_owned(),
    ));
    assert_eq!(finish(&session), b"\x1b[200~\x03ok\x1b[201~");
}

#[test]
fn paste_anyway_preserves_control_codes_and_cancelling_delivers_nothing() {
    let session = spawn_probe("plain");
    session.send(Command::PasteApproved("one\r\ntwo\x03".to_owned()));
    assert_eq!(finish(&session), b"one\rtwo\x03");
    let session = spawn_probe("plain");
    session.send(Command::RequestPaste("one\n".to_owned()));
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if let Some(SessionEvent::PasteWarning { .. }) = session.try_event() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(finish(&session), b"");
}

#[test]
fn real_pty_round_trip_answers_terminal_device_status_queries() {
    let session = spawn_probe("queries");
    assert_eq!(finish(&session), b"\x1b[7;13R\x1b[0n");
}

#[test]
fn terminal_size_queries_report_cells_and_content_pixels() {
    let session = spawn_probe("size");
    assert_eq!(
        finish(&session),
        b"\x1b[4;480;640t\x1b[6;20;8t\x1b[8;24;80t"
    );
}

#[test]
fn pointer_cell_reports_exclude_the_window_margin() {
    let session = spawn_probe("mouse");
    let geometry = wisp::geometry::GridGeometry::new(
        (783, 549),
        (8, 20),
        1.0,
        wisp::config::Margins::default(),
    );
    session.send(Command::Geometry(geometry));
    let (x, y) = geometry.cell_origin(2, 1);
    pointer(
        &session,
        mouse::Action::Press,
        Some(mouse::Button::Left),
        f64::from(x),
        f64::from(y),
    );
    pointer(
        &session,
        mouse::Action::Release,
        Some(mouse::Button::Left),
        f64::from(x),
        f64::from(y),
    );
    assert_eq!(finish(&session), b"\x1b[<0;3;2M\x1b[<0;3;2m");
}

#[test]
fn real_pointer_selection_copies_text_and_typing_clears_the_highlight() {
    let session = spawn_probe("selection");
    pointer(
        &session,
        mouse::Action::Press,
        Some(mouse::Button::Left),
        1.0,
        10.0,
    );
    pointer(&session, mouse::Action::Motion, None, 40.0, 10.0);
    pointer(
        &session,
        mouse::Action::Release,
        Some(mouse::Button::Left),
        40.0,
        10.0,
    );
    session.send(Command::CopySelection);
    assert_eq!(wait_clipboard(&session), "hello");
    key(
        &session,
        key::Key::A,
        key::Mods::empty(),
        key::Action::Press,
        Some("a"),
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut cleared = false;
    while Instant::now() < deadline {
        if let Some(snapshot) = session.try_snapshot()
            && snapshot.cells.iter().all(|cell| !cell.selected)
        {
            cleared = true;
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(cleared, "typing must publish a selection-free snapshot");
    assert_eq!(finish(&session), b"a");
}
