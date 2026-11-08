use std::collections::VecDeque;
use std::process::{Command, Stdio};
use std::thread;

use libghostty_vt::screen::CellWide;
use libghostty_vt::terminal::{Point, PointCoordinate};
use libghostty_vt::{Error, Terminal};
use winit::window::CursorIcon;

const URL_PREFIXES: &[&str] = &[
    "file:",
    "ftp://",
    "ftps://",
    "gemini://",
    "git://",
    "gopher://",
    "http://",
    "https://",
    "irc://",
    "ircs://",
    "kitty:",
    "mailto:",
    "news:",
    "sftp://",
    "ssh://",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkSpan {
    pub row: u16,
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkTarget {
    pub uri: String,
    pub spans: Vec<LinkSpan>,
    pub explicit: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoverState {
    pub pointer: CursorIcon,
    pub link: Option<LinkTarget>,
}

impl Default for HoverState {
    fn default() -> Self {
        Self {
            pointer: CursorIcon::Text,
            link: None,
        }
    }
}

#[derive(Default)]
pub struct LinkCache {
    lines: Option<CachedLines>,
}

struct CachedLines {
    top: u16,
    bottom: u16,
    links: Vec<LinkTarget>,
}

struct MappedCell {
    start: usize,
    end: usize,
    row: u16,
    column: u16,
    width: u16,
    hyperlink: Option<String>,
}

impl LinkCache {
    pub fn invalidate(&mut self) {
        self.lines = None;
    }

    pub fn lookup(
        &mut self,
        terminal: &Terminal<'_, '_>,
        point: PointCoordinate,
    ) -> Result<Option<LinkTarget>, Error> {
        let row = u16::try_from(point.y).map_err(|_| Error::InvalidValue)?;
        if self
            .lines
            .as_ref()
            .is_none_or(|lines| row < lines.top || row > lines.bottom)
        {
            self.lines = Some(read_links(terminal, row)?);
        }
        Ok(self.lines.as_ref().and_then(|lines| {
            lines
                .links
                .iter()
                .find(|link| {
                    link.spans
                        .iter()
                        .any(|span| span.row == row && point.x >= span.start && point.x < span.end)
                })
                .cloned()
        }))
    }
}

fn read_links(terminal: &Terminal<'_, '_>, row: u16) -> Result<CachedLines, Error> {
    let columns = terminal.cols()?;
    let rows = terminal.rows()?;
    let row_wraps = |row| -> Result<bool, Error> {
        terminal
            .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: row }))?
            .row()?
            .is_wrapped()
    };
    let mut top = row;
    let mut bottom = row;
    while top > 0 && row - top < 3 && row_wraps(u32::from(top - 1))? {
        top -= 1;
    }
    while bottom + 1 < rows && bottom - row < 3 && row_wraps(u32::from(bottom))? {
        bottom += 1;
    }

    let mut text = String::new();
    let mut cells = Vec::with_capacity(usize::from(columns) * usize::from(bottom - top + 1));
    for line in top..=bottom {
        for column in 0..columns {
            let reference = terminal.grid_ref(Point::Viewport(PointCoordinate {
                x: column,
                y: u32::from(line),
            }))?;
            let cell = reference.cell()?;
            let width = match cell.wide()? {
                CellWide::Narrow => 1,
                CellWide::Wide => 2,
                CellWide::SpacerHead | CellWide::SpacerTail => continue,
            };
            let mut characters = ['\0'; 16];
            let characters = match reference.graphemes(&mut characters) {
                Ok(length) => characters[..length].to_vec(),
                Err(Error::OutOfSpace { required }) => {
                    let mut characters = vec!['\0'; required];
                    let length = reference.graphemes(&mut characters)?;
                    characters.truncate(length);
                    characters
                }
                Err(error) => return Err(error),
            };
            let start = text.len();
            if characters.is_empty() {
                text.push(' ');
            } else {
                text.extend(characters);
            }
            let hyperlink = if cell.has_hyperlink()? {
                let mut buffer = vec![0; 256];
                let length = loop {
                    match reference.hyperlink_uri(&mut buffer) {
                        Ok(length) => break length,
                        Err(Error::OutOfSpace { required }) => buffer.resize(required, 0),
                        Err(error) => return Err(error),
                    }
                };
                let uri = String::from_utf8_lossy(&buffer[..length]).into_owned();
                (!uri.is_empty()).then_some(uri)
            } else {
                None
            };
            cells.push(MappedCell {
                start,
                end: text.len(),
                row: line,
                column,
                width,
                hyperlink,
            });
        }
    }

    let mut links: Vec<LinkTarget> = Vec::new();
    for cell in &cells {
        if let Some(uri) = &cell.hyperlink {
            if let Some(link) = links
                .iter_mut()
                .find(|link| link.uri == *uri && link.explicit)
            {
                extend_spans(&mut link.spans, cell);
            } else {
                links.push(LinkTarget {
                    uri: uri.clone(),
                    spans: vec![LinkSpan {
                        row: cell.row,
                        start: cell.column,
                        end: cell.column + cell.width,
                    }],
                    explicit: true,
                });
            }
        }
    }
    for (start, end) in detect_urls(&text) {
        let mut spans = Vec::new();
        for cell in &cells {
            if cell.start < end && cell.end > start && cell.hyperlink.is_none() {
                extend_spans(&mut spans, cell);
            }
        }
        if !spans.is_empty() {
            links.push(LinkTarget {
                uri: text[start..end].to_owned(),
                spans,
                explicit: false,
            });
        }
    }
    Ok(CachedLines { top, bottom, links })
}

