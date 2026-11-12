#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use wisp::close_confirmation::{needs_confirmation, shell_was_replaced};

struct Shell {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output: Receiver<Vec<u8>>,
    _master: Box<dyn MasterPty + Send>,
}

impl Shell {
    fn new(program: &str) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(program);
        if program == "fish" {
            command.args([
                "--no-config",
                "--interactive",
                "--init-command",
                "set -g fish_history ''; function fish_prompt; printf 'wisp-test-ready>'; end",
            ]);
        } else {
            command.arg("30");
        }
        command.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        let response_writer = writer.clone();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let (sender, output) = mpsc::channel();
        thread::spawn(move || {
            let mut terminal = libghostty_vt::Terminal::new(libghostty_vt::TerminalOptions {
                cols: 80,
                rows: 24,
                max_scrollback: 0,
            })
            .unwrap();
            terminal
                .on_pty_write(move |_, bytes| {
                    let _ = response_writer.lock().unwrap().write_all(bytes);
                })
                .unwrap();
            let mut buffer = [0; 4096];
            while let Ok(length) = reader.read(&mut buffer) {
                terminal.vt_write(&buffer[..length]);
                if length == 0 || sender.send(buffer[..length].to_vec()).is_err() {
                    break;
                }
            }
        });
        let shell = Self {
            child,
            writer,
            output,
            _master: pair.master,
        };
        if program == "fish" {
            shell.prompt();
        }
        shell
    }

    fn prompt(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = Vec::new();
        loop {
            let bytes = self
                .output
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|error| {
                    panic!(
                        "prompt error {error}: {:?}",
                        String::from_utf8_lossy(&output)
                    )
                });
            output.extend(bytes);
            if output
                .windows(b"wisp-test-ready>".len())
                .any(|window| window == b"wisp-test-ready>")
            {
                return;
            }
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    }

    fn busy(&self) -> bool {
        needs_confirmation(self.child.process_id(), false).unwrap()
    }

    fn wait_busy(&self, expected: bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.busy() != expected {
            assert!(Instant::now() < deadline, "expected busy={expected}");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn actual_fish_prompt_foreground_background_and_stopped_jobs_are_distinguished() {
    let mut shell = Shell::new("fish");
    shell.wait_busy(false);
    shell.send(b"sleep 30\n");
    shell.wait_busy(true);
    shell.send(b"\x03");
    shell.prompt();
    shell.wait_busy(false);
    shell.send(b"sleep 30 &\n");
    shell.prompt();
    shell.wait_busy(true);
    shell.send(b"kill $last_pid; wait\n");
    shell.prompt();
    shell.wait_busy(false);
    shell.send(b"sleep 30\n");
    shell.wait_busy(true);
    shell.send(b"\x1a");
    shell.prompt();
    shell.wait_busy(true);
    shell.send(b"kill -KILL (jobs -p); wait\n");
    shell.prompt();
    shell.wait_busy(false);
}

#[test]
fn explicit_command_needs_confirmation_until_it_exits() {
    let mut command = Shell::new("sleep");
    assert!(needs_confirmation(command.child.process_id(), true).unwrap());
    command.child.kill().unwrap();
    command.child.wait().unwrap();
    assert!(!needs_confirmation(command.child.process_id(), true).unwrap());
}

#[test]
fn shell_exec_replacement_is_detected_even_without_descendants() {
    let mut shell = Shell::new("fish");
    let pid = shell.child.process_id();
    let executable = std::fs::read_link(format!("/proc/{}/exe", pid.unwrap())).unwrap();
    assert!(!shell_was_replaced(pid, &executable).unwrap());
    shell.send(b"exec sleep 30\n");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !shell_was_replaced(pid, &executable).unwrap() {
        assert!(Instant::now() < deadline, "shell did not exec sleep");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!shell.busy());
}
