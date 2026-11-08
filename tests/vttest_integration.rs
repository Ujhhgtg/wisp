use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::CommandBuilder;
use wisp::config::Config;
use wisp::terminal::{Command, Session, SessionEvent, TerminalSnapshot};

fn text(frame: &TerminalSnapshot, row: usize) -> String {
    frame.cells[row * usize::from(frame.cols)..(row + 1) * usize::from(frame.cols)]
        .iter()
        .filter(|cell| cell.width != 0)
        .map(|cell| {
            if cell.text.is_empty() {
                " "
            } else {
                cell.text.as_str()
            }
        })
        .collect()
}

fn wait_frame(
    session: &Session,
    predicate: impl Fn(&TerminalSnapshot) -> bool,
) -> TerminalSnapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(frame) = session.try_snapshot()
            && predicate(&frame)
        {
            return frame;
        }
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Error(error) => panic!("vttest session failed: {error}"),
                SessionEvent::Exited => {
                    panic!("vttest exited before completing the selected screens")
                }
                _ => {}
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the selected vttest screen"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

fn contains(frame: &TerminalSnapshot, needle: &str) -> bool {
    (0..usize::from(frame.rows)).any(|row| text(frame, row).contains(needle))
}

fn assert_movement_border(frame: &TerminalSnapshot, columns: u16) {
    assert_eq!((frame.cols, frame.rows), (columns, 24));
    let width = usize::from(columns);
    let inner_left = (width - 60) / 2;
    assert_eq!(text(frame, 0), "*".repeat(width));
    assert_eq!(text(frame, 23), "*".repeat(width));
    for row in 1..23 {
        let line = text(frame, row);
        assert_eq!(&line[..2], "*+", "left border row {row}");
        assert_eq!(&line[width - 2..], "+*", "right border row {row}");
    }
    for row in [1, 22] {
        assert_eq!(text(frame, row), format!("*{}*", "+".repeat(width - 2)));
    }
    for row in [8, 15] {
        assert_eq!(
            &text(frame, row)[inner_left..inner_left + 60],
            "E".repeat(60)
        );
    }
    for row in 9..15 {
        let line = text(frame, row);
        assert_eq!(&line[inner_left..inner_left + 1], "E");
        assert_eq!(&line[inner_left + 59..inner_left + 60], "E");
    }
}

#[test]
#[ignore = "requires WISP_VTTEST pointing to an external vttest executable"]
fn external_vttest_selected_cursor_and_report_screens_match_expected_output() {
    let binary = PathBuf::from(
        std::env::var_os("WISP_VTTEST").expect("set WISP_VTTEST to the vttest binary"),
    );
    assert!(
        binary.is_file(),
        "vttest binary does not exist: {}",
        binary.display()
    );
    let mut command = CommandBuilder::new(binary);
    command.arg("24x80.132");
    let session = Session::spawn_command(Config::default(), 80, 24, command).unwrap();
    wait_frame(&session, |frame| {
        contains(frame, "Test of cursor movements")
    });
    session.send(Command::Input(b"1\r".to_vec()));
    for columns in [80, 132] {
        let frame = wait_frame(&session, |frame| {
            frame.cols == columns
                && contains(frame, "frame of E's")
                && contains(frame, "Push <RETURN>")
        });
        assert_movement_border(&frame, columns);
        session.send(Command::Input(b"\r".to_vec()));
    }
    for _ in 0..2 {
        wait_frame(&session, |frame| {
            contains(frame, "The left/right margins") && contains(frame, "Push <RETURN>")
        });
        session.send(Command::Input(b"\r".to_vec()));
    }
    let frame = wait_frame(&session, |frame| {
        contains(frame, "four identical lines") && contains(frame, "Push <RETURN>")
    });
    for row in 3..7 {
        assert_eq!(
            text(&frame, row).trim_end(),
            "A B C D E F G H I",
            "embedded-control row {row}"
        );
    }
    session.send(Command::Input(b"\r".to_vec()));
    let frame = wait_frame(&session, |frame| {
        contains(frame, "Test of leading zeros") && contains(frame, "Push <RETURN>")
    });
    assert_eq!(text(&frame, 3).trim_end(), "This is a correct sentence");
    session.send(Command::Input(b"\r".to_vec()));
    wait_frame(&session, |frame| {
        contains(frame, "Test of cursor movements")
    });
    session.send(Command::Input(b"6\r".to_vec()));
    wait_frame(&session, |frame| {
        contains(frame, "Terminal Reports/Responses")
    });
    session.send(Command::Input(b"3\r".to_vec()));
    let report = wait_frame(&session, |frame| {
        contains(frame, "Test of Device Status Report 5") && contains(frame, "Push <RETURN>")
    });
    assert!(contains(&report, "TERMINAL OK"));
    assert!(contains(&report, " -- OK"));
    assert!(!contains(&report, "Unknown response"));
    assert!(!contains(&report, "Ignores origin mode"));
    session.send(Command::Input(b"\r".to_vec()));
    wait_frame(&session, |frame| {
        contains(frame, "Terminal Reports/Responses")
    });
    session.send(Command::Input(b"5\r".to_vec()));
    let report = wait_frame(&session, |frame| {
        contains(frame, "Testing Secondary Device Attributes") && contains(frame, "Push <RETURN>")
    });
    assert!(contains(&report, "Pp="));
    assert!(contains(&report, "Pv="));
    assert!(contains(&report, "Pc="));
    assert!(contains(&report, "ok"));
    session.send(Command::Input(b"\r".to_vec()));
    wait_frame(&session, |frame| {
        contains(frame, "Terminal Reports/Responses")
    });
    session.send(Command::Input(b"0\r".to_vec()));
    wait_frame(&session, |frame| {
        contains(frame, "Test of cursor movements")
    });
    session.send(Command::Input(b"0\r".to_vec()));
}
