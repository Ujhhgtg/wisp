use libghostty_vt::{Terminal, TerminalOptions};
use wisp::config::Config;
use wisp::terminal::{Rgb, SnapshotBuilder, TerminalSnapshot, apply_theme};

struct Fixture {
    terminal: Terminal<'static, 'static>,
    config: Config,
    snapshots: SnapshotBuilder,
}

impl Fixture {
    fn new() -> Self {
        let config = Config::default();
        let mut terminal = Terminal::new(TerminalOptions {
            cols: 12,
            rows: 8,
            max_scrollback: 1024 * 1024,
        })
        .unwrap();
        terminal.resize(12, 8, 8, 16).unwrap();
        apply_theme(&mut terminal, &config).unwrap();
        Self {
            terminal,
            config,
            snapshots: SnapshotBuilder::new().unwrap(),
        }
    }

    fn write(&mut self, text: &str) -> TerminalSnapshot {
        self.snapshots.write(
            &mut self.terminal,
            &self.config,
            text.as_bytes(),
            &mut |_| {},
        );
        self.snapshots.update(&self.terminal, &self.config).unwrap()
    }
}

#[test]
fn virtual_images_resolve_high_ids_placement_colors_and_omitted_diacritics() {
    let mut fixture = Fixture::new();
    let image_id = 42 | (2 << 24);
    let transmission =
        format!("\x1b_Ga=T,f=32,s=1,v=1,i={image_id},p=9,U=1,c=2,r=1;/wAA/w==\x1b\\");
    let empty = fixture.write(&transmission);
    assert!(empty.graphics.placements.is_empty());
    assert_eq!(empty.cursor.column, 0);
    assert_eq!(empty.cursor.row, 0);
    let frame = fixture.write(
        "\x1b[3;4H\x1b[38;5;42;58;5;9;48;2;11;22;33m\u{10eeee}\u{305}\u{305}\u{30e}\u{10eeee}",
    );
    assert_eq!(frame.graphics.placements.len(), 1);
    let image = &frame.graphics.placements[0];
    assert_eq!(image.image_id, image_id);
    assert_eq!(image.placement_id, 9);
    assert_eq!(image.grid_position, [3, 2]);
    assert_eq!(image.pixel_size, [16, 16]);
    assert_eq!(image.source, [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(image.z, -1);
    for column in 3..5 {
        let cell = &frame.cells[2 * 12 + column];
        assert!(cell.text.is_empty());
        assert_eq!(
            cell.background,
            Some(Rgb {
                red: 11,
                green: 22,
                blue: 33
            })
        );
    }
    assert_eq!(frame.cursor.column, 5);
    assert_eq!(frame.cursor.row, 2);
}

#[test]
fn placeholder_fragments_follow_wrapping_and_keep_image_source_coordinates() {
    let mut fixture = Fixture::new();
    fixture.write("\x1b_Ga=T,f=32,s=2,v=1,i=7,p=3,U=1,c=4,r=2;/wAA/wD/AP8=\x1b\\");
    let frame = fixture
        .write("\x1b[2;12H\x1b[38;5;7;58;5;3m\u{10eeee}\u{305}\u{30e}\u{10eeee}\u{30d}\u{310}");
    assert_eq!(frame.graphics.placements.len(), 2);
    let top = frame
        .graphics
        .placements
        .iter()
        .find(|image| image.grid_position == [11, 1])
        .unwrap();
    let bottom = frame
        .graphics
        .placements
        .iter()
        .find(|image| image.grid_position == [0, 2])
        .unwrap();
    assert_eq!(top.source, [1.0, 0.0, 0.5, 0.5]);
    assert_eq!(top.offset, [0, 8]);
    assert_eq!(top.pixel_size, [8, 8]);
    assert_eq!(bottom.source, [1.5, 0.5, 0.5, 0.5]);
    assert_eq!(bottom.pixel_size, [8, 8]);
    assert_eq!(bottom.offset, [0, 0]);
}

#[test]
fn text_erasure_and_id_deletion_remove_placeholder_images() {
    let mut fixture = Fixture::new();
    fixture.write("\x1b_Ga=T,f=32,s=1,v=1,i=8,U=1,c=1,r=1;/wAA/w==\x1b\\");
    let frame = fixture.write("\x1b[38;5;8m\u{10eeee}\u{305}\u{305}");
    assert_eq!(frame.graphics.placements.len(), 1);
    let erased = fixture.write("\rX");
    assert!(erased.graphics.placements.is_empty());
    let frame = fixture.write("\r\u{10eeee}\u{305}\u{305}");
    assert_eq!(frame.graphics.placements.len(), 1);
    let deleted = fixture.write("\x1b_Ga=d,d=i,i=8\x1b\\");
    assert!(deleted.graphics.placements.is_empty());
    assert!(deleted.cells[0].text.is_empty());
}
