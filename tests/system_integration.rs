use std::thread;
use std::time::{Duration, Instant};

use libghostty_vt::{Terminal, TerminalOptions, key, mouse};
use portable_pty::CommandBuilder;
use wisp::config::Config;
use wisp::input::{InputState, PointerEvent};
use wisp::terminal::{Command, Session, SessionEvent};

fn terminal() -> Terminal<'static, 'static> {
    Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100_000,
    })
    .unwrap()
}

fn key_bytes(
    state: &mut InputState,
    terminal: &mut Terminal<'static, 'static>,
    key: key::Key,
    mods: key::Mods,
    text: Option<&str>,
) -> Vec<u8> {
    state
        .key(
            terminal,
            &Config::default(),
            key,
            mods,
            key::Action::Press,
            text.map(str::to_owned),
            None,
        )
        .unwrap()
        .bytes
}

fn mouse_bytes(
    state: &mut InputState,
    terminal: &mut Terminal<'static, 'static>,
    action: mouse::Action,
    button: Option<mouse::Button>,
) -> Vec<u8> {
    state
        .pointer(
            terminal,
            &Config::default(),
            mouse::EncoderSize {
                screen_width: 660,
                screen_height: 412,
                cell_width: 8,
                cell_height: 16,
                padding_left: 12,
                padding_right: 8,
                padding_top: 16,
                padding_bottom: 12,
            },
            PointerEvent {
                action,
                button,
                mods: key::Mods::empty(),
                x: 28.0,
                y: 48.0,
            },
        )
        .unwrap()
        .bytes
}

#[test]
fn legacy_cursor_and_keypad_modes_follow_application_negotiation() {
    let mut terminal = terminal();
    let mut state = InputState::new().unwrap();
    for (key, normal, application) in [
        (
            key::Key::ArrowUp,
            b"\x1b[A".as_slice(),
            b"\x1bOA".as_slice(),
        ),
        (key::Key::ArrowDown, b"\x1b[B", b"\x1bOB"),
        (key::Key::ArrowRight, b"\x1b[C", b"\x1bOC"),
        (key::Key::ArrowLeft, b"\x1b[D", b"\x1bOD"),
        (key::Key::Home, b"\x1b[H", b"\x1bOH"),
        (key::Key::End, b"\x1b[F", b"\x1bOF"),
    ] {
        terminal.vt_write(b"\x1b[?1l");
        assert_eq!(
            key_bytes(&mut state, &mut terminal, key, key::Mods::empty(), None),
            normal
        );
        terminal.vt_write(b"\x1b[?1h");
        assert_eq!(
            key_bytes(&mut state, &mut terminal, key, key::Mods::empty(), None),
            application
        );
    }
    terminal.vt_write(b"\x1b[?1035l\x1b=");
    for (key, expected) in [
        (key::Key::Numpad0, b"\x1bOp".as_slice()),
        (key::Key::Numpad3, b"\x1bOs"),
        (key::Key::NumpadEnter, b"\x1bOM"),
    ] {
        assert_eq!(
            key_bytes(&mut state, &mut terminal, key, key::Mods::empty(), None),
            expected
        );
    }
    terminal.vt_write(b"\x1b>");
    assert_eq!(
        key_bytes(
            &mut state,
            &mut terminal,
            key::Key::Numpad3,
            key::Mods::NUM_LOCK,
            Some("3")
        ),
        b"3"
    );
}

