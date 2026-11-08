use std::cell::{Cell, RefCell};
use std::env;
use std::io::{Read, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use libghostty_vt::fmt::Format;
use libghostty_vt::key;
use libghostty_vt::mouse;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::selection::{FormatOptions, Selection};
use libghostty_vt::style::{PaletteIndex, RgbColor};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, ScrollViewport};
use libghostty_vt::{RenderState, Terminal, TerminalOptions};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::config::{Color, Config, CursorShape};
use crate::geometry::GridGeometry;
use crate::graphics::placeholders::PlaceholderCell;
use crate::graphics::{GraphicsCapture, GraphicsSnapshot};
use crate::input::{InputResult, InputState, PointerEvent};
use crate::links::HoverState;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl From<Color> for Rgb {
    fn from(color: Color) -> Self {
        Self {
            red: color.red,
            green: color.green,
            blue: color.blue,
        }
    }
}

impl From<RgbColor> for Rgb {
    fn from(color: RgbColor) -> Self {
        Self {
            red: color.r,
            green: color.g,
            blue: color.b,
        }
    }
}

impl From<Rgb> for RgbColor {
    fn from(color: Rgb) -> Self {
        Self {
            r: color.red,
            g: color.green,
            b: color.blue,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellSnapshot {
    pub text: String,
    pub width: u8,
    pub foreground: Rgb,
    pub background: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub selected: bool,
}

impl Default for CellSnapshot {
    fn default() -> Self {
        Self {
            text: String::new(),
            width: 1,
            foreground: Rgb::default(),
            background: None,
            bold: false,
            italic: false,
            underline: false,
            selected: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorSnapshot {
    pub column: u16,
    pub row: u16,
    pub visible: bool,
    pub blinking: bool,
    pub shape: CursorShape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionBounds {
    pub start: PointCoordinate,
    pub end: PointCoordinate,
}

#[derive(Clone, Debug)]
pub struct TerminalSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<CellSnapshot>,
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor_color: Rgb,
    pub cursor: CursorSnapshot,
    pub mouse_reporting: bool,
    pub graphics: GraphicsSnapshot,
}

pub struct SnapshotBuilder {
    render_state: RenderState<'static>,
    row_iterator: RowIterator<'static>,
    cell_iterator: CellIterator<'static>,
    graphics: GraphicsCapture,
}

impl SnapshotBuilder {
    pub fn new() -> Result<Self, libghostty_vt::Error> {
        Ok(Self {
            render_state: RenderState::new()?,
            row_iterator: RowIterator::new()?,
            cell_iterator: CellIterator::new()?,
            graphics: GraphicsCapture::new()?,
        })
    }

    pub fn update(
        &mut self,
        terminal: &Terminal<'static, '_>,
        config: &Config,
    ) -> Result<TerminalSnapshot, libghostty_vt::Error> {
        self.graphics.advance(terminal)?;
        let pixels = terminal.pixel_size()?;
        capture_snapshot(
            terminal,
            &mut self.render_state,
            &mut self.row_iterator,
            &mut self.cell_iterator,
            &mut self.graphics,
            config,
            [
                pixels[0] / u32::from(terminal.cols()?.max(1)),
                pixels[1] / u32::from(terminal.rows()?.max(1)),
            ],
        )
    }

    pub fn write(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        config: &Config,
        data: &[u8],
        reply: &mut dyn FnMut(&[u8]),
    ) {
        self.graphics.write(terminal, config, data, reply);
    }
}

fn capture_snapshot(
    terminal: &Terminal<'static, '_>,
    render_state: &mut RenderState<'static>,
    row_iterator: &mut RowIterator<'static>,
    cell_iterator: &mut CellIterator<'static>,
    graphics: &mut GraphicsCapture,
    config: &Config,
    cell_size: [u32; 2],
) -> Result<TerminalSnapshot, libghostty_vt::Error> {
    let snapshot = render_state.update(terminal)?;
    let cols = snapshot.cols()?;
    let rows = snapshot.rows()?;
    let colors = snapshot.colors()?;
    let cursor_visible = snapshot.cursor_visible()?;
    let cursor_blinking = snapshot.cursor_blinking()?;
    let cursor_shape = match snapshot.cursor_visual_style()? {
        CursorVisualStyle::Bar => CursorShape::Beam,
        CursorVisualStyle::Block | CursorVisualStyle::BlockHollow => CursorShape::Block,
        CursorVisualStyle::Underline => CursorShape::Underline,
        _ => config.cursor.shape,
    };
    let cursor = snapshot.cursor_viewport()?.map(|position| CursorSnapshot {
        column: position.x,
        row: position.y,
        visible: cursor_visible,
        blinking: cursor_blinking,
        shape: cursor_shape,
    });
    let mut cells = Vec::with_capacity(usize::from(cols) * usize::from(rows));
    let mut placeholders = Vec::new();
    let mut rows_view = row_iterator.update(&snapshot)?;
    while let Some(row_view) = rows_view.next() {
        let mut cells_view = cell_iterator.update(row_view)?;
        while let Some(cell_view) = cells_view.next() {
            let mut text = String::new();
            cell_view.graphemes_utf8(&mut text)?;
            let width = match cell_view.raw_cell()?.wide()? {
                CellWide::Narrow => 1,
                CellWide::Wide => 2,
                CellWide::SpacerTail | CellWide::SpacerHead => 0,
            };
            let style = cell_view.style()?;
            if let Some(placeholder) = PlaceholderCell::from_text(
                (cells.len() % usize::from(cols)) as u16,
                (cells.len() / usize::from(cols)) as u16,
                &text,
                style.fg_color,
                style.underline_color,
            ) {
                placeholders.push(placeholder);
                text.clear();
            }
            let mut foreground = cell_view
                .fg_color()?
                .map(Rgb::from)
                .unwrap_or_else(|| colors.foreground.into());
            let mut background = cell_view.bg_color()?.map(Rgb::from);
            if style.inverse {
                let default_background: Rgb = colors.background.into();
                let old_foreground = foreground;
                foreground = background.unwrap_or(default_background);
                background = Some(old_foreground);
            }
            if style.faint {
                foreground.red /= 2;
                foreground.green /= 2;
                foreground.blue /= 2;
            }
            if style.invisible {
                foreground = background.unwrap_or_else(|| colors.background.into());
            }
            cells.push(CellSnapshot {
                text,
                width,
                foreground,
                background,
                bold: style.bold,
                italic: style.italic,
                underline: !matches!(style.underline, libghostty_vt::style::Underline::None),
                selected: cell_view.is_selected()?,
            });
        }
        row_view.set_dirty(false)?;
    }
    let cursor_color = snapshot
        .cursor_color()?
        .map(Rgb::from)
        .unwrap_or_else(|| config.colors.cursor.into());
    snapshot.set_dirty(libghostty_vt::render::Dirty::Clean)?;
    Ok(TerminalSnapshot {
        cols,
        rows,
        cells,
        background: colors.background.into(),
        foreground: colors.foreground.into(),
        cursor_color,
        cursor: cursor.unwrap_or(CursorSnapshot {
            visible: false,
            shape: cursor_shape,
            ..CursorSnapshot::default()
        }),
        mouse_reporting: terminal.is_mouse_tracking()?,
        graphics: graphics.update(terminal, config, &placeholders, cell_size)?,
    })
}

#[derive(Debug)]
pub enum SessionEvent {
    Clipboard(String),
    PrimarySelection(String),
    PastePrimary,
    Title(String),
    Exited,
    Error(String),
    Hover(HoverState),
    OpenUrl(String),
}

#[derive(Debug)]
pub enum Command {
    Output(Vec<u8>),
    Input(Vec<u8>),
    Paste(String),
    Geometry(GridGeometry),
    Hover {
        x: f64,
        y: f64,
        mods: key::Mods,
    },
    Leave,
    Resize {
        cols: u16,
        rows: u16,
        cell_width: u32,
        cell_height: u32,
        pixel_width: u32,
        pixel_height: u32,
    },
    Scroll(isize),
    Mouse {
        action: mouse::Action,
        button: Option<mouse::Button>,
        mods: key::Mods,
        x: f64,
        y: f64,
    },
    SelectionStart(PointCoordinate),
    SelectionUpdate(PointCoordinate),
    SelectionEnd(PointCoordinate),
    ClearSelection,
    CopySelection,
    Focus(bool),
    Key {
        key: key::Key,
        mods: key::Mods,
        action: key::Action,
        text: Option<String>,
        unshifted: Option<char>,
    },
    Shutdown,
}

pub struct Session {
    command_tx: SyncSender<Command>,
    snapshot_slot: Arc<Mutex<Option<TerminalSnapshot>>>,
    event_rx: Receiver<SessionEvent>,
    thread: Option<JoinHandle<()>>,
}

pub type SessionWake = Arc<dyn Fn() + Send + Sync>;

struct SnapshotSender {
    slot: Arc<Mutex<Option<TerminalSnapshot>>>,
    wake: SessionWake,
}

impl SnapshotSender {
    fn send(&self, snapshot: TerminalSnapshot) {
        crate::diagnostics::trace("snapshot-publish");
        let replaced = self.slot.lock().unwrap().replace(snapshot).is_some();
        if !replaced {
            (self.wake)();
        }
    }
}

#[derive(Clone)]
struct EventSender {
    sender: Sender<SessionEvent>,
    wake: SessionWake,
}

impl EventSender {
    fn send(&self, event: SessionEvent) -> Result<(), mpsc::SendError<SessionEvent>> {
        self.sender.send(event)?;
        (self.wake)();
        Ok(())
    }
}

impl Session {
    pub fn spawn(
        config: Config,
        cols: u16,
        rows: u16,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        Self::spawn_command(config, cols, rows, CommandBuilder::new(shell))
    }

    pub fn spawn_with_waker(
        config: Config,
        cols: u16,
        rows: u16,
        wake: SessionWake,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        Self::spawn_command_with_waker(config, cols, rows, CommandBuilder::new(shell), wake)
    }

    pub fn spawn_command(
        config: Config,
        cols: u16,
        rows: u16,
        command: CommandBuilder,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::spawn_command_with_waker(config, cols, rows, command, Arc::new(|| {}))
    }

    pub fn spawn_command_with_waker(
        config: Config,
        cols: u16,
        rows: u16,
        mut command: CommandBuilder,
        wake: SessionWake,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        command.env("TERM", "xterm-ghostty");
        command.env("COLORTERM", "truecolor");
        let mut child = pair.slave.spawn_command(command)?;
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let master = pair.master;
        let (command_tx, command_rx) = mpsc::sync_channel(64);
        let snapshot_slot = Arc::new(Mutex::new(None));
        let snapshot_tx = SnapshotSender {
            slot: snapshot_slot.clone(),
            wake: wake.clone(),
        };
        let (event_tx, event_rx) = mpsc::channel();
        let event_tx = EventSender {
            sender: event_tx,
            wake,
        };

        let output_tx = command_tx.clone();
        thread::Builder::new()
            .name("wisp-pty-reader".to_owned())
            .spawn(move || read_pty(reader, output_tx))?;

        let thread = thread::Builder::new()
            .name("wisp-vt".to_owned())
            .spawn(move || {
                if let Err(error) = run_terminal(
                    config,
                    cols,
                    rows,
                    master,
                    writer,
                    &mut child,
                    command_rx,
                    snapshot_tx,
                    event_tx.clone(),
                ) {
                    let _ = event_tx.send(SessionEvent::Error(error.to_string()));
                }
                let _ = event_tx.send(SessionEvent::Exited);
            })?;

        Ok(Self {
            command_tx,
            snapshot_slot,
            event_rx,
            thread: Some(thread),
        })
    }

    pub fn send(&self, command: Command) {
        let _ = self.command_tx.send(command);
    }

    pub fn try_snapshot(&self) -> Option<TerminalSnapshot> {
        self.snapshot_slot.lock().unwrap().take()
    }

    pub fn try_event(&self) -> Option<SessionEvent> {
        match self.event_rx.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.command_tx.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_pty(mut reader: Box<dyn Read + Send>, command_tx: SyncSender<Command>) {
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => {
                let _ = command_tx.send(Command::Shutdown);
                break;
            }
            Ok(length) => {
                if command_tx
                    .send(Command::Output(buffer[..length].to_vec()))
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

fn run_terminal(
    config: Config,
    cols: u16,
    rows: u16,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: &mut Box<dyn portable_pty::Child + Send + Sync>,
    command_rx: Receiver<Command>,
    snapshot_tx: SnapshotSender,
    event_tx: EventSender,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let scrollback = if config.scrollback_limit < 0 {
        usize::MAX
    } else {
        config.scrollback_limit as usize
    };
    let writer = RefCell::new(writer);
    let report_size = Cell::new(libghostty_vt::terminal::SizeReportSize {
        rows,
        columns: cols,
        cell_width: 8,
        cell_height: 18,
    });
    let mut terminal = Terminal::new(TerminalOptions {
        cols,
        rows,
        max_scrollback: scrollback,
    })?;

    apply_theme(&mut terminal, &config)?;
    terminal.on_size({
        let report_size = &report_size;
        move |_| Some(report_size.get())
    })?;
    terminal.on_pty_write({
        let writer = &writer;
        move |_terminal, data| {
            let _ = writer.borrow_mut().write_all(data);
            let _ = writer.borrow_mut().flush();
        }
    })?;
    terminal.on_title_changed({
        let event_tx = event_tx.clone();
        move |terminal| {
            if let Ok(title) = terminal.title() {
                let _ = event_tx.send(SessionEvent::Title(title.to_owned()));
            }
        }
    })?;

    let mut render_state = RenderState::new()?;
    let mut row_iterator = RowIterator::new()?;
    let mut cell_iterator = CellIterator::new()?;
    let graphics_capture = RefCell::new(GraphicsCapture::new()?);
    let mut input = InputState::new()?;
    let mut pixel_width = u32::from(cols) * 8;
    let mut pixel_height = u32::from(rows) * 18;
    let mut cell_width = 8_u32;
    let mut cell_height = 18_u32;
    let mut padding = (0, 0, 0, 0);
    let mut selection = None;
    let snapshot_pending = Cell::new(false);
    let snapshot_time = Cell::new(Instant::now() - Duration::from_millis(8));
    let synchronized_until = Cell::new(None);
    let interactive_echo_pending = Cell::new(false);

    let send_snapshot = |terminal: &Terminal<'static, '_>,
                         render_state: &mut RenderState<'static>,
                         row_iterator: &mut RowIterator<'static>,
                         cell_iterator: &mut CellIterator<'static>,
                         _selection: Option<SelectionBounds>,
                         config: &Config,
                         snapshot_tx: &SnapshotSender|
     -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        snapshot_pending.set(true);
        if terminal.mode(Mode::SYNC_OUTPUT)? {
            if synchronized_until.get().is_none() {
                synchronized_until.set(Some(Instant::now() + Duration::from_secs(1)));
            }
            return Ok(());
        }
        if synchronized_until.replace(None).is_some() {
            snapshot_time.set(Instant::now() - Duration::from_millis(8));
        }
        if snapshot_time.get().elapsed() < Duration::from_millis(8) {
            return Ok(());
        }
        let terminal_snapshot = capture_snapshot(
            terminal,
            render_state,
            row_iterator,
            cell_iterator,
            &mut graphics_capture.borrow_mut(),
            config,
            [report_size.get().cell_width, report_size.get().cell_height],
        )?;
        snapshot_tx.send(terminal_snapshot);
        snapshot_pending.set(false);
        snapshot_time.set(Instant::now());
        Ok(())
    };

    send_snapshot(
        &terminal,
        &mut render_state,
        &mut row_iterator,
        &mut cell_iterator,
        selection,
        &config,
        &snapshot_tx,
    )?;

    loop {
        if graphics_capture.borrow_mut().advance(&terminal)? {
            snapshot_pending.set(true);
        }
        if synchronized_until
            .get()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            terminal.set_mode(Mode::SYNC_OUTPUT, false)?;
        }
        if snapshot_pending.get() {
            send_snapshot(
                &terminal,
                &mut render_state,
                &mut row_iterator,
                &mut cell_iterator,
                selection,
                &config,
                &snapshot_tx,
            )?;
        }
        let size = mouse::EncoderSize {
            screen_width: pixel_width,
            screen_height: pixel_height,
            cell_width,
            cell_height,
            padding_top: padding.1,
            padding_bottom: padding.3,
            padding_right: padding.2,
            padding_left: padding.0,
        };
        let deadline = [
            snapshot_pending.get().then(|| {
                synchronized_until
                    .get()
                    .unwrap_or_else(|| snapshot_time.get() + Duration::from_millis(8))
            }),
            input.pending_link_deadline(),
            input.autoscroll_deadline(&terminal, size)?,
            graphics_capture.borrow().deadline(&terminal),
        ]
        .into_iter()
        .flatten()
        .min();
        let command = match deadline {
            Some(deadline) => {
                command_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            }
            None => command_rx
                .recv()
                .map_err(|_| RecvTimeoutError::Disconnected),
        };
        match command {
            Ok(Command::Output(data)) => {
                crate::diagnostics::trace("pty-output");
                graphics_capture
                    .borrow_mut()
                    .write(&mut terminal, &config, &data, &mut |reply| {
                        let _ = writer.borrow_mut().write_all(reply);
                    });
                let sync_generation = input.synchronized_output_generation();
                let result = input.observe_output(&terminal, &config, &data)?;
                if input.synchronized_output_generation() != sync_generation {
                    synchronized_until.set(Some(Instant::now() + Duration::from_secs(1)));
                }
                deliver_input_result(&writer, &event_tx, &config, result)?;
                if interactive_echo_pending.replace(false) {
                    snapshot_time.set(Instant::now() - Duration::from_millis(8));
                }
                send_snapshot(
                    &terminal,
                    &mut render_state,
                    &mut row_iterator,
                    &mut cell_iterator,
                    selection,
                    &config,
                    &snapshot_tx,
                )?;
            }
            Ok(Command::Input(data)) => {
                let result = input.text(&mut terminal, data)?;
                interactive_echo_pending.set(!result.bytes.is_empty());
                if deliver_input_result(&writer, &event_tx, &config, result)? {
                    send_snapshot(
                        &terminal,
                        &mut render_state,
                        &mut row_iterator,
                        &mut cell_iterator,
                        selection,
                        &config,
                        &snapshot_tx,
                    )?;
                }
            }
            Ok(Command::Paste(text)) => {
                let result = input.paste(&mut terminal, text, &config)?;
                interactive_echo_pending.set(!result.bytes.is_empty());
                if deliver_input_result(&writer, &event_tx, &config, result)? {
                    send_snapshot(
                        &terminal,
                        &mut render_state,
                        &mut row_iterator,
                        &mut cell_iterator,
                        selection,
                        &config,
                        &snapshot_tx,
                    )?;
                }
            }
            Ok(Command::Resize {
                cols,
                rows,
                cell_width: new_cell_width,
                cell_height: new_cell_height,
                pixel_width: new_pixel_width,
                pixel_height: new_pixel_height,
            }) => {
                cell_width = new_cell_width.max(1);
                cell_height = new_cell_height.max(1);
                pixel_width = new_pixel_width.max(1);
                pixel_height = new_pixel_height.max(1);
                padding = (0, 0, 0, 0);
                input.invalidate_links();
                master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: pixel_width as u16,
                    pixel_height: pixel_height as u16,
                })?;
                terminal.resize(cols, rows, cell_width, cell_height)?;
                report_size.set(libghostty_vt::terminal::SizeReportSize {
                    rows,
                    columns: cols,
                    cell_width,
                    cell_height,
                });
                send_snapshot(
                    &terminal,
                    &mut render_state,
                    &mut row_iterator,
                    &mut cell_iterator,
                    selection,
                    &config,
                    &snapshot_tx,
                )?;
            }
            Ok(Command::Geometry(geometry)) => {
                cell_width = geometry.cell_width;
                cell_height = geometry.cell_height;
                pixel_width = geometry.surface_width;
                pixel_height = geometry.surface_height;
                let size = geometry.mouse_size();
                padding = (
                    size.padding_left,
                    size.padding_top,
                    size.padding_right,
                    size.padding_bottom,
                );
                master.resize(PtySize {
                    rows: geometry.rows,
                    cols: geometry.cols,
                    pixel_width: geometry.content_width.min(u32::from(u16::MAX)) as u16,
                    pixel_height: geometry.content_height.min(u32::from(u16::MAX)) as u16,
                })?;
                terminal.resize(geometry.cols, geometry.rows, cell_width, cell_height)?;
                report_size.set(libghostty_vt::terminal::SizeReportSize {
                    rows: geometry.rows,
                    columns: geometry.cols,
                    cell_width,
                    cell_height,
                });
                input.invalidate_links();
                send_snapshot(
                    &terminal,
                    &mut render_state,
                    &mut row_iterator,
                    &mut cell_iterator,
                    selection,
                    &config,
                    &snapshot_tx,
                )?;
            }
            Ok(Command::Scroll(delta)) => {
                terminal.scroll_viewport(ScrollViewport::Delta(delta));
                input.invalidate_links();
                send_snapshot(
                    &terminal,
                    &mut render_state,
                    &mut row_iterator,
                    &mut cell_iterator,
                    selection,
                    &config,
                    &snapshot_tx,
                )?;
            }
            Ok(Command::Mouse {
                action,
                button,
                mods,
                x,
                y,
            }) => {
                let result = input.pointer(
                    &mut terminal,
                    &config,
                    mouse::EncoderSize {
                        screen_width: pixel_width,
                        screen_height: pixel_height,
                        cell_width,
                        cell_height,
                        padding_top: padding.1,
                        padding_bottom: padding.3,
                        padding_right: padding.2,
                        padding_left: padding.0,
                    },
                    PointerEvent {
                        action,
                        button,
                        mods,
                        x,
                        y,
                    },
                )?;
                if !result.bytes.is_empty() {
                    interactive_echo_pending.set(true);
                }
                if deliver_input_result(&writer, &event_tx, &config, result)? {
                    send_snapshot(
                        &terminal,
                        &mut render_state,
                        &mut row_iterator,
                        &mut cell_iterator,
                        selection,
                        &config,
                        &snapshot_tx,
                    )?;
                }
            }
            Ok(Command::Hover { x, y, mods }) => {
                if let Some(hover) = input.hover(
                    &terminal,
                    &config,
                    mouse::EncoderSize {
                        screen_width: pixel_width,
                        screen_height: pixel_height,
                        cell_width,
                        cell_height,
                        padding_top: padding.1,
                        padding_bottom: padding.3,
                        padding_right: padding.2,
                        padding_left: padding.0,
                    },
                    x,
                    y,
                    mods,
                )? {
                    let _ = event_tx.send(SessionEvent::Hover(hover));
                }
            }
            Ok(Command::Leave) => {
                if let Some(hover) = input.clear_hover() {
                    let _ = event_tx.send(SessionEvent::Hover(hover));
                }
            }
            Ok(Command::SelectionStart(point)) => {
                if mouse_tracking_enabled(&terminal)? {
                    continue;
                }
                selection = Some(SelectionBounds {
                    start: point,
                    end: point,
                });
                install_selection(&terminal, selection)?;
                send_snapshot(
                    &terminal,
                    &mut render_state,
                    &mut row_iterator,
                    &mut cell_iterator,
                    selection,
                    &config,
                    &snapshot_tx,
                )?;
            }
            Ok(Command::SelectionUpdate(point) | Command::SelectionEnd(point)) => {
                if mouse_tracking_enabled(&terminal)? {
                    selection = None;
                    install_selection(&terminal, None)?;
                    continue;
                }
                if let Some(bounds) = &mut selection {
                    bounds.end = point;
                    install_selection(&terminal, selection)?;
                    send_snapshot(
                        &terminal,
                        &mut render_state,
                        &mut row_iterator,
                        &mut cell_iterator,
                        selection,
                        &config,
                        &snapshot_tx,
                    )?;
                }
            }
            Ok(Command::ClearSelection) => {
                selection = None;
                input.clear_selection(&terminal)?;
                send_snapshot(
                    &terminal,
                    &mut render_state,
                    &mut row_iterator,
                    &mut cell_iterator,
                    selection,
                    &config,
                    &snapshot_tx,
                )?;
            }
            Ok(Command::CopySelection) => {
                if let Some(text) = input.selection_text(&terminal)? {
                    let _ = event_tx.send(SessionEvent::Clipboard(text));
                } else if let Some(bounds) = selection {
                    if let Some(text) = format_selection(&terminal, bounds)? {
                        let _ = event_tx.send(SessionEvent::Clipboard(text));
                    }
                }
            }
            Ok(Command::Focus(focused)) => {
                if config.protocols.focus_events {
                    let result = input.focus(&terminal, focused)?;
                    deliver_input_result(&writer, &event_tx, &config, result)?;
                }
            }
            Ok(Command::Key {
                key,
                mods,
                action,
                text,
                unshifted,
            }) => {
                crate::diagnostics::trace("key-encode");
                let result =
                    input.key(&mut terminal, &config, key, mods, action, text, unshifted)?;
                if !result.bytes.is_empty() && action != key::Action::Release {
                    interactive_echo_pending.set(true);
                }
                if deliver_input_result(&writer, &event_tx, &config, result)? {
                    send_snapshot(
                        &terminal,
                        &mut render_state,
                        &mut row_iterator,
                        &mut cell_iterator,
                        selection,
                        &config,
                        &snapshot_tx,
                    )?;
                }
            }
            Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Some(uri) = input.poll_link() {
                    let _ = event_tx.send(SessionEvent::OpenUrl(uri));
                }
                let result = input.autoscroll(
                    &mut terminal,
                    &config,
                    mouse::EncoderSize {
                        screen_width: pixel_width,
                        screen_height: pixel_height,
                        cell_width,
                        cell_height,
                        padding_top: padding.1,
                        padding_bottom: padding.3,
                        padding_right: padding.2,
                        padding_left: padding.0,
                    },
                )?;
                if deliver_input_result(&writer, &event_tx, &config, result)? {
                    send_snapshot(
                        &terminal,
                        &mut render_state,
                        &mut row_iterator,
                        &mut cell_iterator,
                        selection,
                        &config,
                        &snapshot_tx,
                    )?;
                }
            }
        }
    }

    Ok(())
}

pub fn apply_theme(
    terminal: &mut Terminal<'_, '_>,
    config: &Config,
) -> Result<(), libghostty_vt::Error> {
    crate::graphics::configure(terminal, config.protocols.kitty_graphics)?;
    terminal
        .set_default_fg_color(Some(Rgb::from(config.colors.foreground).into()))?
        .set_default_bg_color(Some(Rgb::from(config.colors.background).into()))?
        .set_default_cursor_color(Some(Rgb::from(config.colors.cursor).into()))?
        .set_default_cursor_style(Some(match config.cursor.shape {
            CursorShape::Block => libghostty_vt::terminal::CursorStyle::Block,
            CursorShape::Beam => libghostty_vt::terminal::CursorStyle::Bar,
            CursorShape::Underline => libghostty_vt::terminal::CursorStyle::Underline,
        }))?
        .set_default_cursor_blink(Some(config.cursor.blink))?;

    let mut palette = terminal.default_color_palette()?;
    for (index, color) in config.colors.ansi_palette().into_iter().enumerate() {
        palette.set(PaletteIndex(index as _), Rgb::from(color).into());
    }
    terminal.set_default_color_palette(Some(palette))?;
    Ok(())
}

fn write_to_pty(
    writer: &RefCell<Box<dyn Write + Send>>,
    data: &[u8],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    writer.borrow_mut().write_all(data)?;
    writer.borrow_mut().flush()?;
    Ok(())
}

fn deliver_input_result(
    writer: &RefCell<Box<dyn Write + Send>>,
    event_tx: &EventSender,
    config: &Config,
    result: InputResult,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    if !result.bytes.is_empty() {
        write_to_pty(writer, &result.bytes)?;
    }
    if let Some(text) = result.primary {
        if config.mouse.copy_on_select {
            let _ = event_tx.send(SessionEvent::Clipboard(text.clone()));
        }
        let _ = event_tx.send(SessionEvent::PrimarySelection(text));
    }
    if result.paste_primary {
        let _ = event_tx.send(SessionEvent::PastePrimary);
    }
    if let Some(hover) = result.hover {
        let _ = event_tx.send(SessionEvent::Hover(hover));
    }
    if let Some(uri) = result.open_url {
        let _ = event_tx.send(SessionEvent::OpenUrl(uri));
    }
    Ok(result.changed)
}

fn install_selection(
    terminal: &Terminal<'_, '_>,
    bounds: Option<SelectionBounds>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let Some(bounds) = bounds else {
        terminal.set_selection(None)?;
        return Ok(());
    };
    let start = terminal.grid_ref(Point::Viewport(clamp_point(terminal, bounds.start)?))?;
    let end = terminal.grid_ref(Point::Viewport(clamp_point(terminal, bounds.end)?))?;
    let selection = Selection::new(start, end, false);
    terminal.set_selection(Some(&selection))?;
    Ok(())
}

fn format_selection(
    terminal: &Terminal<'_, '_>,
    bounds: SelectionBounds,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    let start = terminal.grid_ref(Point::Viewport(clamp_point(terminal, bounds.start)?))?;
    let end = terminal.grid_ref(Point::Viewport(clamp_point(terminal, bounds.end)?))?;
    let selection = Selection::new(start, end, false);
    let mut buffer = vec![0_u8; 1024];
    loop {
        let options = FormatOptions::new()
            .with_selection(&selection)
            .with_emit_format(Format::Plain)
            .with_unwrap(true)
            .with_trim(true);
        match terminal.format_selection_buf(options, &mut buffer) {
            Ok(Some(length)) => {
                return Ok(Some(
                    String::from_utf8_lossy(&buffer[..length]).into_owned(),
                ));
            }
            Ok(None) => return Ok(None),
            Err(libghostty_vt::Error::OutOfSpace { required }) => buffer.resize(required, 0),
            Err(error) => return Err(error.into()),
        }
    }
}

fn clamp_point(
    terminal: &Terminal<'_, '_>,
    point: PointCoordinate,
) -> Result<PointCoordinate, Box<dyn std::error::Error + Send + Sync>> {
    let cols = terminal.cols()?.max(1);
    let rows = u32::from(terminal.rows()?.max(1));
    Ok(PointCoordinate {
        x: point.x.min(cols - 1),
        y: point.y.min(rows - 1),
    })
}

fn mouse_tracking_enabled(
    terminal: &Terminal<'_, '_>,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    Ok(terminal.mode(libghostty_vt::terminal::Mode::X10_MOUSE)?
        || terminal.mode(libghostty_vt::terminal::Mode::NORMAL_MOUSE)?
        || terminal.mode(libghostty_vt::terminal::Mode::BUTTON_MOUSE)?
        || terminal.mode(libghostty_vt::terminal::Mode::ANY_MOUSE)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::key::Encoder as KeyEncoder;
    use libghostty_vt::mouse::Encoder as MouseEncoder;

    fn terminal() -> Terminal<'static, 'static> {
        Terminal::new(TerminalOptions {
            cols: 80,
            rows: 24,
            max_scrollback: 256,
        })
        .expect("terminal should initialize")
    }

    #[test]
    fn mouse_encoder_is_silent_when_tracking_is_disabled() {
        let terminal = terminal();
        let mut encoder = MouseEncoder::new().expect("mouse encoder should initialize");
        encoder.set_options_from_terminal(&terminal);
        encoder.set_size(mouse::EncoderSize {
            screen_width: 640,
            screen_height: 480,
            cell_width: 8,
            cell_height: 20,
            padding_top: 0,
            padding_bottom: 0,
            padding_left: 0,
            padding_right: 0,
        });
        let mut event = mouse::Event::new().expect("mouse event should initialize");
        event
            .set_action(mouse::Action::Press)
            .set_button(Some(mouse::Button::Left))
            .set_position(mouse::Position { x: 16.0, y: 20.0 });
        let mut encoded = Vec::new();
        encoder
            .encode_to_vec(&event, &mut encoded)
            .expect("mouse event should encode");
        assert!(
            encoded.is_empty(),
            "ordinary programs must not receive mouse bytes"
        );
    }

    #[test]
    fn mouse_encoder_reports_after_terminal_enables_tracking() {
        let mut terminal = terminal();
        terminal.vt_write(b"\x1b[?1000h\x1b[?1006h");
        let mut encoder = MouseEncoder::new().expect("mouse encoder should initialize");
        encoder.set_options_from_terminal(&terminal);
        encoder.set_size(mouse::EncoderSize {
            screen_width: 640,
            screen_height: 480,
            cell_width: 8,
            cell_height: 20,
            padding_top: 0,
            padding_bottom: 0,
            padding_left: 0,
            padding_right: 0,
        });
        let mut event = mouse::Event::new().expect("mouse event should initialize");
        event
            .set_action(mouse::Action::Press)
            .set_button(Some(mouse::Button::Left))
            .set_position(mouse::Position { x: 16.0, y: 20.0 });
        let mut encoded = Vec::new();
        encoder
            .encode_to_vec(&event, &mut encoded)
            .expect("mouse event should encode");
        assert!(!encoded.is_empty());
        assert!(encoded.starts_with(b"\x1b[<"));
    }

    #[test]
    fn key_text_is_encoded_as_terminal_input() {
        let terminal = terminal();
        let mut encoder = KeyEncoder::new().expect("key encoder should initialize");
        encoder.set_options_from_terminal(&terminal);
        let mut event = key::Event::new().expect("key event should initialize");
        event
            .set_action(key::Action::Press)
            .set_key(key::Key::A)
            .set_mods(key::Mods::empty())
            .set_utf8(Some("a"));
        let mut encoded = Vec::new();
        encoder
            .encode_to_vec(&event, &mut encoded)
            .expect("key event should encode");
        assert_eq!(encoded, b"a");
    }
}
