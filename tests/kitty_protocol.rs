use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::CommandBuilder;
use wisp::config::Config;
use wisp::terminal::{Command, Session, SessionEvent, TerminalSnapshot};

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const CLEAR: [u8; 4] = [0; 4];

#[derive(Debug)]
struct Reply {
    fields: BTreeMap<String, String>,
    code: String,
    message: String,
}

struct Probe {
    session: Session,
}

impl Probe {
    fn new(scenario: &str) -> Self {
        Self::with_arguments(scenario, &[])
    }

    fn with_arguments(scenario: &str, arguments: &[&std::ffi::OsStr]) -> Self {
        let mut command = CommandBuilder::new("python3");
        command.arg("-u");
        command.arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/graphics_protocol.py"),
        );
        command.arg(scenario);
        for argument in arguments {
            command.arg(argument);
        }
        let session = Session::spawn_command(Config::default(), 80, 24, command).unwrap();
        session.send(Command::Resize {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
            pixel_width: 640,
            pixel_height: 384,
        });
        let probe = Self { session };
        probe.wait_title("graphics-ready");
        probe
    }

    fn wait_title(&self, prefix: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            while let Some(event) = self.session.try_event() {
                match event {
                    SessionEvent::Title(title) if title.starts_with(prefix) => return title,
                    SessionEvent::Error(error) => panic!("graphics PTY failed: {error}"),
                    SessionEvent::Exited => panic!("graphics PTY exited before {prefix}"),
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline, "timed out waiting for {prefix}");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn step(&self, name: &str) -> Vec<Reply> {
        self.session.send(Command::Input(b"g".to_vec()));
        let prefix = format!("graphics-stage:{name}:");
        let title = self.wait_title(&prefix);
        let hex = title.strip_prefix(&prefix).unwrap();
        let bytes = hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect::<Vec<_>>();
        let text = std::str::from_utf8(&bytes).unwrap();
        text.split("\x1b_G")
            .skip(1)
            .map(|response| {
                let response = response.split("\x1b\\").next().unwrap();
                let (controls, message) = response.split_once(';').unwrap();
                Reply {
                    fields: controls
                        .split(',')
                        .map(|field| {
                            let (key, value) = field.split_once('=').unwrap();
                            (key.to_owned(), value.to_owned())
                        })
                        .collect(),
                    code: message.split(':').next().unwrap().to_owned(),
                    message: message.to_owned(),
                }
            })
            .collect()
    }

    fn frame(&self, predicate: impl Fn(&TerminalSnapshot) -> bool) -> TerminalSnapshot {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(frame) = self.session.try_snapshot()
                && predicate(&frame)
            {
                return frame;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for graphics frame"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn pixels(&self, id: u32, expected: &[u8]) -> TerminalSnapshot {
        self.frame(|frame| {
            frame.graphics.placements.iter().any(|placement| {
                placement.image_id == id && placement.image.rgba.as_ref() == expected
            })
        })
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.session.send(Command::Input(vec![4]));
    }
}

fn assert_reply(reply: &Reply, code: &str, image_id: u32) {
    assert_eq!(reply.code, code, "{reply:?}");
    assert_eq!(
        reply.fields.get("i"),
        Some(&image_id.to_string()),
        "{reply:?}"
    );
}

#[test]
fn graphics_queries_quiet_responses_and_image_numbers_follow_the_protocol() {
    let probe = Probe::new("responses");
    let replies = probe.step("query");
    assert_eq!(replies.len(), 1);
    assert_reply(&replies[0], "OK", 11);
    let replies = probe.step("quiet");
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_reply(&replies[0], "ENOENT", 999);
    let replies = probe.step("numbers");
    assert_eq!(replies.len(), 3, "{replies:?}");
    let first_id = replies[0].fields["i"].parse::<u32>().unwrap();
    let newest_id = replies[1].fields["i"].parse::<u32>().unwrap();
    assert_ne!(first_id, newest_id);
    for reply in &replies {
        assert_eq!(reply.fields.get("I").map(String::as_str), Some("901"));
        assert_eq!(reply.code, "OK");
    }
    assert_reply(&replies[2], "OK", newest_id);
    probe.pixels(newest_id, &GREEN);
}

#[test]
fn feature_queries_report_supported_values_without_interrupting_chunked_uploads() {
    let probe = Probe::new("features");
    assert!(probe.step("partial").is_empty());
    let replies = probe.step("features");
    assert_eq!(replies.len(), 4, "{replies:?}");
    for (reply, id, action, values) in [
        (&replies[0], 991, "T", [("U", "1"), ("C", "1")]),
        (&replies[1], 992, "f", [("r", "1"), ("X", "1")]),
        (&replies[2], 993, "a", [("s", "3"), ("v", "2")]),
        (&replies[3], 994, "c", [("C", "1"), ("w", "1")]),
    ] {
        assert_reply(reply, "OK", id);
        assert_eq!(reply.fields.get("e").map(String::as_str), Some(action));
        for (key, value) in values {
            assert_eq!(
                reply.fields.get(key).map(String::as_str),
                Some(value),
                "{reply:?}"
            );
        }
    }
    let replies = probe.step("complete");
    assert_eq!(replies.len(), 1);
    assert_reply(&replies[0], "OK", 21);
    probe.pixels(21, &[RED, GREEN].concat());
}

#[test]
fn indexed_images_follow_palette_changes_and_preserve_per_pixel_alpha() {
    let probe = Probe::new("indexed");
    let replies = probe.step("image");
    assert_eq!(replies.len(), 1);
    assert_reply(&replies[0], "OK", 71);
    let first = probe.frame(|frame| {
        frame
            .graphics
            .placements
            .iter()
            .any(|image| image.image_id == 71)
    });
    let initial = first
        .graphics
        .placements
        .iter()
        .find(|image| image.image_id == 71)
        .unwrap();
    let generation = initial.image.generation;
    assert!(probe.step("palette").is_empty());
    let expected = [
        0x12, 0x34, 0x56, 255, 0x11, 0x22, 0x33, 255, 0xab, 0xcd, 0xef, 255,
    ];
    let changed = probe.pixels(71, &expected);
    let image = changed
        .graphics
        .placements
        .iter()
        .find(|image| image.image_id == 71)
        .unwrap();
    assert_ne!(image.image.generation, generation);
    let replies = probe.step("alpha");
    assert_eq!(replies.len(), 1);
    assert_reply(&replies[0], "OK", 72);
    probe.pixels(72, &[0x12, 0x34, 0x56, 128, 0xab, 0xcd, 0xef, 0]);
    let replies = probe.step("reject-animation");
    assert_eq!(replies.len(), 1);
    assert_ne!(replies[0].code, "OK");
}

#[test]
fn deleting_an_id_range_preserves_images_outside_both_boundaries() {
    let probe = Probe::new("range");
    assert_eq!(probe.step("images").len(), 3);
    probe.frame(|frame| frame.graphics.placements.len() == 3);
    assert!(probe.step("delete").is_empty());
    let frame = probe.frame(|frame| frame.graphics.placements.len() == 2);
    let ids = frame
        .graphics
        .placements
        .iter()
        .map(|image| image.image_id)
        .collect::<HashSet<_>>();
    assert_eq!(ids, HashSet::from([10, 30]));
    let replies = probe.step("reload");
    assert_eq!(replies.len(), 1);
    assert_reply(&replies[0], "ENOENT", 20);
}

#[test]
fn automatic_image_ids_avoid_collisions_and_delete_aborts_partial_transfers() {
    let probe = Probe::new("lifecycle");
    assert_reply(&probe.step("explicit")[0], "OK", 2_147_483_647);
    probe.pixels(2_147_483_647, &RED);
    let replies = probe.step("automatic");
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].code, "OK");
    let automatic_id = replies[0].fields["i"].parse::<u32>().unwrap();
    assert_ne!(automatic_id, 2_147_483_647);
    probe.pixels(automatic_id, &GREEN);
    assert!(probe.step("partial").is_empty());
    let replies = probe.step("abort");
    assert_eq!(replies.len(), 1);
    assert_reply(&replies[0], "OK", 23);
    probe.pixels(23, &BLUE);
}

