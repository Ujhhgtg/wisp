use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::terminal::ScrollViewport;
use libghostty_vt::{Terminal, TerminalOptions};
use wisp::config::{Color, Config, CursorShape};
use wisp::terminal::{Rgb, SnapshotBuilder, TerminalSnapshot, apply_theme};

struct Fixture {
    terminal: Terminal<'static, 'static>,
    snapshots: SnapshotBuilder,
    config: Config,
}

impl Fixture {
    fn new(cols: u16, rows: u16) -> Self {
        let mut terminal = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: 1024 * 1024,
        })
        .unwrap();
        let mut config = Config::default();
        config.colors.background = Color::new(0x28, 0x2c, 0x34);
        config.colors.foreground = Color::new(0xab, 0xb2, 0xbf);
        apply_theme(&mut terminal, &config).unwrap();
        Self {
            terminal,
            snapshots: SnapshotBuilder::new().unwrap(),
            config,
        }
    }

    fn feed(&mut self, bytes: &[u8]) -> TerminalSnapshot {
        self.terminal.vt_write(bytes);
        self.snapshot()
    }

    fn snapshot(&mut self) -> TerminalSnapshot {
        self.snapshots.update(&self.terminal, &self.config).unwrap()
    }
}

fn text(snapshot: &TerminalSnapshot, row: usize) -> String {
    let start = row * usize::from(snapshot.cols);
    snapshot.cells[start..start + usize::from(snapshot.cols)]
        .iter()
        .map(|cell| {
            if cell.text.is_empty() && cell.width > 0 {
                " "
            } else {
                cell.text.as_str()
            }
        })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[test]
fn production_snapshot_places_text_and_cursor_in_the_same_grid() {
    let mut fixture = Fixture::new(16, 4);
    let snapshot = fixture.feed(b"Hello\x1b[3;5HX");
    assert_eq!(snapshot.cells.len(), 16 * 4);
    assert_eq!(text(&snapshot, 0), "Hello");
    assert_eq!(snapshot.cells[2 * 16 + 4].text, "X");
    assert_eq!((snapshot.cursor.column, snapshot.cursor.row), (5, 2));
    assert!(snapshot.cursor.visible);
    assert_eq!(snapshot.cursor.shape, CursorShape::Beam);
}

#[test]
fn unicode_clusters_and_wide_spacers_do_not_add_extra_columns() {
    let mut fixture = Fixture::new(20, 3);
    let bytes = "A漢e\u{301}B".as_bytes();
    for byte in bytes {
        fixture.terminal.vt_write(&[*byte]);
    }
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.cells[0].text, "A");
    assert_eq!(snapshot.cells[1].text, "漢");
    assert_eq!(snapshot.cells[1].width, 2);
    assert_eq!(snapshot.cells[2].width, 0);
    assert!(snapshot.cells[2].text.is_empty());
    assert_eq!(snapshot.cells[3].text, "e\u{301}");
    assert_eq!(snapshot.cells[4].text, "B");
    assert_eq!((snapshot.cursor.column, snapshot.cursor.row), (5, 0));
}

#[test]
fn ansi_palette_and_truecolor_survive_the_production_snapshot() {
    let mut fixture = Fixture::new(20, 3);
    let palette = fixture.config.colors.ansi_palette();
    let mut bytes = Vec::new();
    for index in 0..16 {
        let sgr = if index < 8 {
            30 + index
        } else {
            90 + index - 8
        };
        bytes.extend_from_slice(format!("\x1b[{sgr}mX").as_bytes());
    }
    bytes.extend_from_slice(b"\x1b[38;2;13;79;151;48;2;19;37;61mY");
    let snapshot = fixture.feed(&bytes);
    for (index, color) in palette.into_iter().enumerate() {
        assert_eq!(
            snapshot.cells[index].foreground,
            Rgb::from(color),
            "color{index}"
        );
    }
    assert_eq!(
        snapshot.cells[16].foreground,
        Rgb::from(Color::new(13, 79, 151))
    );
    assert_eq!(
        snapshot.cells[16].background,
        Some(Rgb::from(Color::new(19, 37, 61)))
    );
    assert_eq!(
        snapshot.background,
        Rgb::from(fixture.config.colors.background)
    );
}