#[test]
fn control_alt_and_modified_function_keys_use_standard_legacy_encodings() {
    let mut terminal = terminal();
    let mut state = InputState::new().unwrap();
    for (key, mods, text, expected) in [
        (key::Key::A, key::Mods::CTRL, Some("a"), b"\x01".as_slice()),
        (key::Key::I, key::Mods::CTRL, Some("i"), b"\t"),
        (key::Key::M, key::Mods::CTRL, Some("m"), b"\r"),
        (key::Key::BracketLeft, key::Mods::CTRL, Some("["), b"\x1b"),
        (key::Key::Space, key::Mods::CTRL, Some(" "), b"\0"),
        (key::Key::X, key::Mods::ALT, Some("x"), b"\x1bx"),
        (
            key::Key::A,
            key::Mods::CTRL | key::Mods::ALT,
            Some("a"),
            b"\x1b\x01",
        ),
        (key::Key::F5, key::Mods::SHIFT, None, b"\x1b[15;2~"),
        (key::Key::ArrowLeft, key::Mods::CTRL, None, b"\x1b[1;5D"),
    ] {
        assert_eq!(
            key_bytes(&mut state, &mut terminal, key, mods, text),
            expected,
            "{key:?} {mods:?}"
        );
    }
    terminal.vt_write(b"\x1b[>4;2m");
    assert_eq!(
        key_bytes(
            &mut state,
            &mut terminal,
            key::Key::I,
            key::Mods::CTRL,
            Some("i")
        ),
        b"\x1b[27;5;105~"
    );
    terminal.vt_write(b"\x1b[>4;0m");
    assert_eq!(
        key_bytes(
            &mut state,
            &mut terminal,
            key::Key::I,
            key::Mods::CTRL,
            Some("i")
        ),
        b"\t"
    );
}

#[test]
fn extended_function_keys_match_advertised_xterm_terminfo_sequences() {
    let mut terminal = terminal();
    let mut state = InputState::new().unwrap();
    for (key, expected) in [
        (key::Key::F13, "\x1b[1;2P"),
        (key::Key::F14, "\x1b[1;2Q"),
        (key::Key::F15, "\x1b[1;2R"),
        (key::Key::F16, "\x1b[1;2S"),
        (key::Key::F17, "\x1b[15;2~"),
        (key::Key::F18, "\x1b[17;2~"),
        (key::Key::F19, "\x1b[18;2~"),
        (key::Key::F20, "\x1b[19;2~"),
        (key::Key::F21, "\x1b[20;2~"),
        (key::Key::F22, "\x1b[21;2~"),
        (key::Key::F23, "\x1b[23;2~"),
        (key::Key::F24, "\x1b[24;2~"),
    ] {
        assert_eq!(
            key_bytes(&mut state, &mut terminal, key, key::Mods::empty(), None),
            expected.as_bytes(),
            "{key:?}"
        );
    }
    assert_eq!(
        key_bytes(
            &mut state,
            &mut terminal,
            key::Key::F3,
            key::Mods::CTRL,
            None
        ),
        b"\x1b[1;5R"
    );
    assert_eq!(
        key_bytes(
            &mut state,
            &mut terminal,
            key::Key::F3,
            key::Mods::ALT | key::Mods::SHIFT,
            None
        ),
        b"\x1b[1;4R"
    );
}

#[test]
fn kitty_negotiation_preserves_control_disambiguation_and_extended_function_identity() {
    let mut terminal = terminal();
    let mut state = InputState::new().unwrap();
    assert_eq!(
        key_bytes(
            &mut state,
            &mut terminal,
            key::Key::I,
            key::Mods::CTRL | key::Mods::ALT,
            Some("i")
        ),
        b"\x1b\t"
    );
    terminal.vt_write(b"\x1b[>1u");
    for (key, text, expected) in [
        (key::Key::I, "i", "\x1b[105;5u"),
        (key::Key::M, "m", "\x1b[109;5u"),
        (key::Key::BracketLeft, "[", "\x1b[91;5u"),
    ] {
        assert_eq!(
            key_bytes(&mut state, &mut terminal, key, key::Mods::CTRL, Some(text)),
            expected.as_bytes()
        );
    }
    assert_eq!(
        key_bytes(
            &mut state,
            &mut terminal,
            key::Key::F13,
            key::Mods::empty(),
            None
        ),
        b"\x1b[57376u"
    );
    terminal.vt_write(b"\x1b[<u");
    assert_eq!(
        key_bytes(
            &mut state,
            &mut terminal,
            key::Key::I,
            key::Mods::CTRL,
            Some("i")
        ),
        b"\t"
    );
}

