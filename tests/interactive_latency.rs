#![cfg(unix)]

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use libghostty_vt::key;
use portable_pty::CommandBuilder;
use wisp::config::Config;
use wisp::terminal::{Command, Session, SessionEvent};

#[test]
fn interactive_echo_is_not_delayed_by_the_bulk_snapshot_timer() {
    let child = r#"
import os, tty
tty.setraw(0)
os.write(1, b'\x1b[2J\x1b[H\x1b]2;ready\x1b\\')
while True:
    byte = os.read(0, 1)
    if not byte or byte == b'\x04': break
    os.write(1, byte)
"#;
    let (sender, receiver) = mpsc::channel();
    let mut command = CommandBuilder::new("python3");
    command.args(["-u", "-c", child]);
    let session = Session::spawn_command_with_waker(
        Config::default(),
        80,
        24,
        command,
        Arc::new(move || {
            let _ = sender.send(());
        }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut ready = false;
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Title(title) if title == "ready" => ready = true,
                SessionEvent::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
        if ready {
            break;
        }
        assert!(Instant::now() < deadline);
        let _ = receiver.recv_timeout(Duration::from_millis(10));
    }
    let mut latencies = Vec::new();
    for column in 1..=24 {
        while session.try_snapshot().is_some() {}
        std::thread::sleep(Duration::from_millis(20));
        let started = Instant::now();
        session.send(Command::Key {
            key: key::Key::X,
            mods: key::Mods::empty(),
            action: key::Action::Press,
            text: Some("x".to_owned()),
            unshifted: Some('x'),
        });
        loop {
            if session
                .try_snapshot()
                .is_some_and(|frame| frame.cursor.column == column)
            {
                latencies.push(started.elapsed());
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            let _ = receiver.recv_timeout(Duration::from_millis(1));
        }
    }
    latencies.sort_unstable();
    assert!(
        latencies[latencies.len() / 2] < Duration::from_millis(10),
        "median echo {:?} hit bulk snapshot timer",
        latencies[latencies.len() / 2]
    );
    session.send(Command::Input(vec![4]));
}
