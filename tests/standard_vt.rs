use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::style::{RgbColor, Style, StyleColor, Underline};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, ScrollViewport};
use libghostty_vt::{Terminal, TerminalOptions};
use wisp::config::{Color, Config};
use wisp::terminal::{Rgb, SnapshotBuilder, TerminalSnapshot, apply_theme};

struct TerminalFixture {
    terminal: Terminal<'static, 'static>,
    snapshots: SnapshotBuilder,
    config: Config,
    replies: Rc<RefCell<Vec<u8>>>,
}

impl TerminalFixture {
    fn new(columns: u16, rows: u16) -> Self {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: columns,
            rows,
            max_scrollback: 1024 * 1024,
        })
        .unwrap();
        terminal.resize(columns, rows, 8, 16).unwrap();
        let config = Config::default();
        apply_theme(&mut terminal, &config).unwrap();
        let replies = Rc::new(RefCell::new(Vec::new()));
        let capture = replies.clone();
        terminal
            .on_pty_write(move |_, bytes| capture.borrow_mut().extend_from_slice(bytes))
            .unwrap();
        Self {
            terminal,
            snapshots: SnapshotBuilder::new().unwrap(),
            config,
            replies,
        }
    }

    fn write(&mut self, data: &[u8]) {
        let replies = self.replies.clone();
        self.snapshots
            .write(&mut self.terminal, &self.config, data, &mut |bytes| {
                replies.borrow_mut().extend_from_slice(bytes)
            });
    }

    fn frame(&mut self) -> TerminalSnapshot {
        self.snapshots.update(&self.terminal, &self.config).unwrap()
    }

    fn row(&mut self, row: usize) -> String {
        let frame = self.frame();
        row_text(&frame, row)
    }

    fn cursor(&self) -> (u16, u16) {
        (
            self.terminal.cursor_x().unwrap(),
            self.terminal.cursor_y().unwrap(),
        )
    }

    fn style(&self, column: u16, row: u32) -> Style {
        self.terminal
            .grid_ref(Point::Viewport(PointCoordinate { x: column, y: row }))
            .unwrap()
            .style()
            .unwrap()
    }

    fn reply(&mut self, data: &[u8]) -> Vec<u8> {
        self.replies.borrow_mut().clear();
        self.write(data);
        self.replies.borrow_mut().drain(..).collect()
    }
}

fn row_text(frame: &TerminalSnapshot, row: usize) -> String {
    frame.cells[row * usize::from(frame.cols)..(row + 1) * usize::from(frame.cols)]
        .iter()
        .filter(|cell| cell.width != 0)
        .map(|cell| {
            if cell.text.is_empty() {
                " "
            } else {
                cell.text.as_str()
            }
        })
        .collect()
}

#[test]
fn c0_controls_move_the_cursor_without_printing_control_glyphs() {
    let mut fixture = TerminalFixture::new(16, 6);
    fixture.write(b"abc\x08X\rY\nZ\x0bV\x0cF\x00\x7f\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1c\x1d\x1e\x1f");
    assert_eq!(fixture.row(0), "YbX             ");
    assert_eq!(fixture.row(1), " Z              ");
    assert_eq!(fixture.row(2), "  V             ");
    assert_eq!(fixture.row(3), "   F            ");
    assert_eq!(fixture.cursor(), (4, 3));
}

#[test]
fn newline_mode_controls_whether_lf_returns_to_column_one() {
    let mut fixture = TerminalFixture::new(12, 4);
    fixture.write(b"abc\nX\x1b[20h\nY\x1b[20l\nZ");
    assert_eq!(fixture.row(1), "   X        ");
    assert_eq!(fixture.row(2), "Y           ");
    assert_eq!(fixture.row(3), " Z          ");
}