#[test]
fn non_latin_control_aliases_follow_physical_keys_without_overriding_latin_remaps() {
    let mut terminal = terminal();
    let mut state = InputState::new().unwrap();
    let encode = |state: &mut InputState,
                  terminal: &mut Terminal<'static, 'static>,
                  physical,
                  text: &str,
                  mods| {
        state
            .key(
                terminal,
                &Config::default(),
                physical,
                mods,
                key::Action::Press,
                Some(text.to_owned()),
                text.chars().next(),
            )
            .unwrap()
            .bytes
    };
    for (physical, text, plain, alt) in [
        (key::Key::I, "ш", b"\t".as_slice(), b"\x1b\t".as_slice()),
        (key::Key::M, "ь", b"\r", b"\x1b\r"),
        (key::Key::BracketLeft, "х", b"\x1b", b"\x1b\x1b"),
    ] {
        assert_eq!(
            encode(&mut state, &mut terminal, physical, text, key::Mods::CTRL),
            plain
        );
        assert_eq!(
            encode(
                &mut state,
                &mut terminal,
                physical,
                text,
                key::Mods::CTRL | key::Mods::ALT
            ),
            alt
        );
    }
    assert_eq!(
        encode(&mut state, &mut terminal, key::Key::I, "c", key::Mods::CTRL),
        b"\x03"
    );
    assert_eq!(
        encode(&mut state, &mut terminal, key::Key::C, "i", key::Mods::CTRL),
        b"\t"
    );
    terminal.vt_write(b"\x1b[>4;2m");
    assert_eq!(
        encode(&mut state, &mut terminal, key::Key::I, "ш", key::Mods::CTRL),
        b"\x1b[27;5;1096~"
    );
    terminal.vt_write(b"\x1b[>4;0m\x1b[>1u");
    assert_eq!(
        encode(&mut state, &mut terminal, key::Key::I, "ш", key::Mods::CTRL),
        b"\x1b[1096;5u"
    );
}

#[test]
fn numlock_off_keypad_navigation_uses_logical_keys_in_legacy_and_kitty_modes() {
    use winit::keyboard::{Key, KeyCode, NamedKey};
    let mut terminal = terminal();
    let mut state = InputState::new().unwrap();
    for (physical, logical, expected) in [
        (KeyCode::Numpad1, NamedKey::End, "\x1b[F"),
        (KeyCode::Numpad2, NamedKey::ArrowDown, "\x1b[B"),
        (KeyCode::Numpad3, NamedKey::PageDown, "\x1b[6~"),
        (KeyCode::Numpad4, NamedKey::ArrowLeft, "\x1b[D"),
        (KeyCode::Numpad5, NamedKey::Clear, "\x1b[E"),
        (KeyCode::Numpad6, NamedKey::ArrowRight, "\x1b[C"),
        (KeyCode::Numpad7, NamedKey::Home, "\x1b[H"),
        (KeyCode::Numpad8, NamedKey::ArrowUp, "\x1b[A"),
        (KeyCode::Numpad9, NamedKey::PageUp, "\x1b[5~"),
        (KeyCode::Numpad0, NamedKey::Insert, "\x1b[2~"),
        (KeyCode::NumpadDecimal, NamedKey::Delete, "\x1b[3~"),
    ] {
        let key = wisp::input::map_numpad_key(physical, &Key::Named(logical)).unwrap();
        assert_eq!(
            key_bytes(&mut state, &mut terminal, key, key::Mods::empty(), None),
            expected.as_bytes()
        );
    }
    assert!(wisp::input::map_numpad_key(KeyCode::Numpad1, &Key::Character("1".into())).is_none());
    assert!(
        wisp::input::map_numpad_key(KeyCode::ArrowDown, &Key::Named(NamedKey::ArrowDown)).is_none()
    );
    terminal.vt_write(b"\x1b[?1h");
    let key = wisp::input::map_numpad_key(KeyCode::Numpad1, &Key::Named(NamedKey::End)).unwrap();
    assert_eq!(
        key_bytes(&mut state, &mut terminal, key, key::Mods::empty(), None),
        b"\x1bOF"
    );
    terminal.vt_write(b"\x1b[>1u");
    assert_eq!(
        key_bytes(&mut state, &mut terminal, key, key::Mods::empty(), None),
        b"\x1b[57424u"
    );
}

