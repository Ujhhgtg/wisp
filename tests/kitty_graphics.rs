use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use libghostty_vt::{Terminal, TerminalOptions};
use portable_pty::CommandBuilder;
use wisp::config::Config;
use wisp::graphics::ImageLayer;
use wisp::terminal::{Command, Session, SessionEvent, SnapshotBuilder, apply_theme};

fn terminal() -> (Terminal<'static, 'static>, Config, SnapshotBuilder) {
    let config = Config::default();
    let mut terminal = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 1024 * 1024,
    })
    .unwrap();
    terminal.resize(80, 24, 8, 16).unwrap();
    apply_theme(&mut terminal, &config).unwrap();
    (terminal, config, SnapshotBuilder::new().unwrap())
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(ALPHABET[((value >> 18) & 63) as usize] as char);
        encoded.push(ALPHABET[((value >> 12) & 63) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    encoded
}

fn png_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_image_data(&[
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 128,
            ])
            .unwrap();
    }
    bytes
}

#[test]
fn direct_rgba_pixels_placements_and_unchanged_arcs_are_preserved() {
    let (mut terminal, config, mut snapshots) = terminal();
    terminal.vt_write(
        b"\x1b[3;4H\x1b_Ga=T,f=32,s=2,v=1,i=17,p=9,c=2,r=1,X=3,Y=4,z=-1;/wAA/wD/AIA=\x1b\\",
    );
    let first = snapshots.update(&terminal, &config).unwrap().graphics;
    assert_eq!(first.placements.len(), 1);
    let placement = &first.placements[0];
    assert_eq!(placement.image_id, 17);
    assert_eq!(placement.placement_id, 9);
    assert_eq!(placement.z, -1);
    assert_eq!(ImageLayer::from_z(placement.z), ImageLayer::BelowText);
    assert_eq!(placement.grid_position, [3, 2]);
    assert_eq!(placement.offset, [3, 4]);
    assert_eq!(
        placement.image.rgba.as_ref(),
        &[255, 0, 0, 255, 0, 255, 0, 128]
    );
    let unchanged = snapshots.update(&terminal, &config).unwrap().graphics;
    assert!(Arc::ptr_eq(&first.placements, &unchanged.placements));
    assert!(Arc::ptr_eq(
        &first.placements[0].image,
        &unchanged.placements[0].image
    ));
}

#[test]
fn png_decoder_accepts_rgba_and_replacement_invalidates_pixel_generation() {
    let (mut terminal, config, mut snapshots) = terminal();
    let transmission = format!("\x1b_Ga=T,f=100,i=31;{}\x1b\\", base64(&png_bytes()));
    terminal.vt_write(transmission.as_bytes());
    let first = snapshots.update(&terminal, &config).unwrap().graphics;
    assert_eq!(first.placements.len(), 1);
    assert_eq!(
        first.placements[0].image.rgba.as_ref(),
        &[
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 128,
        ]
    );
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=2,v=2,i=31;AAAAAAAAAAAAAAAAAAAAAA==\x1b\\");
    let replaced = snapshots.update(&terminal, &config).unwrap().graphics;
    assert_ne!(
        first.placements[0].image.generation,
        replaced.placements[0].image.generation
    );
    assert!(
        replaced.placements[0]
            .image
            .rgba
            .iter()
            .all(|byte| *byte == 0)
    );
    terminal.vt_write(b"\x1b_Ga=d,d=A\x1b\\");
    assert!(
        snapshots
            .update(&terminal, &config)
            .unwrap()
            .graphics
            .placements
            .is_empty()
    );
}