#[test]
fn cursor_motions_use_defaults_and_clip_to_screen_bounds() {
    for (sequence, expected) in [
        ("\x1b[A", (4, 1)),
        ("\x1b[0A", (4, 1)),
        ("\x1b[2B", (4, 4)),
        ("\x1b[3C", (7, 2)),
        ("\x1b[2D", (2, 2)),
        ("\x1b[2E", (0, 4)),
        ("\x1b[2F", (0, 0)),
        ("\x1b[3G", (2, 2)),
        ("\x1b[4d", (4, 3)),
        ("\x1b[2a", (6, 2)),
        ("\x1b[2e", (4, 4)),
        ("\x1b[0;0H", (0, 0)),
        ("\x1b[99;99f", (11, 5)),
        ("\x1b[999A", (4, 0)),
        ("\x1b[999B", (4, 5)),
        ("\x1b[999C", (11, 2)),
        ("\x1b[999D", (0, 2)),
    ] {
        let mut fixture = TerminalFixture::new(12, 6);
        fixture.write(b"\x1b[3;5H");
        fixture.write(sequence.as_bytes());
        assert_eq!(fixture.cursor(), expected, "{sequence:?}");
    }
}

#[test]
fn tab_stops_support_default_custom_clear_forward_and_backward() {
    let mut fixture = TerminalFixture::new(32, 4);
    fixture.write(b"A\tB\tC");
    assert_eq!(fixture.cursor(), (17, 0));
    assert_eq!(fixture.row(0), "A       B       C               ");
    fixture.write(b"\x1b[3g\r\x1b[5G\x1bH\r\tX");
    assert_eq!(fixture.cursor(), (5, 0));
    fixture.write(b"\x1b[9G\x1bH\r\x1b[2I");
    assert_eq!(fixture.cursor(), (8, 0));
    fixture.write(b"\x1b[Z");
    assert_eq!(fixture.cursor(), (4, 0));
    fixture.write(b"\x1b[g\r\t");
    assert_eq!(fixture.cursor(), (8, 0));
}

#[test]
fn autowrap_is_delayed_until_the_next_printable_character() {
    let mut fixture = TerminalFixture::new(5, 3);
    fixture.write(b"ABCDE");
    assert_eq!(fixture.cursor(), (4, 0));
    assert!(fixture.terminal.is_cursor_pending_wrap().unwrap());
    fixture.write(b"F");
    assert_eq!(fixture.row(0), "ABCDE");
    assert_eq!(fixture.row(1), "F    ");
    fixture.write(b"\x1b[H12345\rX");
    assert_eq!(fixture.row(0), "X2345");
    assert_eq!(fixture.cursor(), (1, 0));
    assert!(!fixture.terminal.is_cursor_pending_wrap().unwrap());
}

#[test]
fn disabling_autowrap_replaces_the_final_cell_without_scrolling() {
    let mut fixture = TerminalFixture::new(5, 3);
    fixture.write(b"\x1b[?7lABCDEFG");
    assert_eq!(fixture.row(0), "ABCDG");
    assert_eq!(fixture.row(1), "     ");
    assert_eq!(fixture.cursor(), (4, 0));
}

#[test]
fn save_restore_cursor_restores_position_sgr_and_dec_character_set() {
    let mut fixture = TerminalFixture::new(12, 4);
    fixture.write(b"\x1b[2;3H\x1b[1;31m\x1b(0\x1b7\x1b[4;9H\x1b[0m\x1b(BX\x1b8q");
    assert_eq!(fixture.cursor(), (3, 1));
    assert_eq!(fixture.frame().cells[14].text, "─");
    let style = fixture.style(2, 1);
    assert!(style.bold);
    assert_eq!(
        style.fg_color,
        StyleColor::Palette(libghostty_vt::style::PaletteIndex(1))
    );
}

#[test]
fn ansi_save_restore_cursor_works_when_horizontal_margins_are_disabled() {
    let mut fixture = TerminalFixture::new(12, 4);
    fixture.write(b"\x1b[2;3H\x1b[s\x1b[4;8H\x1b[uX");
    assert_eq!(fixture.cursor(), (3, 1));
    assert_eq!(fixture.frame().cells[14].text, "X");
}

#[test]
fn dec_line_drawing_and_shift_in_out_keep_character_set_designations() {
    let mut fixture = TerminalFixture::new(16, 4);
    fixture.write(b"\x1b)0A\x0elqkx\x0fZ\x1b(0mqj\x1b(B");
    assert_eq!(fixture.row(0).trim_end(), "A┌─┐│Z└─┘");
}