#[test]
fn replacing_image_data_clears_old_placements_but_failed_replacement_preserves_them() {
    let probe = Probe::new("replacement");
    assert_reply(&probe.step("root")[0], "OK", 24);
    probe.pixels(24, &RED);
    assert_reply(&probe.step("replace")[0], "OK", 24);
    probe.frame(|frame| frame.graphics.placements.is_empty());
    assert_reply(&probe.step("place")[0], "OK", 24);
    probe.pixels(24, &GREEN);
    let replies = probe.step("bad-replace");
    assert_eq!(replies.len(), 1);
    assert_ne!(replies[0].code, "OK");
    probe.pixels(24, &GREEN);
}

#[test]
fn image_file_failures_have_identical_responses_and_special_files_never_block() {
    let probe = Probe::new("file-errors");
    assert_reply(&probe.step("file")[0], "OK", 25);
    probe.pixels(25, &RED);
    let replies = probe.step("errors");
    assert_eq!(replies.len(), 7, "{replies:?}");
    for (reply, id) in replies.iter().zip([26, 27, 28, 29, 30, 25, 31]) {
        assert_reply(reply, "EBADF", id);
        assert_eq!(reply.message, "EBADF:Failed to read image file");
    }
}

#[test]
fn all_four_transports_preserve_offsets_formats_frames_and_cleanup() {
    for medium in ["d", "f", "t", "s"] {
        let probe = Probe::with_arguments("transport-matrix", &[medium.as_ref()]);
        for format in ["rgba", "rgb", "png", "zlib", "png-zlib"] {
            let replies = probe.step(&format!("root-{format}"));
            assert_eq!(replies.len(), 1, "{medium}/{format}: {replies:?}");
            assert_reply(&replies[0], "OK", 151);
            probe.pixels(151, &[RED, GREEN].concat());
            let replies = probe.step(&format!("frame-{format}"));
            assert_eq!(replies.len(), 1, "{medium}/{format}: {replies:?}");
            assert_reply(&replies[0], "OK", 151);
            assert!(probe.step(&format!("select-{format}")).is_empty());
            probe.pixels(151, &[RED, GREEN].concat());
        }
        for format in [8, 16] {
            let replies = probe.step(&format!("indexed-{format}"));
            assert_eq!(replies.len(), 1, "{medium}/{format}: {replies:?}");
            assert_reply(&replies[0], "OK", 152);
            probe.frame(|frame| {
                frame.graphics.placements.iter().any(|placement| {
                    placement.image_id == 152
                        && placement.image.rgba.len() == 8
                        && placement.image.rgba[3] == if format == 8 { 255 } else { 128 }
                })
            });
        }
    }
}