#[test]
fn keyboard_action_mode_locks_keys_and_ime_but_preserves_paste_and_mouse() {
    let mut terminal = terminal();
    let mut state = InputState::new().unwrap();
    terminal.vt_write(b"\x1b[2h");
    assert!(
        key_bytes(
            &mut state,
            &mut terminal,
            key::Key::A,
            key::Mods::empty(),
            Some("a")
        )
        .is_empty()
    );
    assert!(
        state
            .text(&mut terminal, "漢".as_bytes().to_vec())
            .unwrap()
            .bytes
            .is_empty()
    );
    assert_eq!(
        state
            .paste(&mut terminal, "pasted".to_owned(), &Config::default())
            .unwrap()
            .bytes,
        b"pasted"
    );
    terminal.vt_write(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        mouse_bytes(
            &mut state,
            &mut terminal,
            mouse::Action::Press,
            Some(mouse::Button::Left)
        ),
        b"\x1b[<0;3;3M"
    );
    terminal.vt_write(b"\x1b[2l");
    assert_eq!(
        key_bytes(
            &mut state,
            &mut terminal,
            key::Key::A,
            key::Mods::empty(),
            Some("a")
        ),
        b"a"
    );
    terminal.vt_write(b"\x1b[2h\x1b[!p");
    assert_eq!(
        key_bytes(
            &mut state,
            &mut terminal,
            key::Key::A,
            key::Mods::empty(),
            Some("a")
        ),
        b"a"
    );
}

#[test]
fn negotiated_mouse_formats_preserve_padding_and_horizontal_buttons() {
    for (mode, expected) in [
        (b"\x1b[?1006h".as_slice(), b"\x1b[<0;3;3M".as_slice()),
        (b"\x1b[?1015h", b"\x1b[32;3;3M"),
        (b"\x1b[?1016h", b"\x1b[<0;16;32M"),
        (b"", b"\x1b[M ##"),
    ] {
        let mut terminal = terminal();
        let mut state = InputState::new().unwrap();
        terminal.vt_write(b"\x1b[?1000h");
        terminal.vt_write(mode);
        assert_eq!(
            mouse_bytes(
                &mut state,
                &mut terminal,
                mouse::Action::Press,
                Some(mouse::Button::Left)
            ),
            expected,
            "mode {mode:?}"
        );
    }
    let mut terminal = terminal();
    let mut state = InputState::new().unwrap();
    terminal.vt_write(b"\x1b[?1000h\x1b[?1006h");
    for (button, code) in [
        (mouse::Button::Six, 66),
        (mouse::Button::Seven, 67),
        (mouse::Button::Eight, 128),
        (mouse::Button::Nine, 129),
    ] {
        assert_eq!(
            mouse_bytes(
                &mut state,
                &mut terminal,
                mouse::Action::Press,
                Some(button)
            ),
            format!("\x1b[<{code};3;3M").as_bytes()
        );
    }
}

#[test]
fn extra_mouse_buttons_keep_drag_tracking_until_their_release() {
    let mut terminal = terminal();
    let mut state = InputState::new().unwrap();
    terminal.vt_write(b"\x1b[?1002h\x1b[?1006h");
    assert_eq!(
        mouse_bytes(
            &mut state,
            &mut terminal,
            mouse::Action::Press,
            Some(mouse::Button::Eight)
        ),
        b"\x1b[<128;3;3M"
    );
    assert_eq!(
        mouse_bytes(&mut state, &mut terminal, mouse::Action::Motion, None),
        b"\x1b[<160;3;3M"
    );
    assert_eq!(
        mouse_bytes(
            &mut state,
            &mut terminal,
            mouse::Action::Release,
            Some(mouse::Button::Eight)
        ),
        b"\x1b[<128;3;3m"
    );
    assert!(mouse_bytes(&mut state, &mut terminal, mouse::Action::Motion, None).is_empty());
}

