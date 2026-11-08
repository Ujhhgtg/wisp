#![cfg(not(test))]

use std::error::Error;
use std::hint::black_box;
use std::time::Instant;

use wisp::{config, terminal};

#[path = "../src/text_layout.rs"]
mod text_layout;

use config::Config;
use terminal::{CellSnapshot, CursorSnapshot, Rgb, TerminalSnapshot};
use text_layout::TextLayout;

const COLUMNS: u16 = 160;
const ROWS: u16 = 48;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let frames = std::env::args()
        .nth(1)
        .map(|argument| argument.parse::<u32>())
        .transpose()?
        .unwrap_or(300);
    if !(1..=10_000).contains(&frames) {
        return Err("frame count must be between 1 and 10000".into());
    }
    let config = config::load()?;
    let mut layout = TextLayout::new(&config, 1.0)?;
    layout.set_blink_visible(true);
    black_box(&layout.empty_buffer);
    println!(
        "Production TextLayout, {COLUMNS}x{ROWS}, {} at {}pt, {frames} frames/workload",
        layout.family, config.font.size
    );
    println!("Includes snapshot mutations and CPU shaping; excludes PTY, GPU, and presentation.");
    for workload in [
        "cursor-only",
        "background-only",
        "uniform-text",
        "many-colors",
        "one-row",
    ] {
        let mut frame = snapshot(&config);
        layout.shape(&frame)?;
        let started = Instant::now();
        let mut shaped_rows = 0;
        for iteration in 0..frames {
            let alternate = iteration.is_multiple_of(2);
            let color = if alternate {
                Rgb {
                    red: 255,
                    green: 0,
                    blue: 0,
                }
            } else {
                Rgb {
                    red: 0,
                    green: 255,
                    blue: 0,
                }
            };
            match workload {
                "cursor-only" => frame.cursor.column = (iteration % u32::from(COLUMNS)) as u16,
                "background-only" => {
                    for cell in &mut frame.cells {
                        cell.background = Some(color);
                    }
                }
                "uniform-text" | "many-colors" => {
                    for (index, cell) in frame.cells.iter_mut().enumerate() {
                        cell.text = if alternate { "x" } else { "y" }.to_owned();
                        cell.foreground = if workload == "uniform-text" || index.is_multiple_of(2) {
                            color
                        } else {
                            frame.foreground
                        };
                    }
                }
                "one-row" => frame.cells[0].text = if alternate { "x" } else { "y" }.to_owned(),
                _ => unreachable!(),
            }
            layout.shape(black_box(&frame))?;
            shaped_rows += layout.shaped_rows;
            black_box(&layout.glyphs);
        }
        println!(
            "{workload:>15}: {:8.1} us/frame; {:.2} shaped rows/frame",
            started.elapsed().as_secs_f64() * 1_000_000.0 / f64::from(frames),
            shaped_rows as f64 / f64::from(frames)
        );
    }
    Ok(())
}

fn snapshot(config: &Config) -> TerminalSnapshot {
    let pattern = "let some_value = other => function != test; ~> -> == ";
    TerminalSnapshot {
        cols: COLUMNS,
        rows: ROWS,
        cells: pattern
            .chars()
            .cycle()
            .take(usize::from(COLUMNS) * usize::from(ROWS))
            .map(|character| CellSnapshot {
                text: character.to_string(),
                foreground: config.colors.foreground.into(),
                ..Default::default()
            })
            .collect(),
        background: config.colors.background.into(),
        foreground: config.colors.foreground.into(),
        cursor_color: config.colors.cursor.into(),
        cursor: CursorSnapshot::default(),
        mouse_reporting: false,
        graphics: Default::default(),
    }
}