#[test]
fn local_transports_reject_short_or_out_of_bounds_reads_and_only_unlink_owned_resources() {
    for medium in ["f", "t", "s"] {
        let probe = Probe::with_arguments("transport-errors", &[medium.as_ref()]);
        for stage in ["short", "offset", "length", "undersized", "missing"] {
            let replies = probe.step(stage);
            assert_eq!(replies.len(), 1, "{medium}/{stage}: {replies:?}");
            assert_reply(&replies[0], "EBADF", 161);
            assert_eq!(replies[0].message, "EBADF:Failed to read image file");
        }
        if medium != "s" {
            assert_reply(&probe.step("special")[0], "EBADF", 161);
            assert_reply(&probe.step("symlink")[0], "OK", 163);
            probe.pixels(163, &[RED, GREEN].concat());
        }
        if medium == "t" {
            assert_reply(&probe.step("retained")[0], "OK", 162);
            probe.pixels(162, &[RED, GREEN].concat());
        }
    }
}

#[test]
fn animation_frame_patches_composition_and_deletion_preserve_exact_pixels() {
    let probe = Probe::new("animation");
    assert_reply(&probe.step("root")[0], "OK", 61);
    probe.pixels(61, &RED.repeat(4));
    let replies = probe.step("frame");
    assert_eq!(replies.len(), 1);
    assert_reply(&replies[0], "OK", 61);
    assert_eq!(replies[0].fields.get("r").map(String::as_str), Some("2"));
    assert!(probe.step("select").is_empty());
    probe.pixels(61, &[RED, RED, RED, GREEN].concat());
    assert_reply(&probe.step("edit-blend")[0], "OK", 61);
    probe.pixels(61, &[RED, RED, RED, GREEN].concat());
    assert_reply(&probe.step("edit-overwrite")[0], "OK", 61);
    probe.pixels(61, &[CLEAR, RED, RED, GREEN].concat());
    assert_reply(&probe.step("compose")[0], "OK", 61);
    let expected = [CLEAR, RED, RED, RED].concat();
    probe.pixels(61, &expected);
    let replies = probe.step("compose-errors");
    assert_eq!(replies.len(), 3, "{replies:?}");
    assert_reply(&replies[0], "EINVAL", 61);
    assert_reply(&replies[1], "EINVAL", 61);
    assert_reply(&replies[2], "ENOENT", 61);
    assert!(probe.step("delete-root").is_empty());
    probe.pixels(61, &expected);
    assert!(probe.step("delete-last").is_empty());
    probe.frame(|frame| frame.graphics.placements.is_empty());
}

