use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use libghostty_vt::{Terminal, TerminalOptions};
use portable_pty::CommandBuilder;
use wisp::config::Config;
use wisp::terminal::{
    Command, Session, SessionEvent, SnapshotBuilder, TerminalSnapshot, apply_theme,
};

const COLUMNS: u16 = 80;
const ROWS: u16 = 24;
const FINAL_MARKER: &str = "WISP_THROUGHPUT_DONE";
const READY_TITLE: &str = "wisp-throughput-ready";
const DONE_TITLE: &str = "wisp-throughput-done";
const PROBE: &str = r#"
import os
import sys
import termios
import tty

original = termios.tcgetattr(0)
tty.setraw(0)
try:
    with open(sys.argv[1], 'rb') as source:
        payload = source.read()
    os.write(1, b'\x1b]2;wisp-throughput-ready\x1b\\')
    if os.read(0, 1) != b's':
        raise RuntimeError('invalid start handshake')
    remaining = memoryview(payload)
    while remaining:
        written = os.write(1, remaining[:65536])
        remaining = remaining[written:]
    while os.read(0, 1) != b'\x04':
        pass
finally:
    termios.tcsetattr(0, termios.TCSANOW, original)
"#;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mebibytes = std::env::args()
        .nth(1)
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(16);
    if !(1..=256).contains(&mebibytes) {
        return Err("payload size must be between 1 and 256 MiB".into());
    }
    let payload = make_payload(mebibytes * 1024 * 1024);
    let config = Config {
        scrollback_limit: 0,
        ..Default::default()
    };

    let mut terminal = Terminal::new(TerminalOptions {
        cols: COLUMNS,
        rows: ROWS,
        max_scrollback: 0,
    })?;
    apply_theme(&mut terminal, &config)?;
    let mut snapshots = SnapshotBuilder::new()?;
    let started = Instant::now();
    for chunk in payload.chunks(16 * 1024) {
        terminal.vt_write(chunk);
    }
    let baseline_elapsed = started.elapsed();
    let snapshot = snapshots.update(&terminal, &config)?;
    if !has_final_marker(&snapshot) {
        return Err("direct Ghostty parse did not reach the final screen marker".into());
    }

    let payload_file = PayloadFile::new(&payload)?;
    let mut command = CommandBuilder::new("python3");
    command.arg("-c");
    command.arg(PROBE);
    command.arg(&payload_file.0);
    let session = Session::spawn_command(config, COLUMNS, ROWS, command)?;
    session.send(Command::Resize {
        cols: COLUMNS,
        rows: ROWS,
        cell_width: 8,
        cell_height: 16,
        pixel_width: u32::from(COLUMNS) * 8,
        pixel_height: u32::from(ROWS) * 16,
    });
    wait_until_ready(&session)?;
    while session.try_snapshot().is_some() {}
    let started = Instant::now();
    session.send(Command::Input(b"s".to_vec()));
    let mut title_complete = false;
    let mut snapshot_complete = false;
    let mut consumed_snapshots = 0_usize;
    let deadline = started + Duration::from_secs(30);
    while !(title_complete && snapshot_complete) {
        if Instant::now() >= deadline {
            return Err("PTY benchmark timed out before its final snapshot".into());
        }
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Title(title) if title == DONE_TITLE => title_complete = true,
                SessionEvent::Error(error) => return Err(error.into()),
                SessionEvent::Exited => {
                    return Err("PTY exited before completing the benchmark".into());
                }
                _ => {}
            }
        }
        if let Some(snapshot) = session.try_snapshot() {
            consumed_snapshots += 1;
            snapshot_complete |= has_final_marker(&snapshot);
        }
        if !(title_complete && snapshot_complete) {
            thread::sleep(Duration::from_millis(1));
        }
    }
    let session_elapsed = started.elapsed();
    session.send(Command::Input(vec![4]));
    let size = payload.len() as f64 / (1024.0 * 1024.0);
    println!(
        "Payload: {} bytes ({size:.3} MiB), 80 columns, 24 rows, scrollback disabled",
        payload.len()
    );
    println!(
        "Ghostty VT parse: {:.3} s, {:.1} MiB/s",
        baseline_elapsed.as_secs_f64(),
        size / baseline_elapsed.as_secs_f64()
    );
    println!(
        "Wisp PTY + VT + snapshots: {:.3} s, {:.1} MiB/s, {consumed_snapshots} snapshots consumed",
        session_elapsed.as_secs_f64(),
        size / session_elapsed.as_secs_f64()
    );
    println!("Both paths verified the final rendered-grid marker; GPU rendering is not included.");
    Ok(())
}

fn make_payload(target_bytes: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(target_bytes + 128);
    let text = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghi";
    for line in 0.. {
        let prefix = format!("\x1b[38;5;{}m{line:08x}:", line % 16);
        if payload.len() + prefix.len() + text.len() + 2 > target_bytes {
            break;
        }
        payload.extend_from_slice(prefix.as_bytes());
        payload.extend_from_slice(text);
        payload.extend_from_slice(b"\r\n");
    }
    payload.extend_from_slice(
        format!("\x1b[0m\r\n{FINAL_MARKER}\r\n\x1b]2;{DONE_TITLE}\x1b\\").as_bytes(),
    );
    payload
}

fn has_final_marker(snapshot: &TerminalSnapshot) -> bool {
    snapshot
        .cells
        .chunks(usize::from(snapshot.cols))
        .any(|row| {
            let text = row
                .iter()
                .map(|cell| cell.text.as_str())
                .collect::<String>();
            text.contains(FINAL_MARKER)
        })
}

fn wait_until_ready(session: &Session) -> Result<(), Box<dyn Error + Send + Sync>> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Title(title) if title == READY_TITLE => return Ok(()),
                SessionEvent::Error(error) => return Err(error.into()),
                SessionEvent::Exited => {
                    return Err("PTY exited during the benchmark handshake".into());
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(1));
    }
    Err("PTY benchmark timed out during its handshake".into())
}

struct PayloadFile(PathBuf);

impl PayloadFile {
    fn new(bytes: &[u8]) -> Result<Self, std::io::Error> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("wisp-throughput-{}-{nonce}.vt", std::process::id()));
        fs::write(&path, bytes)?;
        Ok(Self(path))
    }
}

impl Drop for PayloadFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