fn extend_spans(spans: &mut Vec<LinkSpan>, cell: &MappedCell) {
    if let Some(span) = spans.last_mut() {
        if span.row == cell.row && span.end == cell.column {
            span.end += cell.width;
            return;
        }
    }
    spans.push(LinkSpan {
        row: cell.row,
        start: cell.column,
        end: cell.column + cell.width,
    });
}

fn detect_urls(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        let rest = &text[offset..];
        let Some(prefix) = URL_PREFIXES
            .iter()
            .find(|prefix| rest.starts_with(**prefix))
        else {
            offset += rest.chars().next().map_or(1, char::len_utf8);
            continue;
        };
        if offset > 0
            && text[..offset]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_alphanumeric() || character == '_')
        {
            offset += prefix.len();
            continue;
        }
        let mut end = offset;
        for (index, character) in rest.char_indices() {
            if character.is_whitespace()
                || character.is_control()
                || matches!(character, '<' | '>' | '"' | '\'' | '`')
            {
                break;
            }
            end = offset + index + character.len_utf8();
        }
        loop {
            let candidate = &text[offset..end];
            let Some(last) = candidate.chars().next_back() else {
                break;
            };
            let unmatched = match last {
                ')' => candidate.matches(')').count() > candidate.matches('(').count(),
                ']' => candidate.matches(']').count() > candidate.matches('[').count(),
                '}' => candidate.matches('}').count() > candidate.matches('{').count(),
                _ => false,
            };
            if unmatched || matches!(last, '.' | ',' | ':' | ';' | '!' | '?') {
                end -= last.len_utf8();
            } else {
                break;
            }
        }
        if end > offset + prefix.len() {
            ranges.push((offset, end));
        }
        offset = end.max(offset + prefix.len());
    }
    ranges
}

pub fn clickable_modifiers(tracking: bool, mods: libghostty_vt::key::Mods) -> bool {
    use libghostty_vt::key::Mods;
    let mods = mods & (Mods::SHIFT | Mods::CTRL | Mods::ALT | Mods::SUPER);
    mods == (Mods::CTRL | Mods::SHIFT) || mods == Mods::SHIFT || (!tracking && mods.is_empty())
}

pub fn pointer_icon(
    tracking: bool,
    dragging: Option<bool>,
    link: bool,
    override_icon: Option<CursorIcon>,
) -> CursorIcon {
    if let Some(rectangle) = dragging {
        return if rectangle {
            CursorIcon::Crosshair
        } else {
            CursorIcon::Text
        };
    }
    if link {
        return CursorIcon::Pointer;
    }
    override_icon.unwrap_or(if tracking {
        CursorIcon::Default
    } else {
        CursorIcon::Text
    })
}

pub fn open_url(uri: &str) -> std::io::Result<()> {
    if uri.is_empty() || uri.chars().any(char::is_control) || uri.starts_with('-') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid link target",
        ));
    }
    let mut child = Command::new("xdg-open")
        .arg(uri)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[derive(Default)]