#[test]
fn chunked_and_compressed_animation_uploads_preserve_the_base_image() {
    let probe = Probe::new("animation-chunks");
    assert_reply(&probe.step("root")[0], "OK", 62);
    probe.pixels(62, &RED.repeat(2));
    assert!(probe.step("partial").is_empty());
    let replies = probe.step("complete");
    assert_eq!(replies.len(), 1);
    assert_reply(&replies[0], "OK", 62);
    assert_eq!(replies[0].fields.get("r").map(String::as_str), Some("2"));
    assert!(probe.step("select").is_empty());
    probe.pixels(62, &[GREEN, BLUE].concat());
    let replies = probe.step("compressed");
    assert_eq!(replies.len(), 1);
    assert_reply(&replies[0], "OK", 62);
    assert_eq!(replies[0].fields.get("r").map(String::as_str), Some("3"));
    assert!(probe.step("select-third").is_empty());
    probe.pixels(62, &BLUE.repeat(2));
    assert!(probe.step("quiet").is_empty());
    let replies = probe.step("repeat-id");
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_reply(&replies[0], "OK", 62);
    assert_eq!(replies[0].fields.get("r").map(String::as_str), Some("5"));
    assert!(probe.step("repeat-id-select").is_empty());
    probe.pixels(62, &GREEN.repeat(2));
}

#[test]
fn animation_frames_support_png_file_temporary_and_shared_memory_transport_by_image_number() {
    let probe = Probe::new("animation-transport");
    let replies = probe.step("root");
    assert_eq!(replies.len(), 1);
    let id = replies[0].fields["i"].parse::<u32>().unwrap();
    assert_eq!(replies[0].fields.get("I").map(String::as_str), Some("913"));
    probe.pixels(id, &RED.repeat(2));
    for (name, select, frame, expected) in [
        ("file", "file-select", "2", [GREEN, BLUE].concat()),
        ("temporary", "temporary-select", "3", BLUE.repeat(2)),
        ("shared", "shared-select", "4", [RED, GREEN].concat()),
        ("png", "png-select", "5", [GREEN, RED].concat()),
    ] {
        let replies = probe.step(name);
        assert_eq!(replies.len(), 1, "{name}: {replies:?}");
        assert_reply(&replies[0], "OK", id);
        assert_eq!(replies[0].fields.get("I").map(String::as_str), Some("913"));
        assert_eq!(replies[0].fields.get("r").map(String::as_str), Some(frame));
        assert!(probe.step(select).is_empty());
        probe.pixels(id, &expected);
    }
}

#[test]
fn frame_canvas_background_alpha_and_errors_follow_the_protocol() {
    let probe = Probe::new("animation-canvas");
    assert_reply(&probe.step("root")[0], "OK", 65);
    assert_reply(&probe.step("canvas")[0], "OK", 65);
    assert!(probe.step("select").is_empty());
    probe.pixels(65, &[BLUE, BLUE, BLUE, GREEN].concat());
    assert_reply(&probe.step("half-alpha")[0], "OK", 65);
    probe.pixels(65, &[[128, 0, 127, 255], BLUE, BLUE, GREEN].concat());
    let replies = probe.step("errors");
    assert_eq!(replies.len(), 5, "{replies:?}");
    assert_reply(&replies[0], "ENOENT", 999);
    for reply in &replies[1..] {
        assert_reply(reply, "EINVAL", 65);
    }
}

