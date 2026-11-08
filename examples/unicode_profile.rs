use std::error::Error;
use std::hint::black_box;
use std::time::{Duration, Instant};

use libghostty_vt::{Terminal, TerminalOptions};
use wisp::config::Config;
use wisp::terminal::{SnapshotBuilder, apply_theme};

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let repetitions = arguments
        .first()
        .map_or(Ok(20), |value| value.parse::<usize>())?;
    let columns = arguments
        .get(1)
        .map_or(Ok(160), |value| value.parse::<u16>())?;
    let rows = arguments
        .get(2)
        .map_or(Ok(48), |value| value.parse::<u16>())?;
    if repetitions == 0 || columns == 0 || rows == 0 {
        return Err("repetitions, columns and rows must be positive".into());
    }
    let mut config = Config::default();
    config.scrollback_limit = 10_000;
    let unique = unique_combining_payload();
    let repeated = "a\u{300}\u{300}\u{300}".repeat(256 * 1024).into_bytes();
    let ascii = vec![b'a'; unique.len()];
    println!(
        "{columns} columns, {rows} rows, alternate screen, {repetitions} repetitions, 16 KiB chunks"
    );
    for (description, payload) in [
        ("ASCII", ascii),
        ("Repeated combining cell", repeated),
        ("Kitty unique_unicode", unique),
    ] {
        for capture in [false, true] {
            let mut terminal = Terminal::new(TerminalOptions {
                cols: columns,
                rows,
                max_scrollback: 10_000,
            })?;
            apply_theme(&mut terminal, &config)?;
            terminal.vt_write(b"\x1b[?1049h\x1b[?2026h");
            let mut builder = SnapshotBuilder::new()?;
            let mut snapshot_count = 0;
            let mut snapshot_elapsed = Duration::ZERO;
            let mut last_snapshot = Instant::now();
            let started = Instant::now();
            for _ in 0..repetitions {
                for chunk in payload.chunks(16 * 1024) {
                    terminal.vt_write(chunk);
                    if capture && last_snapshot.elapsed() >= Duration::from_millis(8) {
                        let capture_started = Instant::now();
                        black_box(builder.update(&terminal, &config)?);
                        snapshot_elapsed += capture_started.elapsed();
                        snapshot_count += 1;
                        last_snapshot = Instant::now();
                    }
                }
                terminal.vt_write(b"\x1b[m\x1b[H\x1b[2J\x1b[?2026l\x1b[?2026h");
            }
            let elapsed = started.elapsed();
            let rate = (payload.len() * repetitions) as f64 / 1_048_576.0 / elapsed.as_secs_f64();
            let mode = if capture { "with snapshots" } else { "VT only" };
            println!(
                "{description:24} {mode:14}: {:8.3} s, {rate:8.2} MiB/s, {snapshot_count:5} snapshots, {:8.3} ms in snapshot capture",
                elapsed.as_secs_f64(),
                snapshot_elapsed.as_secs_f64() * 1000.0
            );
        }
    }
    Ok(())
}

fn unique_combining_payload() -> Vec<u8> {
    let mut payload = String::with_capacity(256 * 1024 * 7);
    for index in 0..256 * 1024 {
        let mut digits = index;
        payload.push('a');
        for _ in 0..3 {
            payload.push(char::from_u32(0x300 + digits % 0x70).expect("combining codepoint"));
            digits /= 0x70;
        }
    }
    payload.into_bytes()
}
