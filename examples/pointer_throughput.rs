use std::hint::black_box;
use std::time::Instant;

use libghostty_vt::{Terminal, TerminalOptions};
use wisp::config::Config;
use wisp::input::InputState;
use wisp::links::PointerShapes;

fn payloads() -> Vec<(&'static str, Vec<u8>)> {
    let mut unicode = String::new();
    for codepoint in 0x300..0x1f00 {
        if let Some(character) = char::from_u32(codepoint) {
            unicode.push('a');
            unicode.push(character);
        }
    }
    let mut long_osc = b"\x1b]0;".to_vec();
    long_osc.resize(256 * 1024, b'a');
    long_osc.extend_from_slice(b"\x1b\\");
    vec![
        ("ascii", b"hello world 1234567890\r\n".repeat(512)),
        ("unique-unicode", unicode.into_bytes()),
        (
            "csi",
            b"\x1b[38;2;128;96;64m\x1b[48;2;40;44;52mtest\x1b[0m".repeat(512),
        ),
        ("long-osc", long_osc),
        (
            "sync-alt-csi",
            b"\x1b[?2026h\x1b[?1049h\x1b[?1049l\x1b[?2026l".repeat(512),
        ),
    ]
}

fn measure(name: &str, bytes: &[u8], repetitions: usize, mut process: impl FnMut(&[u8])) {
    for _ in 0..3 {
        for chunk in bytes.chunks(16 * 1024) {
            process(chunk);
        }
    }
    let started = Instant::now();
    for _ in 0..repetitions {
        for chunk in bytes.chunks(16 * 1024) {
            process(chunk);
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    println!(
        "{name}: {:.1} MB/s",
        (bytes.len() * repetitions) as f64 / 1_000_000.0 / seconds
    );
}

fn main() {
    let megabytes = std::env::args()
        .nth(1)
        .map(|value| value.parse::<usize>().expect("expected megabytes"))
        .unwrap_or(64)
        .max(1);
    let config = Config::default();
    for (name, bytes) in payloads() {
        let repetitions = (megabytes * 1_000_000 / bytes.len()).max(1);
        let mut shapes = PointerShapes::default();
        measure(&format!("pointer/{name}"), &bytes, repetitions, |chunk| {
            black_box(shapes.observe(black_box(chunk)));
        });
        let terminal = Terminal::new(TerminalOptions {
            cols: 160,
            rows: 40,
            max_scrollback: 0,
        })
        .expect("terminal should initialize");
        let mut input = InputState::new().expect("input should initialize");
        measure(&format!("input/{name}"), &bytes, repetitions, |chunk| {
            black_box(
                input
                    .observe_output(&terminal, &config, black_box(chunk))
                    .expect("output observer"),
            );
        });
        let mut terminal = Terminal::new(TerminalOptions {
            cols: 160,
            rows: 40,
            max_scrollback: 0,
        })
        .expect("terminal should initialize");
        measure(
            &format!("vt/{name}"),
            &bytes,
            repetitions.min(100),
            |chunk| {
                terminal.vt_write(black_box(chunk));
            },
        );
    }
}