#[test]
fn terminal_driven_animation_waits_for_loading_frames_and_stops_after_a_finite_loop() {
    let probe = Probe::new("animation-loading");
    assert_eq!(probe.step("root").len(), 2);
    probe.pixels(63, &RED);
    assert!(probe.step("loading").is_empty());
    probe.pixels(63, &GREEN);
    thread::sleep(Duration::from_millis(220));
    assert_reply(&probe.step("append")[0], "OK", 63);
    probe.pixels(63, &BLUE);
    assert!(probe.step("stop").is_empty());
    assert!(probe.step("finite").is_empty());
    probe.pixels(63, &RED);
    probe.pixels(63, &GREEN);
    probe.pixels(63, &BLUE);
    let deadline = Instant::now() + Duration::from_millis(350);
    while Instant::now() < deadline {
        if let Some(frame) = probe.session.try_snapshot() {
            assert!(
                frame
                    .graphics
                    .placements
                    .iter()
                    .all(|image| image.image.rgba.as_ref() == BLUE)
            );
        }
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn relative_placements_follow_parents_reject_cycles_and_delete_descendants() {
    let probe = Probe::new("relative");
    assert_reply(&probe.step("parent")[0], "OK", 101);
    probe.frame(|frame| frame.graphics.placements.len() == 1);
    assert_reply(&probe.step("child")[0], "OK", 102);
    let frame = probe.frame(|frame| frame.graphics.placements.len() == 2);
    let child = frame
        .graphics
        .placements
        .iter()
        .find(|image| image.image_id == 102)
        .unwrap();
    assert_eq!(child.grid_position, [5, 1]);
    assert_eq!((frame.cursor.column, frame.cursor.row), (3, 2));
    assert_reply(&probe.step("move")[0], "OK", 101);
    let frame = probe.frame(|frame| {
        frame
            .graphics
            .placements
            .iter()
            .any(|image| image.image_id == 102 && image.grid_position == [9, 4])
    });
    assert_eq!((frame.cursor.column, frame.cursor.row), (7, 5));
    let replies = probe.step("errors");
    assert_eq!(replies.len(), 2);
    assert_reply(&replies[0], "ECYCLE", 101);
    assert_reply(&replies[1], "ENOPARENT", 102);
    assert!(probe.step("delete").is_empty());
    probe.frame(|frame| frame.graphics.placements.is_empty());
    let replies = probe.step("child-gone");
    assert_eq!(replies.len(), 1);
    assert_reply(&replies[0], "ENOENT", 102);
}

#[test]
fn animation_state_is_isolated_between_screens_and_cleared_by_reset() {
    let probe = Probe::new("screens");
    assert_eq!(probe.step("main").len(), 2);
    probe.pixels(64, &GREEN);
    assert_reply(&probe.step("alternate")[0], "OK", 64);
    probe.pixels(64, &BLUE);
    assert!(probe.step("restore").is_empty());
    probe.pixels(64, &GREEN);
    assert!(probe.step("reset").is_empty());
    probe.frame(|frame| frame.graphics.placements.is_empty());
}

fn icat_dependencies_available() -> bool {
    let kitten = std::process::Command::new("kitten")
        .arg("--version")
        .output();
    let pillow = std::process::Command::new("python3")
        .args(["-c", "from PIL import Image"])
        .status();
    if !kitten.is_ok_and(|output| output.status.success())
        || !pillow.is_ok_and(|status| status.success())
    {
        eprintln!("kitten or Pillow unavailable; skipping real GIF integration");
        return false;
    }
    true
}

#[test]
fn actual_kitten_icat_gif_animates_after_the_client_exits() {
    if !icat_dependencies_available() {
        return;
    }
    let directory = std::env::temp_dir().join(format!("wisp-animated-icat-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let probe = Probe::with_arguments("icat", &[directory.as_os_str()]);
    probe.session.send(Command::Input(b"g".to_vec()));
    assert_eq!(
        probe.wait_title("graphics-stage:icat:"),
        "graphics-stage:icat:30"
    );
    let mut colors = HashSet::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && colors.len() < 3 {
        if let Some(frame) = probe.session.try_snapshot() {
            for placement in frame.graphics.placements.iter() {
                if placement.image.rgba.len() >= 4 {
                    colors.insert(placement.image.rgba[..4].to_vec());
                }
            }
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        colors.len() >= 3,
        "icat produced only these frame colors: {colors:?}"
    );
    drop(probe);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn kitten_icat_animates_large_delta_frames_with_every_transfer_mode() {
    use std::hash::{Hash, Hasher};

    if !icat_dependencies_available() {
        return;
    }
    let directory = std::env::temp_dir().join(format!("wisp-delta-icat-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    for mode in ["detect", "stream", "file", "memory"] {
        let probe = Probe::with_arguments("icat-delta", &[directory.as_os_str(), mode.as_ref()]);
        probe.session.send(Command::Input(b"g".to_vec()));
        assert_eq!(
            probe.wait_title("graphics-stage:icat:"),
            "graphics-stage:icat:30"
        );
        let mut frames = HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && frames.len() < 4 {
            if let Some(frame) = probe.session.try_snapshot() {
                for placement in frame.graphics.placements.iter() {
                    let mut hash = std::collections::hash_map::DefaultHasher::new();
                    placement.image.rgba.hash(&mut hash);
                    frames.insert(hash.finish());
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            frames.len() >= 4,
            "icat {mode} produced only {} distinct frames",
            frames.len()
        );
        drop(probe);
    }
    std::fs::remove_dir_all(directory).unwrap();
}
