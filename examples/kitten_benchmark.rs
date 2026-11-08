use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::CommandBuilder;
use wisp::terminal::{Command, Session, SessionEvent};

const DONE_TITLE: &str = "wisp-kitten-benchmark-done";
const PROBE: &str = r#"
import os
import subprocess
import sys
import tty

with open(sys.argv[1], 'wb') as result:
    status = subprocess.run(['kitten', '__benchmark__', *sys.argv[2:]], stdout=result, stderr=result).returncode
    result.write(f'\nExit status: {status}\n'.encode())
tty.setraw(0)
os.write(1, b'\x1b]2;wisp-kitten-benchmark-done\x1b\\')
while os.read(0, 1) != b'\x04':
    pass
"#;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.is_empty() {
        arguments.extend(
            [
                "--repetitions",
                "20",
                "unique_unicode",
                "csi",
                "long_escape_codes",
            ]
            .map(str::to_owned),
        );
    }
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let output = OutputFile(std::env::temp_dir().join(format!(
        "wisp-kitten-benchmark-{}-{nonce}.txt",
        std::process::id()
    )));
    let mut command = CommandBuilder::new("python3");
    command.args(["-u", "-c", PROBE]);
    command.arg(&output.0);
    command.args(&arguments);
    let (sender, receiver) = mpsc::channel();
    let config = wisp::config::load()?;
    let scrollback = config.scrollback_limit;
    let session = Session::spawn_command_with_waker(
        config,
        160,
        48,
        command,
        Arc::new(move || {
            let _ = sender.send(());
        }),
    )?;
    let mut snapshots = 0;
    let started = Instant::now();
    loop {
        while session.try_snapshot().is_some() {
            snapshots += 1;
        }
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Title(title) if title == DONE_TITLE => {
                    let report = fs::read_to_string(&output.0)?;
                    if !report.ends_with("Exit status: 0\n") {
                        return Err(report.into());
                    }
                    println!(
                        "Installed kitten, Wisp production PTY/VT path, 160x48, scrollback={scrollback}"
                    );
                    let plain = report
                        .replace("\x1b]\x1b\\\x1bc", "")
                        .replace("\x1b[31m", "")
                        .replace("\x1b[32m", "")
                        .replace("\x1b[m", "");
                    println!("{plain}");
                    println!("Snapshots consumed: {snapshots}; GPU rendering is excluded.");
                    session.send(Command::Input(vec![4]));
                    return Ok(());
                }
                SessionEvent::Error(error) => return Err(error.into()),
                SessionEvent::Exited => return Err("kitten exited before completing".into()),
                _ => {}
            }
        }
        if started.elapsed() > Duration::from_secs(300) {
            return Err("kitten benchmark timed out".into());
        }
        let _ = receiver.recv_timeout(Duration::from_millis(100));
    }
}

struct OutputFile(PathBuf);

impl Drop for OutputFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
