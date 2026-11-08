use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use portable_pty::CommandBuilder;
use wisp::config::Config;
use wisp::terminal::{Command, Session, SessionEvent};

const PROBE: &str = r#"
import base64
import os
import select
import sys
import tty

tty.setraw(0)
os.write(1, b'\x1b]2;effects-ready\x1b\\')
for index, encoded in enumerate(sys.argv[1:]):
    assert os.read(0, 1) == b'g'
    os.write(1, base64.b64decode(encoded) + b'\x1b[5n')
    reply = b''
    while not reply.endswith(b'\x1b[0n'):
        if not select.select([0], [], [], 5)[0]: raise RuntimeError('DSR timed out')
        reply += os.read(0, 65536)
    title = 'effects-stage:' + str(index) + ':' + reply[:-4].hex()
    os.write(1, ('\x1b]2;' + title + '\x1b\\').encode())
while os.read(0, 1) != b'\x04': pass
"#;

#[derive(Default, Debug)]
struct Effects {
    clipboard: Vec<String>,
    primary: Vec<String>,
    titles: Vec<String>,
    bells: usize,
    reply: Vec<u8>,
}

struct Probe {
    session: Session,
    stage: usize,
}

impl Probe {
    fn new(config: Config, payloads: &[&[u8]]) -> Self {
        let mut command = CommandBuilder::new("python3");
        command.args(["-u", "-c", PROBE]);
        for payload in payloads {
            command.arg(STANDARD.encode(payload));
        }
        let session = Session::spawn_command(config, 80, 24, command).unwrap();
        let started = Instant::now();
        loop {
            if let Some(event) = session.try_event() {
                match event {
                    SessionEvent::Title(title) if title == "effects-ready" => break,
                    SessionEvent::Error(error) => panic!("{error}"),
                    SessionEvent::Exited => panic!("effects child exited before ready"),
                    _ => {}
                }
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(1));
        }
        Self { session, stage: 0 }
    }

    fn step(&mut self) -> Effects {
        let prefix = format!("effects-stage:{}:", self.stage);
        self.stage += 1;
        self.session.send(Command::Input(b"g".to_vec()));
        let started = Instant::now();
        let mut effects = Effects::default();
        loop {
            while let Some(event) = self.session.try_event() {
                match event {
                    SessionEvent::Title(title) if title.starts_with(&prefix) => {
                        effects.reply = title
                            .strip_prefix(&prefix)
                            .unwrap()
                            .as_bytes()
                            .chunks_exact(2)
                            .map(|pair| {
                                u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()
                            })
                            .collect();
                        return effects;
                    }
                    SessionEvent::Clipboard(text) => effects.clipboard.push(text),
                    SessionEvent::PrimarySelection(text) => effects.primary.push(text),
                    SessionEvent::Bell => effects.bells += 1,
                    SessionEvent::Title(title) => effects.titles.push(title),
                    SessionEvent::Error(error) => panic!("{error}"),
                    SessionEvent::Exited => panic!("effects child exited before {prefix}"),
                    _ => {}
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "timed out waiting for {prefix}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.session.send(Command::Input(vec![4]));
    }
}

#[test]
fn osc52_and_iterm_copy_writes_route_text_and_clear_to_the_correct_clipboard() {
    let mut probe = Probe::new(
        Config::default(),
        &[
            b"\x1b]52;c;aGVsbG8=\x07\x1b]52;p;cHJpbWFyeQ==\x1b\\",
            b"\x1b]52;c;\x1b\\\x1b]52;p;\x07",
            b"\x1b]1337;Copy=:aVRlcm0y\x1b\\",
        ],
    );
    let writes = probe.step();
    assert_eq!(writes.clipboard, ["hello"]);
    assert_eq!(writes.primary, ["primary"]);
    let clear = probe.step();
    assert_eq!(clear.clipboard, [""]);
    assert_eq!(clear.primary, [""]);
    let iterm = probe.step();
    assert_eq!(iterm.clipboard, ["iTerm2"]);
    assert!(iterm.primary.is_empty());
}

#[test]
fn clipboard_policy_denies_writes_and_queries_never_expose_host_contents() {
    let mut config = Config::default();
    config.protocols.clipboard_write = false;
    let mut probe = Probe::new(
        config,
        &[b"\x1b]52;c;aGVsbG8=\x07\x1b]52;p;\x1b\\\x1b]1337;Copy=:aVRlcm0y\x1b\\\x1b]52;c;?\x07"],
    );
    let denied = probe.step();
    assert!(denied.clipboard.is_empty());
    assert!(denied.primary.is_empty());
    assert!(denied.reply.is_empty());
    let mut probe = Probe::new(Config::default(), &[b"\x1b]52;c;?\x07\x1b]52;p;?\x1b\\"]);
    let query = probe.step();
    assert!(query.clipboard.is_empty());
    assert!(query.primary.is_empty());
    assert!(query.reply.is_empty());
}

#[test]
fn xtversion_and_color_scheme_queries_report_wisp_and_current_background() {
    let mut probe = Probe::new(
        Config::default(),
        &[b"\x1b[>0q\x1b[?996n", b"\x1b]11;#ffffff\x1b\\\x1b[?996n"],
    );
    let dark = probe.step();
    assert_eq!(
        dark.reply,
        format!(
            "\x1bP>|wisp {}\x1b\\\x1b[?997;1n",
            env!("CARGO_PKG_VERSION")
        )
        .as_bytes()
    );
    assert_eq!(probe.step().reply, b"\x1b[?997;2n");
}

#[test]
fn bell_bursts_are_coalesced_and_title_updates_still_reach_the_window() {
    let mut payload = vec![7; 64];
    payload.extend_from_slice(b"\x1b]2;application-title\x1b\\");
    let mut probe = Probe::new(Config::default(), &[&payload, b"\x07"]);
    let burst = probe.step();
    assert_eq!(burst.bells, 1);
    assert_eq!(burst.titles, ["application-title"]);
    thread::sleep(Duration::from_millis(120));
    assert_eq!(probe.step().bells, 1);
}