fn wait_title(session: &Session, prefix: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Title(title) if title.starts_with(prefix) => return title,
                SessionEvent::Error(error) => panic!("{error}"),
                SessionEvent::Exited => panic!("child exited before {prefix}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for {prefix}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn child_process_has_resolvable_terminfo_identity_and_correct_resized_pty_dimensions() {
    let probe = r#"
import array, curses, fcntl, os, termios, tty
tty.setraw(0)
os.write(1, b'\x1b]2;system-ready\x1b\\')
assert os.read(0, 1) == b's'
curses.setupterm(fd=1)
size = array.array('H', [0, 0, 0, 0])
fcntl.ioctl(0, termios.TIOCGWINSZ, size, True)
values = [os.environ.get(key, '') for key in ['TERM', 'TERM_PROGRAM', 'TERM_PROGRAM_VERSION', 'COLORTERM']]
values += [str(curses.tigetnum('colors')), ','.join(map(str, size))]
os.write(1, ('\x1b]2;system-result:' + '|'.join(values) + '\x1b\\').encode())
while os.read(0, 1) != b'\x04': pass
"#;
    let mut command = CommandBuilder::new("python3");
    command.args(["-u", "-c", probe]);
    let session = Session::spawn_command(Config::default(), 80, 24, command).unwrap();
    wait_title(&session, "system-ready");
    session.send(Command::Resize {
        cols: 93,
        rows: 31,
        cell_width: 8,
        cell_height: 17,
        pixel_width: 744,
        pixel_height: 527,
    });
    session.send(Command::Input(b"s".to_vec()));
    let title = wait_title(&session, "system-result:");
    let values = title
        .strip_prefix("system-result:")
        .unwrap()
        .split('|')
        .collect::<Vec<_>>();
    assert!(["xterm-ghostty", "xterm-256color", "xterm", "vt100"].contains(&values[0]));
    assert_eq!(values[1], "wisp");
    assert_eq!(values[2], env!("CARGO_PKG_VERSION"));
    assert_eq!(values[3], "truecolor");
    if matches!(values[0], "xterm-ghostty" | "xterm-256color") {
        assert!(values[4].parse::<i32>().unwrap() >= 256);
    }
    assert_eq!(values[5], "31,93,744,527");
    session.send(Command::Input(vec![4]));
}

#[test]
fn control_i_completes_a_filename_in_a_real_bash_readline_session() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("wisp-readline-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("wisp_completion_target"), b"").unwrap();
    let mut command = CommandBuilder::new("bash");
    command.args(["--noprofile", "--norc", "-i"]);
    command.cwd(&directory);
    command.env("INPUTRC", "/dev/null");
    command.env("PS1", "\x1b]2;readline-ready\x07PROMPT> ");
    let session = Session::spawn_command(Config::default(), 100, 24, command).unwrap();
    wait_title(&session, "readline-ready");
    session.send(Command::Input(b"printf '%s\\n' ./wisp_comp".to_vec()));
    session.send(Command::Key {
        key: key::Key::I,
        mods: key::Mods::CTRL,
        action: key::Action::Press,
        text: Some("i".to_owned()),
        unshifted: Some('i'),
    });
    session.send(Command::Key {
        key: key::Key::Enter,
        mods: key::Mods::empty(),
        action: key::Action::Press,
        text: None,
        unshifted: None,
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(snapshot) = session.try_snapshot() {
            let output = snapshot
                .cells
                .chunks(usize::from(snapshot.cols))
                .map(|row| {
                    row.iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                })
                .collect::<Vec<_>>();
            if output.iter().any(|line| line == "./wisp_completion_target") {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "Ctrl+I did not complete the readline filename"
        );
        thread::sleep(Duration::from_millis(1));
    }
    session.send(Command::Input(b"exit\r".to_vec()));
    drop(session);
    std::fs::remove_dir_all(directory).unwrap();
}