#[test]
fn origin_mode_addresses_and_clips_within_scrolling_margins() {
    let mut fixture = TerminalFixture::new(12, 6);
    fixture.write(b"\x1b[2;5r\x1b[?6h");
    assert_eq!(fixture.cursor(), (0, 1));
    fixture.write(b"\x1b[2;3HX");
    assert_eq!(fixture.cursor(), (3, 2));
    assert_eq!(fixture.frame().cells[2 * 12 + 2].text, "X");
    fixture.write(b"\x1b[99;99H");
    assert_eq!(fixture.cursor(), (11, 4));
    fixture.write(b"\x1b[?6l");
    assert_eq!(fixture.cursor(), (0, 0));
}

#[test]
fn index_reverse_index_and_next_line_scroll_only_the_selected_region() {
    let mut fixture = TerminalFixture::new(8, 6);
    fixture.write(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\x1b[2;5r\x1b[5;1H\x1bD");
    assert_eq!(
        (0..6)
            .map(|row| fixture.row(row).trim_end().to_owned())
            .collect::<Vec<_>>(),
        ["1", "3", "4", "5", "", "6"]
    );
    fixture.write(b"\x1b[2;1H\x1bM");
    assert_eq!(
        (0..6)
            .map(|row| fixture.row(row).trim_end().to_owned())
            .collect::<Vec<_>>(),
        ["1", "", "3", "4", "5", "6"]
    );
    fixture.write(b"\x1b[3;4H\x1bE");
    assert_eq!(fixture.cursor(), (0, 3));
}

#[test]
fn insert_and_delete_characters_shift_cells_but_erase_does_not() {
    for (command, expected) in [
        ("\x1b[2@", "ab  cdef"),
        ("\x1b[2P", "abefgh  "),
        ("\x1b[2X", "ab  efgh"),
    ] {
        let mut fixture = TerminalFixture::new(8, 3);
        fixture.write(b"abcdefgh\x1b[3G");
        fixture.write(command.as_bytes());
        assert_eq!(fixture.row(0), expected, "{command:?}");
        assert_eq!(fixture.cursor(), (2, 0));
    }
}

#[test]
fn insert_mode_and_replacement_mode_preserve_expected_cells() {
    let mut fixture = TerminalFixture::new(8, 3);
    fixture.write(b"abcdefgh\x1b[3G\x1b[4hXY\x1b[4lZ");
    assert_eq!(fixture.row(0), "abXYZdef");
    assert_eq!(fixture.cursor(), (5, 0));
}

#[test]
fn insert_delete_and_scroll_lines_preserve_rows_outside_the_region() {
    for (command, expected) in [
        ("\x1b[2L", ["1", "2", "", "", "3", "6"]),
        ("\x1b[2M", ["1", "2", "5", "", "", "6"]),
        ("\x1b[S", ["1", "3", "4", "5", "", "6"]),
        ("\x1b[T", ["1", "", "2", "3", "4", "6"]),
    ] {
        let mut fixture = TerminalFixture::new(8, 6);
        fixture.write(b"1\r\n2\r\n3\r\n4\r\n5\r\n6\x1b[2;5r\x1b[3;1H");
        fixture.write(command.as_bytes());
        let actual = (0..6)
            .map(|row| fixture.row(row).trim_end().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{command:?}");
    }
}

#[test]
fn horizontal_margins_bound_origin_and_wrapping() {
    let mut fixture = TerminalFixture::new(10, 5);
    fixture.write(b"\x1b[?69h\x1b[3;7s\x1b[?6hABCDEZ");
    assert_eq!(fixture.row(0), "  ABCDE   ");
    assert_eq!(fixture.row(1), "  Z       ");
    assert_eq!(fixture.cursor(), (3, 1));
    fixture.write(b"\x1b[99;99H");
    assert_eq!(fixture.cursor(), (6, 4));
}

#[test]
fn erase_line_modes_erase_the_correct_inclusive_range() {
    for (command, expected) in [
        ("\x1b[K", "abc     "),
        ("\x1b[1K", "    efgh"),
        ("\x1b[2K", "        "),
    ] {
        let mut fixture = TerminalFixture::new(8, 3);
        fixture.write(b"abcdefgh\x1b[4G");
        fixture.write(command.as_bytes());
        assert_eq!(fixture.row(0), expected, "{command:?}");
        assert_eq!(fixture.cursor(), (3, 0));
    }
}

#[test]
fn erase_display_modes_preserve_cursor_and_respect_each_side() {
    for (command, expected) in [
        ("\x1b[J", ["1111", "22  ", "    "]),
        ("\x1b[1J", ["    ", "   2", "3333"]),
        ("\x1b[2J", ["    ", "    ", "    "]),
    ] {
        let mut fixture = TerminalFixture::new(4, 3);
        fixture.write(b"1111\r\n2222\r\n3333\x1b[2;3H");
        fixture.write(command.as_bytes());
        assert_eq!(
            (0..3).map(|row| fixture.row(row)).collect::<Vec<_>>(),
            expected,
            "{command:?}"
        );
        assert_eq!(fixture.cursor(), (2, 1));
    }
}

#[test]
fn selective_erasure_preserves_protected_cells_but_normal_erasure_clears_them() {
    let mut fixture = TerminalFixture::new(12, 3);
    fixture.write(b"plain\x1b[1\"qKEEP\x1b[0\"qend\r\x1b[?2K");
    assert_eq!(fixture.row(0), "     KEEP   ");
    let protected = fixture
        .terminal
        .grid_ref(Point::Viewport(PointCoordinate { x: 5, y: 0 }))
        .unwrap();
    assert!(protected.cell().unwrap().is_protected().unwrap());
    fixture.write(b"\x1b[2K");
    assert_eq!(fixture.row(0), "            ");
}

#[test]
fn bce_applies_to_erase_insert_delete_and_scrolled_blank_cells() {
    for command in ["\x1b[2K", "\x1b[2X", "\x1b[2@", "\x1b[2P", "\x1b[2J"] {
        let mut fixture = TerminalFixture::new(8, 3);
        fixture.write(b"abcdefgh\x1b[3G\x1b[48;2;12;34;56m");
        fixture.write(command.as_bytes());
        let frame = fixture.frame();
        assert!(
            frame
                .cells
                .iter()
                .filter(|cell| cell.text.is_empty())
                .any(|cell| cell.background == Some(Rgb::from(Color::new(12, 34, 56)))),
            "{command:?}"
        );
    }
    let mut fixture = TerminalFixture::new(8, 3);
    fixture.write(b"\x1b[48;2;12;34;56m\x1b[3;1H\n");
    let frame = fixture.frame();
    assert!(
        frame.cells[16..]
            .iter()
            .all(|cell| cell.background == Some(Rgb::from(Color::new(12, 34, 56))))
    );
}

#[test]
fn sgr_extended_attributes_and_independent_resets_survive_the_grid() {
    let mut fixture = TerminalFixture::new(12, 3);
    fixture.write(b"\x1b[1;2;3;4:3;5;7;8;9;53;58:2::12:34:56mA\x1b[22;23;24;25;27;28;29;55;59mB");
    let first = fixture.style(0, 0);
    assert!(
        first.bold
            && first.faint
            && first.italic
            && first.blink
            && first.inverse
            && first.invisible
            && first.strikethrough
            && first.overline
    );
    assert_eq!(first.underline, Underline::Curly);
    assert_eq!(
        first.underline_color,
        StyleColor::Rgb(RgbColor {
            r: 12,
            g: 34,
            b: 56
        })
    );
    assert!(fixture.style(1, 0).is_default());
}

#[test]
fn semicolon_and_colon_color_encodings_select_the_same_rgb() {
    let mut fixture = TerminalFixture::new(12, 3);
    fixture.write(b"\x1b[38;2;12;34;56;48;5;17mA\x1b[38:2::12:34:56;48:5:17mB");
    assert_eq!(fixture.style(0, 0), fixture.style(1, 0));
}

#[test]
fn alternate_screen_and_1048_save_restore_do_not_corrupt_primary_history() {
    let mut fixture = TerminalFixture::new(12, 4);
    fixture.write(b"main\x1b[?1048h\x1b[3;7H\x1b[?1048l");
    assert_eq!(fixture.cursor(), (4, 0));
    fixture.write(b"\x1b[?1049hALT\r\nother\x1b[?1049l");
    assert_eq!(fixture.row(0), "main        ");
    assert_eq!(fixture.row(1), "            ");
    assert_eq!(fixture.cursor(), (4, 0));
    fixture.write(b"\x1b[?1049h");
    assert_eq!(fixture.row(0), "            ");
}

#[test]
fn erasing_scrollback_does_not_erase_the_visible_screen() {
    let mut fixture = TerminalFixture::new(8, 3);
    for number in 0..8 {
        fixture.write(format!("L{number}\r\n").as_bytes());
    }
    let before = fixture.frame();
    assert!(fixture.terminal.scrollback_rows().unwrap() > 0);
    fixture.write(b"\x1b[3J");
    let after = fixture.frame();
    assert_eq!(fixture.terminal.scrollback_rows().unwrap(), 0);
    assert_eq!(before.cells, after.cells);
    fixture.terminal.scroll_viewport(ScrollViewport::Top);
    assert_eq!(fixture.frame().cells, after.cells);
}

#[test]
fn device_status_and_mode_reports_have_exact_framing_and_coordinates() {
    let mut fixture = TerminalFixture::new(80, 24);
    assert_eq!(
        fixture.reply(b"\x1b[7;13H\x1b[5n\x1b[6n"),
        b"\x1b[0n\x1b[7;13R"
    );
    assert_eq!(
        fixture.reply(b"\x1b[?25$p\x1b[?25l\x1b[?25$p\x1b[?25h"),
        b"\x1b[?25;1$y\x1b[?25;2$y"
    );
    assert_eq!(
        fixture.reply(b"\x1b[4$p\x1b[4h\x1b[4$p\x1b[4l"),
        b"\x1b[4;2$y\x1b[4;1$y"
    );
    assert_eq!(fixture.reply(b"\x1b[?9999$p"), b"\x1b[?9999;0$y");
}

#[test]
fn origin_relative_cursor_reports_and_private_mode_save_restore_work() {
    let mut fixture = TerminalFixture::new(12, 6);
    fixture.write(b"\x1b[2;5r\x1b[?6h\x1b[2;3H");
    assert_eq!(fixture.reply(b"\x1b[6n"), b"\x1b[2;3R");
    fixture.write(b"\x1b[?7l\x1b[?7s\x1b[?7h\x1b[?7r");
    assert!(!fixture.terminal.mode(Mode::WRAPAROUND).unwrap());
}

#[test]
fn osc_titles_and_palette_queries_accept_bel_and_string_terminator() {
    let mut fixture = TerminalFixture::new(12, 4);
    fixture.write(b"\x1b]2;title with spaces\x07");
    assert_eq!(fixture.terminal.title().unwrap(), "title with spaces");
    fixture.write(b"\x1b]0;unicode-title-\xe6\xbc\xa2\x1b\\");
    assert_eq!(fixture.terminal.title().unwrap(), "unicode-title-漢");
    fixture.write(b"\x1b]4;1;#123456\x1b\\");
    assert_eq!(
        fixture.reply(b"\x1b]4;1;?\x07"),
        b"\x1b]4;1;rgb:1212/3434/5656\x07"
    );
    assert_eq!(
        fixture.reply(b"\x1b]4;1;?\x1b\\"),
        b"\x1b]4;1;rgb:1212/3434/5656\x1b\\"
    );
}

#[test]
fn soft_reset_resets_modes_and_attributes_without_erasing_text() {
    let mut fixture = TerminalFixture::new(12, 4);
    fixture.write(b"KEEP\x1b[1;31m\x1b[4h\x1b[?6h\x1b[?25l\x1b[!p");
    assert_eq!(fixture.row(0), "KEEP        ");
    assert!(!fixture.terminal.mode(Mode::INSERT).unwrap());
    assert!(!fixture.terminal.mode(Mode::ORIGIN).unwrap());
    assert!(fixture.terminal.is_cursor_visible().unwrap());
    assert!(fixture.terminal.cursor_style().unwrap().is_default());
}

#[test]
fn soft_reset_restores_configured_cursor_shape_and_blinking() {
    let mut fixture = TerminalFixture::new(12, 4);
    fixture.write(b"\x1b[2 q");
    assert_eq!(fixture.reply(b"\x1bP$q q\x1b\\"), b"\x1bP1$r2 q\x1b\\");
    fixture.write(b"\x1b[!p");
    let frame = fixture.frame();
    assert_eq!(frame.cursor.shape, fixture.config.cursor.shape);
    assert!(frame.cursor.blinking);
    assert_eq!(fixture.reply(b"\x1bP$q q\x1b\\"), b"\x1bP1$r5 q\x1b\\");
}

#[test]
fn soft_reset_keeps_cursor_and_custom_tabs_but_discards_saved_cursor_and_margins() {
    let mut fixture = TerminalFixture::new(20, 6);
    fixture.write(b"KEEP\x1b[3g\x1b[6G\x1bH\x1b[2;5r\x1b[?69h\x1b[4;18s\x1b[?6h\x1b[2;3H\x1b7\x1b[1;31m\x1b[1\"q\x1b(0");
    let cursor = fixture.cursor();
    fixture.write(b"\x1b[!p");
    assert_eq!(fixture.cursor(), cursor);
    assert!(!fixture.terminal.mode(Mode::LEFT_RIGHT_MARGIN).unwrap());
    fixture.write(b"\x1b8\tq");
    assert_eq!(fixture.cursor(), (6, 0));
    assert_eq!(fixture.frame().cells[5].text, "q");
    assert!(fixture.style(5, 0).is_default());
    assert!(
        !fixture
            .terminal
            .grid_ref(Point::Viewport(PointCoordinate { x: 5, y: 0 }))
            .unwrap()
            .cell()
            .unwrap()
            .is_protected()
            .unwrap()
    );
}

#[test]
fn reverse_video_uses_default_color_swap_and_sgr_inverse_can_reverse_it_again() {
    let mut fixture = TerminalFixture::new(12, 3);
    fixture.config.colors.foreground = Color::new(200, 210, 220);
    fixture.config.colors.background = Color::new(10, 20, 30);
    apply_theme(&mut fixture.terminal, &fixture.config).unwrap();
    fixture.write(b"A\x1b[?5hB\x1b[7mC\x1b[0m");
    let frame = fixture.frame();
    assert_eq!(
        frame.background,
        Rgb::from(fixture.config.colors.foreground)
    );
    assert_eq!(
        frame.foreground,
        Rgb::from(fixture.config.colors.background)
    );
    assert_eq!(frame.cells[1].foreground, frame.foreground);
    assert_eq!(
        frame.cells[2].foreground,
        Rgb::from(fixture.config.colors.foreground)
    );
    assert_eq!(
        frame.cells[2].background,
        Some(Rgb::from(fixture.config.colors.background))
    );
    fixture.write(b"\x1b[?5l");
    let restored = fixture.frame();
    assert_eq!(
        restored.background,
        Rgb::from(fixture.config.colors.background)
    );
    assert_eq!(
        restored.foreground,
        Rgb::from(fixture.config.colors.foreground)
    );
}

#[test]
fn full_reset_restores_tabs_modes_screen_and_default_attributes() {
    let mut fixture = TerminalFixture::new(20, 4);
    fixture.write(b"KEEP\x1b[3g\x1b[1;31m\x1b[4h\x1b[?7l\x1b[?25l\x1bc");
    assert_eq!(fixture.row(0), "                    ");
    assert_eq!(fixture.cursor(), (0, 0));
    assert!(!fixture.terminal.mode(Mode::INSERT).unwrap());
    assert!(fixture.terminal.mode(Mode::WRAPAROUND).unwrap());
    assert!(fixture.terminal.is_cursor_visible().unwrap());
    assert!(fixture.terminal.cursor_style().unwrap().is_default());
    fixture.write(b"\t");
    assert_eq!(fixture.cursor(), (8, 0));
}

#[test]
fn dec_alignment_fills_the_grid_and_restores_the_home_position() {
    let mut fixture = TerminalFixture::new(8, 4);
    fixture.write(b"\x1b[3;5H\x1b#8");
    let frame = fixture.frame();
    assert!(frame.cells.iter().all(|cell| cell.text == "E"));
    assert_eq!(fixture.cursor(), (0, 0));
}

#[test]
fn chunking_never_changes_mixed_unicode_controls_or_query_responses() {
    let input = "start\r\n漢e\u{301}🙂\x1b[38:2::12:34:56m色\x1b[0m\x1b[2;8H!\x1b[6n\x1b]2;chunked\x07\x1bPignored\x1b\\\x1b[4;2Hend";
    let mut baseline = TerminalFixture::new(24, 6);
    baseline.write(input.as_bytes());
    let expected = baseline.frame();
    let replies = baseline.replies.borrow().clone();
    for chunk_size in 1..=31 {
        let mut fixture = TerminalFixture::new(24, 6);
        for chunk in input.as_bytes().chunks(chunk_size) {
            fixture.write(chunk);
        }
        let actual = fixture.frame();
        assert_eq!(actual.cells, expected.cells, "chunk size {chunk_size}");
        assert_eq!(actual.cursor, expected.cursor, "chunk size {chunk_size}");
        assert_eq!(
            *fixture.replies.borrow(),
            replies,
            "chunk size {chunk_size}"
        );
        assert_eq!(fixture.terminal.title().unwrap(), "chunked");
    }
}

#[test]
fn dcs_status_queries_report_rendition_margins_protection_and_cursor_style() {
    let mut fixture = TerminalFixture::new(24, 6);
    assert_eq!(fixture.reply(b"\x1bP$qm\x1b\\"), b"\x1bP1$r0m\x1b\\");
    fixture.write(b"\x1b[1;3;4;31;48;2;12;34;56m\x1b[2;5r\x1b[?69h\x1b[4;18s\x1b[1\"q\x1b[6 q");
    for (query, response) in [
        (
            b"m".as_slice(),
            b"\x1bP1$r0;1;3;4;31;48:2::12:34:56m\x1b\\".as_slice(),
        ),
        (b"r", b"\x1bP1$r2;5r\x1b\\"),
        (b"s", b"\x1bP1$r4;18s\x1b\\"),
        (b"\"q", b"\x1bP1$r1\"q\x1b\\"),
        (b" q", b"\x1bP1$r6 q\x1b\\"),
        (b"bogus", b"\x1bP0$r\x1b\\"),
    ] {
        let request = [b"\x1bP$q".as_slice(), query, b"\x1b\\"].concat();
        assert_eq!(fixture.reply(&request), response, "query {query:?}");
    }
}

#[test]
fn dcs_termcap_queries_report_known_values_and_negative_responses_for_missing_keys() {
    let mut fixture = TerminalFixture::new(24, 6);
    assert_eq!(fixture.reply(b"\x1bP+q436f;524742;776973702d6e6f6e6578697374656e74\x1b\\"),
        b"\x1bP1+r436F=323536\x1b\\\x1bP1+r524742=38\x1b\\\x1bP0+r776973702D6E6F6E6578697374656E74\x1b\\");
}

#[test]
fn dcs_status_queries_are_chunk_safe_and_do_not_print_their_payload() {
    let input = b"before\x1b[31m\x1bP$qm\x1b\\after";
    for size in 1..=input.len() {
        let mut fixture = TerminalFixture::new(24, 6);
        for chunk in input.chunks(size) {
            fixture.write(chunk);
        }
        assert_eq!(
            fixture.row(0).trim_end(),
            "beforeafter",
            "chunk size {size}"
        );
        assert_eq!(
            fixture.replies.borrow().as_slice(),
            b"\x1bP1$r0;31m\x1b\\",
            "chunk size {size}"
        );
    }
}

#[test]
fn dcs_rendition_queries_preserve_extended_underline_and_decoration_colors() {
    let mut fixture = TerminalFixture::new(24, 6);
    fixture.write(b"\x1b[4:3;53;58:2::12:34:56m");
    assert_eq!(
        fixture.reply(b"\x1bP$qm\x1b\\"),
        b"\x1bP1$r0;4:3;53;58:2::12:34:56m\x1b\\"
    );
    fixture.write(b"\x1b[4:2;58:5:9m");
    assert_eq!(
        fixture.reply(b"\x1bP$qm\x1b\\"),
        b"\x1bP1$r0;4:2;53;58:5:9m\x1b\\"
    );
}