pub struct PointerShapes {
    stacks: [VecDeque<Option<CursorIcon>>; 2],
    state: EscapeState,
    buffer: Vec<u8>,
    screen: usize,
    sync_generation: u64,
}

#[derive(Default)]
enum EscapeState {
    #[default]
    Ground,
    Escape,
    OscPrefix,
    Osc,
    OscIgnore,
    OscEscape(bool),
    CsiStart,
    CsiPrivate {
        number: u16,
        alternate: bool,
        synchronized: bool,
    },
    CsiIgnore,
    StringIgnore,
    StringEscape,
}

impl PointerShapes {
    pub fn current(&self) -> Option<CursorIcon> {
        self.stacks[self.screen].back().copied().flatten()
    }

    pub fn sync_generation(&self) -> u64 {
        self.sync_generation
    }

    pub fn observe(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut replies = Vec::new();
        let mut remaining = bytes;
        while !remaining.is_empty() {
            match self.state {
                EscapeState::Ground => {
                    let Some(escape) = memchr::memchr(0x1b, remaining) else {
                        break;
                    };
                    remaining = &remaining[escape + 1..];
                    self.state = EscapeState::Escape;
                }
                EscapeState::Escape => {
                    let byte = remaining[0];
                    remaining = &remaining[1..];
                    match byte {
                        b']' => {
                            self.buffer.clear();
                            self.state = EscapeState::OscPrefix;
                        }
                        b'[' => self.state = EscapeState::CsiStart,
                        b'P' | b'_' | b'^' => self.state = EscapeState::StringIgnore,
                        b'c' => {
                            self.stacks.iter_mut().for_each(VecDeque::clear);
                            self.screen = 0;
                            self.state = EscapeState::Ground;
                        }
                        0x1b => {}
                        _ => self.state = EscapeState::Ground,
                    }
                }
                EscapeState::OscPrefix => {
                    let byte = remaining[0];
                    remaining = &remaining[1..];
                    if byte == 7 {
                        self.state = EscapeState::Ground;
                    } else if byte == 0x1b {
                        self.state = EscapeState::OscEscape(false);
                    } else if byte == b"22;"[self.buffer.len()] {
                        self.buffer.push(byte);
                        if self.buffer.len() == 3 {
                            self.state = EscapeState::Osc;
                        }
                    } else {
                        self.buffer.clear();
                        self.state = EscapeState::OscIgnore;
                    }
                }
                EscapeState::Osc => {
                    let end = memchr::memchr2(7, 0x1b, remaining).unwrap_or(remaining.len());
                    if self.buffer.len() + end <= 4096 {
                        self.buffer.extend_from_slice(&remaining[..end]);
                    } else {
                        self.buffer.clear();
                        self.state = EscapeState::OscIgnore;
                    }
                    remaining = &remaining[end..];
                    if let Some(terminator) = remaining.first() {
                        let capture = matches!(self.state, EscapeState::Osc);
                        if *terminator == 7 {
                            if capture {
                                self.finish_osc(&mut replies);
                            }
                            self.state = EscapeState::Ground;
                        } else {
                            self.state = EscapeState::OscEscape(capture);
                        }
                        remaining = &remaining[1..];
                    }
                }
                EscapeState::OscIgnore => {
                    let Some(end) = memchr::memchr2(7, 0x1b, remaining) else {
                        break;
                    };
                    self.state = if remaining[end] == 7 {
                        EscapeState::Ground
                    } else {
                        EscapeState::OscEscape(false)
                    };
                    remaining = &remaining[end + 1..];
                }
                EscapeState::OscEscape(capture) => {
                    if remaining[0] == b'\\' && capture {
                        self.finish_osc(&mut replies);
                    }
                    self.state = EscapeState::Ground;
                    remaining = &remaining[1..];
                }
                EscapeState::CsiStart => {
                    if remaining[0] == b'?' {
                        remaining = &remaining[1..];
                        self.state = EscapeState::CsiPrivate {
                            number: 0,
                            alternate: false,
                            synchronized: false,
                        };
                    } else {
                        self.state = EscapeState::CsiIgnore;
                    }
                }
                EscapeState::CsiPrivate {
                    number,
                    alternate,
                    synchronized,
                } => {
                    let mut number = number;
                    let mut alternate = alternate;
                    let mut synchronized = synchronized;
                    let mut consumed = 0;
                    let mut next_state = None;
                    for byte in remaining {
                        consumed += 1;
                        match byte {
                            b'0'..=b'9' => {
                                number = number
                                    .saturating_mul(10)
                                    .saturating_add(u16::from(byte - b'0'));
                            }
                            b';' => {
                                alternate |= matches!(number, 47 | 1047 | 1049);
                                synchronized |= number == 2026;
                                number = 0;
                            }
                            b'@'..=b'~' => {
                                if *byte == b'h' && (synchronized || number == 2026) {
                                    self.sync_generation = self.sync_generation.wrapping_add(1);
                                }
                                if alternate || matches!(number, 47 | 1047 | 1049) {
                                    match byte {
                                        b'h' => self.screen = 1,
                                        b'l' => self.screen = 0,
                                        _ => {}
                                    }
                                }
                                next_state = Some(EscapeState::Ground);
                                break;
                            }
                            0x1b => {
                                next_state = Some(EscapeState::Escape);
                                break;
                            }
                            _ => {
                                next_state = Some(EscapeState::CsiIgnore);
                                break;
                            }
                        }
                    }
                    self.state = next_state.unwrap_or(EscapeState::CsiPrivate {
                        number,
                        alternate,
                        synchronized,
                    });
                    remaining = &remaining[consumed..];
                }
                EscapeState::CsiIgnore => {
                    let Some(end) = remaining
                        .iter()
                        .position(|byte| (0x40..=0x7e).contains(byte) || *byte == 0x1b)
                    else {
                        break;
                    };
                    self.state = if remaining[end] == 0x1b {
                        EscapeState::Escape
                    } else {
                        EscapeState::Ground
                    };
                    remaining = &remaining[end + 1..];
                }
                EscapeState::StringIgnore => {
                    let Some(end) = memchr::memchr(0x1b, remaining) else {
                        break;
                    };
                    self.state = EscapeState::StringEscape;
                    remaining = &remaining[end + 1..];
                }
                EscapeState::StringEscape => {
                    self.state = match remaining[0] {
                        b'\\' => EscapeState::Ground,
                        0x1b => EscapeState::StringEscape,
                        _ => EscapeState::StringIgnore,
                    };
                    remaining = &remaining[1..];
                }
            }
        }
        replies
    }

