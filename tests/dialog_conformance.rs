use std::thread;
use std::time::{Duration, Instant};

use portable_pty::CommandBuilder;
use wisp::close_confirmation::{CloseDialog, DialogKind};
use wisp::config::Config;
use wisp::terminal::{Command, Session, SessionEvent, TerminalSnapshot};

fn reference(dialog: &CloseDialog, columns: u16, rows: u16) -> TerminalSnapshot {
    let mut command = CommandBuilder::new("kitten");
    let message = if dialog.kind == DialogKind::Paste {
        dialog.message.replace(
            "code execution vulnerabilities",
            "\x1b[31mcode execution vulnerabilities\x1b[39m",
        )
    } else {
        dialog
            .message
            .replace("running: sleep.", "running: \x1b[32msleep\x1b[39m.")
    };
    command.args([
        "ask",
        "--type",
        if dialog.kind == DialogKind::Close {
            "yesno"
        } else {
            "choices"
        },
        "--message",
        &message,
    ]);
    if dialog.kind == DialogKind::Paste {
        command.args([
            "--choice",
            "s;green:Sanitize and paste",
            "--choice",
            "a;red:Paste anyway",
            "--choice",
            "c;yellow:Cancel",
            "--default",
            "s",
        ]);
    }
    let session = Session::spawn_command(Config::default(), columns, rows, command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = session.try_snapshot()
            && frame
                .cells
                .iter()
                .any(|cell| cell.text == "╭" || cell.text == "Y" || cell.text == "S")
        {
            session.send(Command::Input(b"\x1b".to_vec()));
            return frame;
        }
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Error(error) => panic!("kitten ask failed: {error}"),
                SessionEvent::Exited => panic!("kitten ask exited before rendering"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "kitten ask did not render");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn dialog_cells_match_actual_kitty_ask_layout_colors_and_rendition() {
    for (columns, rows) in [(80, 24), (40, 12), (100, 30)] {
        for dialog in [
            CloseDialog::new(
                "Are you sure you want to close this window? It is running: sleep.".to_owned(),
            ),
            CloseDialog::paste(),
        ] {
            let actual = reference(&dialog, columns, rows);
            let expected = dialog.layout(columns, rows, &Config::default()).frame;
            for (index, (actual, expected)) in actual.cells.iter().zip(&expected.cells).enumerate()
            {
                let normalize = |text: &str| {
                    if text.is_empty() {
                        " ".to_owned()
                    } else {
                        text.to_owned()
                    }
                };
                assert_eq!(
                    normalize(&actual.text),
                    normalize(&expected.text),
                    "{:?} {columns}x{rows} cell {},{}",
                    dialog.kind,
                    index % usize::from(columns),
                    index / usize::from(columns)
                );
                if !actual.text.trim().is_empty() {
                    assert_eq!(
                        actual.foreground, expected.foreground,
                        "{:?} cell{index} {:?}",
                        dialog.kind, actual.text
                    );
                    assert_eq!(
                        actual.bold, expected.bold,
                        "{:?} cell{index} {:?}",
                        dialog.kind, actual.text
                    );
                    assert_eq!(actual.underline_style, expected.underline_style);
                }
            }
        }
    }
}