#[test]
fn png_grayscale_rgb_and_palette_transparency_normalize_to_rgba() {
    for (color_type, source, palette, transparency, expected) in [
        (
            png::ColorType::Rgb,
            vec![12, 34, 56],
            None,
            None,
            vec![12, 34, 56, 255],
        ),
        (
            png::ColorType::Grayscale,
            vec![91],
            None,
            None,
            vec![91, 91, 91, 255],
        ),
        (
            png::ColorType::GrayscaleAlpha,
            vec![91, 123],
            None,
            None,
            vec![91, 91, 91, 123],
        ),
        (
            png::ColorType::Indexed,
            vec![0],
            Some(vec![12, 34, 56]),
            Some(vec![123]),
            vec![12, 34, 56, 123],
        ),
    ] {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 1, 1);
            encoder.set_color(color_type);
            encoder.set_depth(png::BitDepth::Eight);
            if let Some(palette) = palette {
                encoder.set_palette(palette);
            }
            if let Some(transparency) = transparency {
                encoder.set_trns(transparency);
            }
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&source)
                .unwrap();
        }
        let (mut terminal, config, mut snapshots) = terminal();
        terminal.vt_write(format!("\x1b_Ga=T,f=100,i=31;{}\x1b\\", base64(&png)).as_bytes());
        let result = snapshots.update(&terminal, &config).unwrap().graphics;
        assert_eq!(result.placements.len(), 1, "{color_type:?}");
        assert_eq!(
            result.placements[0].image.rgba.as_ref(),
            expected,
            "{color_type:?}"
        );
    }
}

#[test]
fn disabled_graphics_clears_storage_on_both_screens() {
    let (mut terminal, mut config, mut snapshots) = terminal();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=7;/wAA/w==\x1b\\");
    assert_eq!(
        snapshots
            .update(&terminal, &config)
            .unwrap()
            .graphics
            .placements
            .len(),
        1
    );
    config.protocols.kitty_graphics = false;
    apply_theme(&mut terminal, &config).unwrap();
    assert!(
        snapshots
            .update(&terminal, &config)
            .unwrap()
            .graphics
            .placements
            .is_empty()
    );
    terminal.vt_write(b"\x1b[?1049h\x1b_Ga=T,f=32,s=1,v=1,i=7;/wAA/w==\x1b\\");
    assert!(
        snapshots
            .update(&terminal, &config)
            .unwrap()
            .graphics
            .placements
            .is_empty()
    );
}

#[test]
fn kitty_queries_reply_without_placing_probe_images() {
    let (mut terminal, config, mut snapshots) = terminal();
    let responses = Rc::new(RefCell::new(Vec::new()));
    let capture = responses.clone();
    terminal
        .on_pty_write(move |_, bytes| capture.borrow_mut().extend_from_slice(bytes))
        .unwrap();
    terminal.vt_write(b"\x1b_Ga=q,f=24,s=1,v=1,i=99;AAAA\x1b\\");
    assert_eq!(responses.borrow().as_slice(), b"\x1b_Gi=99;OK\x1b\\");
    assert!(
        snapshots
            .update(&terminal, &config)
            .unwrap()
            .graphics
            .placements
            .is_empty()
    );
}

#[test]
fn chunked_and_zlib_transmissions_share_the_same_rgba_snapshot() {
    let (mut terminal, config, mut snapshots) = terminal();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=42,m=1;/wAA\x1b\\");
    assert!(
        snapshots
            .update(&terminal, &config)
            .unwrap()
            .graphics
            .placements
            .is_empty()
    );
    terminal.vt_write(b"\x1b_Gm=0;/w==\x1b\\");
    let chunked = snapshots.update(&terminal, &config).unwrap().graphics;
    assert_eq!(chunked.placements[0].image.rgba.as_ref(), &[255, 0, 0, 255]);
    terminal.vt_write(b"\x1b_Ga=T,f=32,o=z,s=1,v=1,i=43;eJz7z8DwHwAE/wH/\x1b\\");
    let compressed = snapshots.update(&terminal, &config).unwrap().graphics;
    assert!(
        compressed
            .placements
            .iter()
            .any(|placement| placement.image_id == 43
                && placement.image.rgba.as_ref() == [255, 0, 0, 255])
    );
}