#[test]
fn sgr_inverse_reset_and_bce_use_configured_default_colors() {
    let mut fixture = Fixture::new(12, 3);
    let snapshot = fixture.feed(b"\x1b[1;3;4;7mA\x1b[0mB\x1b[48;2;12;34;56m\x1b[K");
    let reversed = &snapshot.cells[0];
    assert!(reversed.bold && reversed.italic && reversed.underline);
    assert_eq!(
        reversed.foreground,
        Rgb::from(fixture.config.colors.background)
    );
    assert_eq!(
        reversed.background,
        Some(Rgb::from(fixture.config.colors.foreground))
    );
    assert_eq!(
        snapshot.cells[1].foreground,
        Rgb::from(fixture.config.colors.foreground)
    );
    assert_eq!(snapshot.cells[1].background, None);
    for cell in &snapshot.cells[2..12] {
        assert_eq!(cell.background, Some(Rgb::from(Color::new(12, 34, 56))));
    }
}

#[test]
fn osc_theme_overrides_and_resets_update_background_and_palette() {
    let mut fixture = Fixture::new(12, 3);
    let snapshot =
        fixture.feed(b"\x1b]11;#123456\x1b\\\x1b]10;#a1b2c3\x1b\\\x1b]4;1;#654321\x1b\\\x1b[31mX");
    assert_eq!(snapshot.background, Rgb::from(Color::new(0x12, 0x34, 0x56)));
    assert_eq!(snapshot.foreground, Rgb::from(Color::new(0xa1, 0xb2, 0xc3)));
    assert_eq!(
        snapshot.cells[0].foreground,
        Rgb::from(Color::new(0x65, 0x43, 0x21))
    );
    let reset = fixture.feed(b"\x1b]111\x1b\\\x1b]110\x1b\\\x1b]104;1\x1b\\\x1b[31mY");
    assert_eq!(
        reset.background,
        Rgb::from(fixture.config.colors.background)
    );
    assert_eq!(
        reset.foreground,
        Rgb::from(fixture.config.colors.foreground)
    );
    assert_eq!(
        reset.cells[1].foreground,
        Rgb::from(fixture.config.colors.color1)
    );
}

#[test]
fn cursor_style_visibility_and_alternate_screen_restore() {
    let mut fixture = Fixture::new(20, 4);
    let first = fixture.feed(b"ABC\x1b[3 q");
    assert_eq!(first.cursor.shape, CursorShape::Underline);
    let hidden = fixture.feed(b"\x1b[?25l");
    assert!(!hidden.cursor.visible);
    let alternate = fixture.feed(b"\x1b[?25h\x1b[?1049h\x1b[2J\x1b[HALT\x1b[6 q");
    assert_eq!(text(&alternate, 0), "ALT");
    assert_eq!(alternate.cursor.shape, CursorShape::Beam);
    let restored = fixture.feed(b"\x1b[?1049l");
    assert_eq!(text(&restored, 0), "ABC");
    assert_eq!((restored.cursor.column, restored.cursor.row), (3, 0));
}

#[test]
fn scrollback_viewport_and_cursor_agree_at_top_and_bottom() {
    let mut fixture = Fixture::new(10, 3);
    for number in 1..=10 {
        fixture
            .terminal
            .vt_write(format!("L{number:02}\r\n").as_bytes());
    }
    fixture.terminal.scroll_viewport(ScrollViewport::Top);
    let top = fixture.snapshot();
    assert_eq!(text(&top, 0), "L01");
    assert!(!top.cursor.visible);
    fixture.terminal.scroll_viewport(ScrollViewport::Bottom);
    let bottom = fixture.snapshot();
    assert_eq!(text(&bottom, 0), "L09");
    assert_eq!(text(&bottom, 1), "L10");
    assert_eq!((bottom.cursor.column, bottom.cursor.row), (0, 2));
    assert!(bottom.cursor.visible);
}

#[test]
fn resize_reflow_preserves_text_and_cursor() {
    let mut fixture = Fixture::new(10, 4);
    fixture.feed(b"abcdefghijklmnop");
    fixture.terminal.resize(8, 4, 9, 19).unwrap();
    let snapshot = fixture.snapshot();
    assert_eq!(snapshot.cols, 8);
    assert_eq!(snapshot.cells.len(), 32);
    assert_eq!(text(&snapshot, 0), "abcdefgh");
    assert_eq!(text(&snapshot, 1), "ijklmnop");
    assert_eq!((snapshot.cursor.column, snapshot.cursor.row), (0, 2));
    let appended = fixture.feed(b"Q");
    assert_eq!(text(&appended, 2), "Q");
    assert_eq!((appended.cursor.column, appended.cursor.row), (1, 2));
}

#[test]
fn terminal_query_responses_use_one_based_vt_coordinates() {
    let mut fixture = Fixture::new(80, 24);
    let replies = Rc::new(RefCell::new(Vec::new()));
    let capture = replies.clone();
    fixture
        .terminal
        .on_pty_write(move |_, bytes| capture.borrow_mut().extend_from_slice(bytes))
        .unwrap();
    fixture.feed(b"\x1b[7;13H\x1b[6n\x1b[5n");
    assert_eq!(replies.borrow().as_slice(), b"\x1b[7;13R\x1b[0n");
}

