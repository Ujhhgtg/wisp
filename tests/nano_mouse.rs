use std::error::Error;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use libghostty_vt::{key, mouse};
use portable_pty::CommandBuilder;
use wisp::config::Config;
use wisp::terminal::{Command, Session, SessionEvent, TerminalSnapshot};

#[test]
fn actual_nano_mouse_moves_vt_cursor_after_an_idle_period()
-> Result<(), Box<dyn Error + Send + Sync>> {
    if !std::process::Command::new("nano")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("nano unavailable; skipping external mouse integration");
        return Ok(());
    }
    let mut command = CommandBuilder::new("python3");
    command.arg("-u");
    command.arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nano_probe.py"));
    let session = Session::spawn_command(Config::default(), 80, 24, command)?;
    session.send(Command::Resize {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 20,
        pixel_width: 640,
        pixel_height: 480,
    });
    let ready = wait_snapshot(&session, |snapshot| {
        snapshot.mouse_reporting && find_row(snapshot, "alpha bravo").is_some()
    })?;
    let first_text_row = find_row(&ready, "alpha bravo").expect("nano first text row");
    let second_text_row = find_row(&ready, "second line").expect("nano second text row");
    eprintln!(
        "nano ready: cursor=({}, {}) shape={:?} visible={} blinking={} first_text_row={first_text_row}",
        ready.cursor.column,
        ready.cursor.row,
        ready.cursor.shape,
        ready.cursor.visible,
        ready.cursor.blinking
    );
    thread::sleep(Duration::from_millis(750));
    let clicked = Instant::now();
    for action in [mouse::Action::Press, mouse::Action::Release] {
        session.send(Command::Mouse {
            action,
            button: Some(mouse::Button::Left),
            mods: key::Mods::empty(),
            x: 13.0 * 8.0 + 4.0,
            y: f64::from(second_text_row) * 20.0 + 10.0,
        });
    }
    let moved = wait_snapshot(&session, |snapshot| {
        snapshot.cursor.column == 13 && snapshot.cursor.row == second_text_row
    })?;
    eprintln!(
        "nano mouse after idle: {:.3}ms cursor=({}, {}) shape={:?} visible={} blinking={}",
        clicked.elapsed().as_secs_f64() * 1000.0,
        moved.cursor.column,
        moved.cursor.row,
        moved.cursor.shape,
        moved.cursor.visible,
        moved.cursor.blinking
    );
    assert!(moved.cursor.visible);
    session.send(Command::Key {
        key: key::Key::X,
        mods: key::Mods::CTRL,
        action: key::Action::Press,
        text: Some("x".to_owned()),
        unshifted: Some('x'),
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Title(title) if title == "nano-status:0" => return Ok(()),
                SessionEvent::Error(error) => return Err(error.into()),
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    Err("nano did not exit cleanly after Ctrl+X".into())
}

fn wait_snapshot(
    session: &Session,
    predicate: impl Fn(&TerminalSnapshot) -> bool,
) -> Result<TerminalSnapshot, Box<dyn Error + Send + Sync>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_cursor = None;
    while Instant::now() < deadline {
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Error(error) => return Err(error.into()),
                SessionEvent::Title(title) if title.starts_with("nano-status:") => {
                    return Err(format!("nano exited early: {title}").into());
                }
                _ => {}
            }
        }
        if let Some(snapshot) = session.try_snapshot() {
            last_cursor = Some(snapshot.cursor);
            if predicate(&snapshot) {
                return Ok(snapshot);
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    Err(format!("timed out waiting for nano snapshot; last cursor={last_cursor:?}").into())
}

fn find_row(snapshot: &TerminalSnapshot, needle: &str) -> Option<u16> {
    (0..snapshot.rows).find(|row| {
        let offset = usize::from(*row) * usize::from(snapshot.cols);
        snapshot.cells[offset..offset + usize::from(snapshot.cols)]
            .iter()
            .map(|cell| {
                if cell.text.is_empty() {
                    " "
                } else {
                    &cell.text
                }
            })
            .collect::<String>()
            .contains(needle)
    })
}
