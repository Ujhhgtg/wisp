#![cfg(windows)]

use std::thread;
use std::time::{Duration, Instant};

use libghostty_vt::key;
use portable_pty::CommandBuilder;
use wisp::close_confirmation::{close_message, needs_confirmation, shell_was_replaced};
use wisp::config::Config;
use wisp::terminal::{Command, Session, SessionEvent, TerminalSnapshot};

#[test]
fn windows_shell_identity_and_process_queries_work() {
    let current = std::env::current_exe().unwrap();
    let pid = std::process::id();
    assert!(!shell_was_replaced(Some(pid), &current).unwrap());
    assert!(!needs_confirmation(Some(u32::MAX), true).unwrap());
    assert!(needs_confirmation(Some(pid), true).unwrap());
    let message = close_message(Some(pid));
    assert!(
        message.starts_with("Are you sure you want to close this window? It is running: "),
        "{message}"
    );
    assert!(!message.ends_with("running: a program."), "{message}");
}

#[test]
fn windows_descendant_processes_require_close_confirmation() {
    let mut child = std::process::Command::new("cmd.exe")
        .args(["/D", "/Q", "/C", "ping.exe -n 4 127.0.0.1 >nul"])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !needs_confirmation(Some(child.id()), false).unwrap() {
        assert!(Instant::now() < deadline, "child process was not detected");
        thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn conpty_session_round_trips_output() {
    let mut command = CommandBuilder::new("cmd.exe");
    command.args(["/D", "/Q", "/C", "echo wisp-windows-ready"]);
    let session = Session::spawn_command(Config::default(), 80, 24, command).unwrap();
    assert!(session.needs_close_confirmation().unwrap());
    session.send(Command::Resize {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 20,
        pixel_width: 640,
        pixel_height: 480,
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut output_seen = false;
    let mut exited = false;
    loop {
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Error(error) => panic!("ConPTY session failed: {error}"),
                SessionEvent::Exited => exited = true,
                _ => {}
            }
        }
        output_seen |= session.try_snapshot().is_some_and(|snapshot| {
            snapshot
                .cells
                .chunks(usize::from(snapshot.cols))
                .any(|row| {
                    row.iter()
                        .map(|cell| cell.text.as_str())
                        .collect::<String>()
                        .contains("wisp-windows-ready")
                })
        });
        if output_seen && exited {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for ConPTY output"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_snapshot(
    session: &Session,
    predicate: impl Fn(&TerminalSnapshot) -> bool,
) -> TerminalSnapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Error(error) => panic!("terminal session failed: {error}"),
                SessionEvent::Exited => panic!("terminal exited before the expected frame"),
                _ => {}
            }
        }
        if let Some(snapshot) = session.try_snapshot()
            && predicate(&snapshot)
        {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "timed out waiting for a frame");
        thread::sleep(Duration::from_millis(5));
    }
}

fn first_text_row(snapshot: &TerminalSnapshot) -> String {
    snapshot
        .cells
        .chunks(usize::from(snapshot.cols))
        .map(|row| {
            row.iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>()
        })
        .find(|row| !row.trim().is_empty())
        .unwrap_or_default()
}

#[test]
fn nushell_autoload_prevents_osc133_scroll() {
    if !std::process::Command::new("nu")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("Nushell unavailable; skipping autoload regression");
        return;
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let config = std::env::temp_dir().join(format!(
        "wisp-nushell-osc133-on-{}-{nonce}.nu",
        std::process::id()
    ));
    std::fs::write(&config, "$env.config.shell_integration.osc133 = true\n").unwrap();

    let mut command = CommandBuilder::new("nu.exe");
    command.args(["--config", config.to_str().unwrap(), "--no-history"]);
    command.env_remove("NU_VENDOR_AUTOLOAD_DIR");
    let session = Session::spawn_command(Config::default(), 80, 24, command).unwrap();
    let initial = wait_snapshot(&session, |snapshot| first_text_row(snapshot).contains("__"));
    let top = first_text_row(&initial);
    session.send(Command::Key {
        key: key::Key::A,
        mods: key::Mods::empty(),
        action: key::Action::Press,
        text: Some("a".to_owned()),
        unshifted: Some('a'),
    });
    let typed = wait_snapshot(&session, |snapshot| {
        snapshot.cursor.column > initial.cursor.column || snapshot.cursor.row != initial.cursor.row
    });
    assert_eq!(first_text_row(&typed), top, "typing scrolled the screen");
    std::fs::remove_file(config).unwrap();
}

#[test]
fn configured_nushell_shim_is_not_treated_as_a_running_job() {
    if !std::process::Command::new("nu")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("Nushell unavailable; skipping shim close-confirmation regression");
        return;
    }
    let config = Config {
        shell: Some(std::path::PathBuf::from("nu")),
        ..Config::default()
    };
    let session = Session::spawn(config, 80, 24).unwrap();
    wait_snapshot(&session, |snapshot| first_text_row(snapshot).contains("__"));
    thread::sleep(Duration::from_secs(5));
    assert!(
        !session.needs_close_confirmation().unwrap(),
        "idle Nushell shim was reported as a running job"
    );
}

#[test]
fn configured_nushell_shim_still_reports_a_running_job() {
    if !std::process::Command::new("nu")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("Nushell unavailable; skipping shim close-confirmation job regression");
        return;
    }
    let config = Config {
        shell: Some(std::path::PathBuf::from("nu")),
        ..Config::default()
    };
    let session = Session::spawn(config, 80, 24).unwrap();
    wait_snapshot(&session, |snapshot| first_text_row(snapshot).contains("__"));
    session.send(Command::Input(b"ping.exe -n 5 127.0.0.1\r".to_vec()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !session.needs_close_confirmation().unwrap() {
        assert!(Instant::now() < deadline, "running job was not detected");
        thread::sleep(Duration::from_millis(10));
    }
}