#[test]
fn repeated_clean_and_dirty_snapshots_preserve_grid_size_and_colors() {
    let mut fixture = Fixture::new(16, 3);
    let first = fixture.feed(b"\x1b[48;2;40;44;52mhello");
    let clean = fixture.snapshot();
    assert_eq!(text(&first, 0), text(&clean, 0));
    assert_eq!(first.background, clean.background);
    assert_eq!(first.cells[0].background, clean.cells[0].background);
    let dirty = fixture.feed(b"\x1b[2;1Hnew");
    assert_eq!(dirty.cells.len(), 48);
    assert_eq!(text(&dirty, 0), "hello");
    assert_eq!(text(&dirty, 1), "new");
}

#[test]
fn complete_sgr_decorations_and_conceal_survive_the_production_snapshot() {
    let mut fixture = Fixture::new(8, 2);
    let frame = fixture.feed(b"\x1b[4:3;58;2;12;34;56;9;53;5mA\x1b[0mB\x1b[8mC");
    let decorated = &frame.cells[0];
    assert_eq!(
        decorated.underline_style,
        wisp::terminal::UnderlineStyle::Curly
    );
    assert_eq!(
        decorated.underline_color,
        Some(Rgb {
            red: 12,
            green: 34,
            blue: 56
        })
    );
    assert!(decorated.strikethrough && decorated.overline && decorated.blink);
    let reset = &frame.cells[1];
    assert_eq!(reset.underline_style, wisp::terminal::UnderlineStyle::None);
    assert_eq!(reset.underline_color, None);
    assert!(!reset.strikethrough && !reset.overline && !reset.blink && !reset.hidden);
    assert!(frame.cells[2].hidden);
    assert_eq!(frame.cells[2].foreground, frame.background);
}

#[test]
fn dense_combining_graphemes_survive_capacity_growth_and_mixed_width_reflow() {
    let mut fixture = Fixture::new(160, 64);
    let mut expected = String::new();
    for index in 0..512 {
        expected.push('a');
        for shift in [0, 4, 8] {
            expected.push(char::from_u32(0x300 + ((index >> shift) % 0x70)).unwrap());
        }
        expected.push_str("漢b\u{301}界");
    }
    for chunk in expected.as_bytes().chunks(113) {
        fixture.terminal.vt_write(chunk);
    }
    for columns in [160, 63, 191, 80, 160] {
        fixture.terminal.resize(columns, 64, 9, 19).unwrap();
        let snapshot = fixture.snapshot();
        let actual = snapshot
            .cells
            .iter()
            .filter(|cell| cell.width > 0)
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        assert_eq!(
            actual, expected,
            "grapheme content after reflow to {columns}"
        );
        assert_eq!(
            snapshot.cells.iter().filter(|cell| cell.width == 2).count(),
            1024,
            "wide character count after reflow to {columns}"
        );
        assert!(snapshot.cursor.visible);
    }
}

#[test]
fn repeat_character_matches_literal_output_across_modes_and_batch_boundaries() {
    for (setup, character) in [
        ("", 'a'),
        ("\x1b[?7l", 'x'),
        ("\x1b[4hABCDE\r\x1b[3G", 'b'),
        ("\x1b[2;5r\x1b[5;17H", 'c'),
        ("\x1b[?69h\x1b[3;13s\x1b[2;3H", 'd'),
        ("\x1b(0", 'q'),
        ("\x1b[1;31;44m", 'e'),
        ("", '漢'),
        ("", '🙂'),
    ] {
        for count in [1, 16, 17, 18, 127, 128, 129, 4095, 4096, 4097] {
            let mut repeated = Fixture::new(17, 7);
            let mut literal = Fixture::new(17, 7);
            let prefix = format!("{setup}{character}");
            repeated.terminal.vt_write(prefix.as_bytes());
            literal.terminal.vt_write(prefix.as_bytes());
            repeated
                .terminal
                .vt_write(format!("\x1b[{count}b").as_bytes());
            literal
                .terminal
                .vt_write(character.to_string().repeat(count).as_bytes());
            let repeated = repeated.snapshot();
            let literal = literal.snapshot();
            assert_eq!(
                repeated.cells, literal.cells,
                "REP {count}, setup {setup:?}, character {character}"
            );
            assert_eq!(
                repeated.cursor, literal.cursor,
                "REP cursor {count}, setup {setup:?}, character {character}"
            );
        }
    }
}