#[test]
fn scrolling_keeps_partial_placement_negative_origin_and_drops_offscreen_images() {
    let (mut terminal, config, mut snapshots) = terminal();
    terminal.resize(8, 4, 8, 16).unwrap();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=7,c=2,r=3,C=1;/wAA/w==\x1b\\");
    let first = snapshots.update(&terminal, &config).unwrap().graphics;
    assert_eq!(first.placements[0].grid_position, [0, 0]);
    terminal.vt_write(b"\x1b[4;1H\r\n");
    let partial = snapshots.update(&terminal, &config).unwrap().graphics;
    assert_eq!(partial.placements[0].grid_position[1], -1);
    assert_eq!(
        partial.placements[0].image.generation,
        first.placements[0].image.generation
    );
    terminal.vt_write(b"\r\n\r\n\r\n");
    assert!(
        snapshots
            .update(&terminal, &config)
            .unwrap()
            .graphics
            .placements
            .is_empty()
    );
}

#[test]
fn image_layers_follow_the_kitty_z_threshold() {
    assert_eq!(ImageLayer::from_z(i32::MIN), ImageLayer::BelowBackground);
    assert_eq!(
        ImageLayer::from_z(i32::MIN / 2 - 1),
        ImageLayer::BelowBackground
    );
    assert_eq!(ImageLayer::from_z(i32::MIN / 2), ImageLayer::BelowText);
    assert_eq!(ImageLayer::from_z(-1), ImageLayer::BelowText);
    assert_eq!(ImageLayer::from_z(0), ImageLayer::AboveText);
}

#[test]
fn kitten_icat_auto_stream_file_and_shared_memory_display_real_image() {
    let available = std::process::Command::new("kitten")
        .arg("--version")
        .output();
    if !available.is_ok_and(|output| output.status.success()) {
        eprintln!("kitten is not installed; skipping external icat integration");
        return;
    }
    let directory = std::env::temp_dir().join(format!("wisp-icat-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let image = directory.join("fixture.png");
    std::fs::File::create(&image)
        .unwrap()
        .write_all(&png_bytes())
        .unwrap();
    for medium in ["detect", "stream", "file", "memory"] {
        let mut command = CommandBuilder::new("python3");
        command.arg("-u");
        command.arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/icat_probe.py"));
        command.arg(medium);
        command.arg(&image);
        let session = Session::spawn_command(Config::default(), 80, 24, command).unwrap();
        session.send(Command::Resize {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
            pixel_width: 640,
            pixel_height: 384,
        });
        wait_title(&session, "icat-ready", Duration::from_secs(3));
        session.send(Command::Input(b"g".to_vec()));
        let started = Instant::now();
        let result = wait_title(&session, "icat-status:", Duration::from_secs(5));
        assert_eq!(result, "icat-status:0", "icat {medium} failed");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "icat {medium} spent time detecting unsupported protocol"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut captured = None;
        while Instant::now() < deadline {
            if let Some(snapshot) = session.try_snapshot() {
                if !snapshot.graphics.placements.is_empty() {
                    captured = Some(snapshot.graphics);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        let graphics =
            captured.unwrap_or_else(|| panic!("icat {medium} produced no image placement"));
        assert_eq!(graphics.placements[0].image.width, 2, "{medium}");
        assert_eq!(graphics.placements[0].image.height, 2, "{medium}");
        assert_eq!(
            &graphics.placements[0].image.rgba[..4],
            &[255, 0, 0, 255],
            "{medium}"
        );
        session.send(Command::Input(vec![4]));
    }
    std::fs::remove_file(image).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

fn wait_title(session: &Session, prefix: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Title(title) if title.starts_with(prefix) => return title,
                SessionEvent::Error(error) => panic!("terminal failed: {error}"),
                SessionEvent::Exited => panic!("terminal exited before {prefix}"),
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    panic!("timed out waiting for {prefix}");
}
