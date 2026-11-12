#![cfg(unix)]

use std::thread;
use std::time::{Duration, Instant};

use portable_pty::CommandBuilder;
use wisp::config::Config;
use wisp::terminal::{Command, Session, SessionEvent, TerminalSnapshot};

const PROBE: &str = r#"
import os
import sys
import tty

tty.setraw(0)
os.write(1, b'\x1b[2J\x1b[HBEFORE\x1b]2;sync-ready\x1b\\')
assert os.read(0, 1) == b's'
os.write(1, b'\x1b[?2026h\x1b[HAFTER!\x1b[5n')
reply = b''
while not reply.endswith(b'\x1b[0n'):
    reply += os.read(0, 32)
os.write(1, b'\x1b]2;sync-query-answered\x1b\\')
if sys.argv[1] == 'refresh':
    assert os.read(0, 1) == b'f'
    os.write(1, b'\x1b[?2026h\x1b]2;sync-refreshed\x1b\\')
assert os.read(0, 1) == b'r'
if sys.argv[1] in ('release', 'refresh'):
    os.write(1, b'\x1b[?2026l')
else:
    os.write(1, b'\x1b[?2026$p')
    reply = b''
    while not reply.endswith(b'$y'):
        reply += os.read(0, 32)
    assert reply == b'\x1b[?2026;2$y', reply
os.write(1, b'\x1b]2;sync-done\x1b\\')
while os.read(0, 1) != b'\x04':
    pass
"#;

fn spawn(scenario: &str) -> Session {
    let mut command = CommandBuilder::new("python3");
    command.args(["-u", "-c", PROBE, scenario]);
    let session = Session::spawn_command(Config::default(), 80, 24, command).unwrap();
    wait_title(&session, "sync-ready");
    wait_frame(&session, "BEFORE", Duration::from_secs(2));
    session
}

fn wait_title(session: &Session, title: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Title(current) if current == title => return,
                SessionEvent::Error(error) => panic!("{error}"),
                SessionEvent::Exited => panic!("child exited before {title}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for {title}");
        thread::sleep(Duration::from_millis(1));
    }
}

fn contains(frame: &TerminalSnapshot, text: &str) -> bool {
    frame
        .cells
        .iter()
        .map(|cell| cell.text.as_str())
        .collect::<String>()
        .contains(text)
}

fn wait_frame(session: &Session, text: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if session
            .try_snapshot()
            .is_some_and(|frame| contains(&frame, text))
        {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {text}");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn synchronized_output_hides_intermediate_frames_but_keeps_query_replies_live() {
    let session = spawn("release");
    session.send(Command::Input(b"s".to_vec()));
    wait_title(&session, "sync-query-answered");
    let deadline = Instant::now() + Duration::from_millis(120);
    while Instant::now() < deadline {
        assert!(
            session.try_snapshot().is_none(),
            "published a synchronized frame"
        );
        thread::sleep(Duration::from_millis(1));
    }
    session.send(Command::Input(b"r".to_vec()));
    wait_title(&session, "sync-done");
    wait_frame(&session, "AFTER!", Duration::from_secs(2));
    session.send(Command::Input(vec![4]));
}

#[test]
fn abandoned_synchronized_output_times_out_and_resets_the_reported_mode() {
    let session = spawn("timeout");
    let started = Instant::now();
    session.send(Command::Input(b"s".to_vec()));
    wait_title(&session, "sync-query-answered");
    wait_frame(&session, "AFTER!", Duration::from_secs(3));
    assert!(started.elapsed() >= Duration::from_millis(900));
    session.send(Command::Input(b"r".to_vec()));
    wait_title(&session, "sync-done");
    session.send(Command::Input(vec![4]));
}

#[test]
fn repeated_synchronized_output_requests_refresh_the_watchdog() {
    let session = spawn("refresh");
    session.send(Command::Input(b"s".to_vec()));
    wait_title(&session, "sync-query-answered");
    thread::sleep(Duration::from_millis(600));
    assert!(session.try_snapshot().is_none());
    session.send(Command::Input(b"f".to_vec()));
    wait_title(&session, "sync-refreshed");
    let deadline = Instant::now() + Duration::from_millis(600);
    while Instant::now() < deadline {
        assert!(
            session.try_snapshot().is_none(),
            "watchdog was not refreshed"
        );
        thread::sleep(Duration::from_millis(1));
    }
    session.send(Command::Input(b"r".to_vec()));
    wait_title(&session, "sync-done");
    wait_frame(&session, "AFTER!", Duration::from_secs(2));
    session.send(Command::Input(vec![4]));
}