    fn finish_osc(&mut self, replies: &mut Vec<u8>) {
        let Some(payload) = self.buffer.strip_prefix(b"22;") else {
            return;
        };
        let Ok(payload) = std::str::from_utf8(payload) else {
            return;
        };
        let operation = payload.chars().next().unwrap_or('=');
        let value = if !payload.is_empty() && matches!(operation, '=' | '>' | '<' | '?') {
            &payload[operation.len_utf8()..]
        } else {
            payload
        };
        match operation {
            '?' => {
                let result = value
                    .split(',')
                    .map(|name| match name {
                        "__current__" => self.current().map_or("0", |icon| icon.name()).to_owned(),
                        "__default__" => "text".to_owned(),
                        "__grabbed__" => "default".to_owned(),
                        name => if parse_shape(name).is_some() {
                            "1"
                        } else {
                            "0"
                        }
                        .to_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                replies.extend_from_slice(b"\x1b]22;");
                replies.extend_from_slice(result.as_bytes());
                replies.extend_from_slice(b"\x1b\\");
            }
            '<' => {
                self.stacks[self.screen].pop_back();
            }
            _ => {
                for name in value.split(',') {
                    let icon = if name.is_empty() {
                        Some(None)
                    } else {
                        parse_shape(name).map(Some)
                    };
                    if let Some(icon) = icon {
                        let stack = &mut self.stacks[self.screen];
                        if operation != '>' && !stack.is_empty() {
                            stack.pop_back();
                        }
                        if stack.len() == 16 {
                            stack.pop_front();
                        }
                        stack.push_back(icon);
                    }
                }
            }
        }
    }
}

fn parse_shape(name: &str) -> Option<CursorIcon> {
    let name = match name {
        "arrow" | "left_ptr" => "default",
        "beam" | "xterm" | "ibeam" => "text",
        "pointing_hand" | "hand2" | "hand" => "pointer",
        "question_arrow" | "whats_this" => "help",
        "clock" | "watch" => "wait",
        "half-busy" | "left_ptr_watch" => "progress",
        "tcross" => "crosshair",
        "plus" | "cross" => "cell",
        "fleur" | "pointer-move" => "move",
        "right_side" => "e-resize",
        "top_right_corner" => "ne-resize",
        "top_left_corner" => "nw-resize",
        "top_side" => "n-resize",
        "bottom_right_corner" => "se-resize",
        "bottom_left_corner" => "sw-resize",
        "bottom_side" => "s-resize",
        "left_side" => "w-resize",
        "sb_h_double_arrow" | "split_h" => "ew-resize",
        "sb_v_double_arrow" | "split_v" => "ns-resize",
        "size_bdiag" | "size-bdiag" => "nesw-resize",
        "size_fdiag" | "size-fdiag" => "nwse-resize",
        "zoom_in" => "zoom-in",
        "zoom_out" => "zoom-out",
        "dnd-link" => "alias",
        "dnd-copy" => "copy",
        "forbidden" | "crossed_circle" => "not-allowed",
        "dnd-no-drop" => "no-drop",
        "openhand" | "hand1" => "grab",
        "closedhand" | "dnd-none" => "grabbing",
        name => name,
    };
    name.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::TerminalOptions;

    fn terminal(columns: u16) -> Terminal<'static, 'static> {
        Terminal::new(TerminalOptions {
            cols: columns,
            rows: 8,
            max_scrollback: 1024 * 1024,
        })
        .unwrap()
    }

    #[test]
    fn url_detection_trims_sentence_punctuation_and_balances_parentheses() {
        let text =
            "See (https://example.com/a_(b)). or mailto:a@example.com, then file:///tmp/a.txt.";
        let found = detect_urls(text)
            .into_iter()
            .map(|(start, end)| &text[start..end])
            .collect::<Vec<_>>();
        assert_eq!(
            found,
            [
                "https://example.com/a_(b)",
                "mailto:a@example.com",
                "file:///tmp/a.txt"
            ]
        );
    }

    #[test]
    fn osc8_target_takes_precedence_over_displayed_url() {
        let mut terminal = terminal(80);
        terminal.vt_write(
            b"\x1b]8;;https://actual.example/path\x1b\\https://display.example\x1b]8;;\x1b\\",
        );
        let mut cache = LinkCache::default();
        let link = cache
            .lookup(&terminal, PointCoordinate { x: 10, y: 0 })
            .unwrap()
            .unwrap();
        assert_eq!(link.uri, "https://actual.example/path");
        assert!(link.explicit);
        assert_eq!(
            link.spans,
            [LinkSpan {
                row: 0,
                start: 0,
                end: 23
            }]
        );
    }

    #[test]
    fn plain_url_wraps_across_rows_and_wide_characters_preserve_columns() {
        let mut terminal = terminal(20);
        terminal.vt_write("漢 https://example.com/path".as_bytes());
        let mut cache = LinkCache::default();
        let link = cache
            .lookup(&terminal, PointCoordinate { x: 2, y: 1 })
            .unwrap()
            .unwrap();
        assert_eq!(link.uri, "https://example.com/path");
        assert_eq!(
            link.spans,
            [
                LinkSpan {
                    row: 0,
                    start: 3,
                    end: 20
                },
                LinkSpan {
                    row: 1,
                    start: 0,
                    end: 7
                }
            ]
        );
    }

    #[test]
    fn invalidation_replaces_cached_hover_after_output() {
        let mut terminal = terminal(80);
        let point = PointCoordinate { x: 4, y: 0 };
        let mut cache = LinkCache::default();
        terminal.vt_write(b"https://example.com");
        assert!(cache.lookup(&terminal, point).unwrap().is_some());
        terminal.vt_write(b"\r\x1b[2Kplain");
        cache.invalidate();
        assert!(cache.lookup(&terminal, point).unwrap().is_none());
    }

    #[test]
    fn kitty_modifiers_and_pointer_priority_match_default_mappings() {
        use libghostty_vt::key::Mods;
        assert!(clickable_modifiers(false, Mods::empty()));
        assert!(!clickable_modifiers(true, Mods::empty()));
        assert!(clickable_modifiers(true, Mods::SHIFT));
        assert!(clickable_modifiers(true, Mods::CTRL | Mods::SHIFT));
        assert!(!clickable_modifiers(false, Mods::CTRL));
        assert!(!clickable_modifiers(false, Mods::ALT | Mods::SHIFT));
        assert_eq!(pointer_icon(false, None, false, None), CursorIcon::Text);
        assert_eq!(pointer_icon(true, None, false, None), CursorIcon::Default);
        assert_eq!(pointer_icon(true, None, true, None), CursorIcon::Pointer);
        assert_eq!(
            pointer_icon(false, Some(true), true, None),
            CursorIcon::Crosshair
        );
        assert_eq!(
            pointer_icon(true, None, false, Some(CursorIcon::Wait)),
            CursorIcon::Wait
        );
    }

    #[test]
    fn osc22_shapes_are_chunk_safe_and_separate_for_alternate_screen() {
        let mut shapes = PointerShapes::default();
        shapes.observe(b"\x1b]22;point");
        shapes.observe(b"er\x1b\\");
        assert_eq!(shapes.current(), Some(CursorIcon::Pointer));
        shapes.observe(b"\x1b[?1049h\x1b]22;>wait\x1b\\");
        assert_eq!(shapes.current(), Some(CursorIcon::Wait));
        shapes.observe(b"\x1b[?1049l");
        assert_eq!(shapes.current(), Some(CursorIcon::Pointer));
        assert_eq!(
            shapes.observe(b"\x1b]22;?__current__,__default__,__grabbed__,nope\x1b\\"),
            b"\x1b]22;pointer,text,default,0\x1b\\"
        );
        shapes.observe(b"\x1b]22;<\x1b\\");
        assert_eq!(shapes.current(), None);
        shapes.observe(b"\x1b]22;crosshair\x07\x1bc");
        assert_eq!(shapes.current(), None);
    }

    #[test]
    fn setting_pointer_shape_replaces_stack_top_and_empty_resets_current_only() {
        let mut shapes = PointerShapes::default();
        shapes.observe(b"\x1b]22;>pointer,wait\x1b\\\x1b]22;crosshair\x1b\\");
        assert_eq!(shapes.current(), Some(CursorIcon::Crosshair));
        shapes.observe(b"\x1b]22;<\x1b\\");
        assert_eq!(shapes.current(), Some(CursorIcon::Pointer));
        shapes.observe(b"\x1b]22;>wait\x1b\\\x1b]22;\x1b\\");
        assert_eq!(shapes.current(), None);
        shapes.observe(b"\x1b]22;<\x1b\\");
        assert_eq!(shapes.current(), Some(CursorIcon::Pointer));
        assert_eq!(
            shapes.observe(b"\x1b]22;?beam,arrow,hand2\x1b\\"),
            b"\x1b]22;1,1,1\x1b\\"
        );
    }

    #[test]
    fn pointer_observer_preserves_every_split_boundary_for_osc_and_private_csi() {
        let data = b"text\x1b]22;pointer\x1b\\\x1b[38;2;10;20;30mcolor\x1b[?2026h\x1b[?1;1049;25h\x1b]22;wait\x07\x1b]8;;https://example.org\x1b\\link\x1b]8;;\x1b\\\x1b[?1049;1l\x1b[?2026l\x1b]22;?__current__\x1b\\";
        let expected = b"\x1b]22;pointer\x1b\\";
        for split in 0..=data.len() {
            let mut shapes = PointerShapes::default();
            let mut replies = shapes.observe(&data[..split]);
            replies.extend(shapes.observe(&data[split..]));
            assert_eq!(replies, expected, "split {split}");
            assert_eq!(shapes.current(), Some(CursorIcon::Pointer), "split {split}");
            shapes.observe(b"\x1b[?1049h");
            assert_eq!(shapes.current(), Some(CursorIcon::Wait), "split {split}");
        }
        for chunk_size in 1..=32 {
            let mut shapes = PointerShapes::default();
            let replies = data
                .chunks(chunk_size)
                .flat_map(|chunk| shapes.observe(chunk))
                .collect::<Vec<_>>();
            assert_eq!(replies, expected, "chunk size {chunk_size}");
        }
    }

    #[test]
    fn unrelated_long_osc_never_buffers_payload_and_handles_split_terminators() {
        for terminator in [b"\x07".as_slice(), b"\x1b\\"] {
            let mut shapes = PointerShapes::default();
            shapes.observe(b"\x1b]0;");
            assert!(shapes.buffer.is_empty());
            let payload = vec![b'a'; 16 * 1024];
            for _ in 0..64 {
                assert!(shapes.observe(&payload).is_empty());
                assert!(shapes.buffer.is_empty());
            }
            for byte in terminator {
                assert!(shapes.observe(&[*byte]).is_empty());
            }
            shapes.observe(b"\x1b]22;wait\x1b\\");
            assert_eq!(shapes.current(), Some(CursorIcon::Wait));
        }
    }

    #[test]
    fn only_exact_osc22_prefix_is_interpreted_and_oversize_messages_are_discarded() {
        let mut shapes = PointerShapes::default();
        shapes.observe(b"\x1b]22;pointer\x1b\\");
        for prefix in [b"2;".as_slice(), b"220;", b"222;", b"22:", b"2222;"] {
            shapes.observe(b"\x1b]");
            shapes.observe(prefix);
            shapes.observe(b"wait\x1b\\");
            assert_eq!(shapes.current(), Some(CursorIcon::Pointer));
        }
        shapes.observe(b"\x1b]22;wait,");
        shapes.observe(&vec![b'a'; 8192]);
        shapes.observe(b"\x1b\\");
        assert_eq!(shapes.current(), Some(CursorIcon::Pointer));
        shapes.observe(b"\x1b]22;crosshair\x07");
        assert_eq!(shapes.current(), Some(CursorIcon::Crosshair));
    }

    #[test]
    fn ignored_dcs_apc_and_pm_payloads_cannot_change_pointer_shapes() {
        for introducer in [b'P', b'_', b'^'] {
            let mut shapes = PointerShapes::default();
            shapes.observe(b"\x1b]22;pointer\x1b\\");
            shapes.observe(&[0x1b, introducer]);
            shapes.observe(b"payload\x1b]22;wait\x07\x1b[?1049h");
            shapes.observe(b"\x1b");
            shapes.observe(b"\\");
            assert_eq!(shapes.current(), Some(CursorIcon::Pointer));
            assert!(
                shapes
                    .observe(b"\x1b]22;?__current__\x1b\\")
                    .ends_with(b"pointer\x1b\\")
            );
        }
    }

    #[test]
    fn synchronized_output_generation_counts_each_decset_across_chunk_boundaries() {
        let bytes = b"\x1b[?2026h\x1b[?2026h\x1b[?1049;2026;25h\x1b[?2026;1049h\x1b[?2026l\x1b[2026h\x1b[?2026$p\x1bc\x1b[?25;2026h";
        for split in 0..=bytes.len() {
            let mut shapes = PointerShapes::default();
            shapes.observe(&bytes[..split]);
            shapes.observe(&bytes[split..]);
            assert_eq!(shapes.sync_generation(), 5, "split {split}");
        }
        for chunk_size in 1..=32 {
            let mut shapes = PointerShapes::default();
            for chunk in bytes.chunks(chunk_size) {
                shapes.observe(chunk);
            }
            assert_eq!(shapes.sync_generation(), 5, "chunk size {chunk_size}");
        }
    }

    #[test]
    fn synchronized_output_generation_ignores_string_payloads_and_counts_duplicate_parameters_once()
    {
        let mut shapes = PointerShapes::default();
        for introducer in [b']', b'P', b'_', b'^'] {
            shapes.observe(&[0x1b, introducer]);
            shapes.observe(b"0;payload\x1b[?2026h");
            shapes.observe(b"\x1b");
            shapes.observe(b"\\");
            assert_eq!(shapes.sync_generation(), 0, "introducer {introducer}");
        }
        shapes.observe(b"\x1b[?2026;2026h");
        assert_eq!(shapes.sync_generation(), 1);
        shapes.observe(b"\x1bc");
        assert_eq!(shapes.sync_generation(), 1);
        shapes.observe(b"\x1b[?2026h");
        assert_eq!(shapes.sync_generation(), 2);
    }
}
