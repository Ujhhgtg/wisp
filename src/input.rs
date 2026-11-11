use std::time::{Duration, Instant};

use libghostty_vt::key::{self, KittyKeyFlags};
use libghostty_vt::mouse::{self, EncoderSize};
use libghostty_vt::screen::{Screen, TrackedGridRef};
use libghostty_vt::selection::gesture::{DragEvent, Geometry, Gesture, PressEvent, ReleaseEvent};
use libghostty_vt::selection::{FormatOptions, Selection};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, PointSpace, ScrollViewport};
use libghostty_vt::{Error, Terminal};

use crate::config::Config;
use crate::links::{
    HoverState, LinkCache, LinkTarget, PointerShapes, clickable_modifiers, pointer_icon,
};

const CLICK_INTERVAL: Duration = Duration::from_millis(500);

const KITTY_WORD_BOUNDARIES: &[char] = &[
    ' ', '\t', '\r', '\n', '\u{000b}', '\u{000c}', '!', '"', '$', '\'', '(', ')', '*', ',', ':',
    ';', '<', '>', '[', '\\', ']', '^', '`', '{', '|', '}',
];

#[derive(Clone, Copy, Debug)]
pub struct PointerEvent {
    pub action: mouse::Action,
    pub button: Option<mouse::Button>,
    pub mods: key::Mods,
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Debug, Default)]
pub struct InputResult {
    pub bytes: Vec<u8>,
    pub changed: bool,
    pub primary: Option<String>,
    pub paste_primary: bool,
    pub hover: Option<HoverState>,
    pub open_url: Option<String>,
}

pub struct InputState {
    key_encoder: key::Encoder<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    gesture: Gesture<'static>,
    selection: Option<TrackedSelection>,
    drag: Option<Drag>,
    app_button: Option<mouse::Button>,
    pressed_at: Instant,
    last_pointer: Option<PointerEvent>,
    last_autoscroll: Duration,
    links: LinkCache,
    pointer_shapes: PointerShapes,
    hover: HoverState,
    link_press: Option<LinkPress>,
    pending_link: Option<(Duration, String)>,
    last_hover_query: Option<HoverQuery>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct HoverQuery {
    point: Option<PointCoordinate>,
    modifiers: key::Mods,
    tracking: bool,
    dragging: Option<bool>,
    icon: Option<winit::window::CursorIcon>,
    hyperlinks: bool,
}

struct LinkPress {
    target: Option<LinkTarget>,
    point: PointCoordinate,
    time: Duration,
    immediate: bool,
}

struct TrackedSelection {
    start: TrackedGridRef,
    end: TrackedGridRef,
    rectangle: bool,
}

#[derive(Clone, Copy)]
enum Drag {
    Select {
        button: mouse::Button,
        rectangle: bool,
    },
    Extend {
        button: mouse::Button,
        start: bool,
    },
}

impl InputState {
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            key_encoder: key::Encoder::new()?,
            mouse_encoder: mouse::Encoder::new()?,
            gesture: Gesture::new()?,
            selection: None,
            drag: None,
            app_button: None,
            pressed_at: Instant::now(),
            last_pointer: None,
            last_autoscroll: Duration::ZERO,
            links: LinkCache::default(),
            pointer_shapes: PointerShapes::default(),
            hover: HoverState::default(),
            link_press: None,
            pending_link: None,
            last_hover_query: None,
        })
    }

    pub fn key(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        config: &Config,
        key: key::Key,
        mods: key::Mods,
        action: key::Action,
        text: Option<String>,
        unshifted: Option<char>,
    ) -> Result<InputResult, Error> {
        if terminal.mode(Mode::KAM)? {
            return Ok(InputResult::default());
        }
        let legacy =
            !config.protocols.kitty_keyboard || terminal.kitty_keyboard_flags()?.is_empty();
        self.key_encoder.set_options_from_terminal(terminal);
        if !config.protocols.kitty_keyboard {
            self.key_encoder.set_kitty_flags(KittyKeyFlags::DISABLED);
        }
        let (encoded_key, encoded_mods) = if legacy {
            legacy_function_key(key, mods)
        } else {
            (key, mods)
        };
        let mut event = key::Event::new()?;
        event
            .set_key(encoded_key)
            .set_mods(encoded_mods)
            .set_action(action);
        let text = text.filter(|text| !text.chars().any(char::is_control));
        let unshifted = unshifted
            .or_else(|| text.as_ref()?.chars().next()?.to_lowercase().next())
            .or_else(|| physical_codepoint(key));
        let physical_alias = text
            .as_ref()
            .filter(|text| !text.is_ascii())
            .and(match key {
                key::Key::I => Some(b'\t'),
                key::Key::M => Some(b'\r'),
                key::Key::BracketLeft => Some(0x1b),
                _ => None,
            });
        event.set_utf8(text);
        if let Some(unshifted) = unshifted {
            event.set_unshifted_codepoint(unshifted);
        }
        let mut result = InputResult::default();
        self.key_encoder.encode_to_vec(&event, &mut result.bytes)?;
        if legacy {
            if let Some(alias) = legacy_control_alias(&result.bytes, physical_alias) {
                result.bytes.clear();
                result.bytes.extend_from_slice(alias);
            } else if encoded_key == key::Key::F3
                && let Some(modifier) = result
                    .bytes
                    .strip_prefix(b"\x1b[13;")
                    .and_then(|sequence| sequence.strip_suffix(b"~"))
            {
                let mut encoded = b"\x1b[1;".to_vec();
                encoded.extend_from_slice(modifier);
                encoded.push(b'R');
                result.bytes = encoded;
            }
        }
        if !result.bytes.is_empty() && action != key::Action::Release {
            let selection_changed = self.clear_selection(terminal)?;
            let scrollbar = terminal.scrollbar()?;
            terminal.scroll_viewport(ScrollViewport::Bottom);
            self.invalidate_links();
            result.changed =
                selection_changed || scrollbar.offset + scrollbar.len < scrollbar.total;
        }
        Ok(result)
    }

    pub fn text(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        text: Vec<u8>,
    ) -> Result<InputResult, Error> {
        if terminal.mode(Mode::KAM)? {
            return Ok(InputResult::default());
        }
        self.input_bytes(terminal, text)
    }

    fn input_bytes(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        text: Vec<u8>,
    ) -> Result<InputResult, Error> {
        let selection_changed = self.clear_selection(terminal)?;
        let scrollbar = terminal.scrollbar()?;
        terminal.scroll_viewport(ScrollViewport::Bottom);
        self.invalidate_links();
        Ok(InputResult {
            bytes: text,
            changed: selection_changed || scrollbar.offset + scrollbar.len < scrollbar.total,
            ..InputResult::default()
        })
    }

    pub fn paste(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        text: String,
        config: &Config,
    ) -> Result<InputResult, Error> {
        let bracketed = config.protocols.bracketed_paste && terminal.mode(Mode::BRACKETED_PASTE)?;
        let mut input = text.into_bytes();
        let mut output = vec![0; input.len().saturating_add(12)];
        let length = libghostty_vt::paste::encode(&mut input, bracketed, &mut output)?;
        output.truncate(length);
        self.input_bytes(terminal, output)
    }

    pub fn focus(&self, terminal: &Terminal<'_, '_>, focused: bool) -> Result<InputResult, Error> {
        Ok(InputResult {
            bytes: if terminal.mode(Mode::FOCUS_EVENT)? {
                if focused { b"\x1b[I" } else { b"\x1b[O" }.to_vec()
            } else {
                Vec::new()
            },
            ..InputResult::default()
        })
    }

    pub fn paste_approved(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        text: String,
        config: &Config,
    ) -> Result<InputResult, Error> {
        let bracketed = config.protocols.bracketed_paste && terminal.mode(Mode::BRACKETED_PASTE)?;
        let bytes = if bracketed {
            let mut bytes = text.into_bytes();
            loop {
                let previous = bytes.len();
                for terminator in [b"\x1b[201~".as_slice(), b"\x9b201~".as_slice()] {
                    while let Some(offset) = memchr::memmem::find(&bytes, terminator) {
                        bytes.drain(offset..offset + terminator.len());
                    }
                }
                if bytes.len() == previous {
                    break;
                }
            }
            let mut output = b"\x1b[200~".to_vec();
            output.extend(bytes);
            output.extend_from_slice(b"\x1b[201~");
            output
        } else {
            text.replace("\r\n", "\n").replace('\n', "\r").into_bytes()
        };
        self.input_bytes(terminal, bytes)
    }

    pub fn pointer(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        config: &Config,
        size: EncoderSize,
        event: PointerEvent,
    ) -> Result<InputResult, Error> {
        self.pointer_at(terminal, config, size, event, self.pressed_at.elapsed())
    }

    pub fn pointer_at(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        config: &Config,
        size: EncoderSize,
        event: PointerEvent,
        time: Duration,
    ) -> Result<InputResult, Error> {
        self.last_pointer = Some(event);
        let tracking = config.mouse.enabled && terminal.is_mouse_tracking()?;
        let override_grab = event.mods.contains(key::Mods::SHIFT);
        let point = pointer_point(terminal, size, event.x, event.y)?;
        if event.action == mouse::Action::Press {
            self.pending_link = None;
            if event.button == Some(mouse::Button::Left) {
                let modifiers = event.mods
                    & (key::Mods::CTRL | key::Mods::SHIFT | key::Mods::ALT | key::Mods::SUPER);
                let immediate = modifiers == (key::Mods::CTRL | key::Mods::SHIFT);
                let target = if config.protocols.hyperlinks
                    && clickable_modifiers(tracking, modifiers)
                    && over_grid(terminal, size, event.x, event.y)?
                {
                    self.links.lookup(terminal, point)?
                } else {
                    None
                };
                self.link_press = Some(LinkPress {
                    target,
                    point,
                    time,
                    immediate,
                });
                if immediate {
                    return Ok(InputResult {
                        hover: self.hover(terminal, config, size, event.x, event.y, event.mods)?,
                        ..Default::default()
                    });
                }
            } else {
                self.link_press = None;
            }
        }
        if event.action == mouse::Action::Release
            && event.button == Some(mouse::Button::Left)
            && self
                .link_press
                .as_ref()
                .is_some_and(|press| press.immediate)
        {
            let press = self.link_press.take().expect("immediate link press");
            let mut result = InputResult::default();
            if point == press.point {
                result.open_url = press.target.map(|target| target.uri);
            }
            result.hover = self.hover(terminal, config, size, event.x, event.y, event.mods)?;
            return Ok(result);
        }
        let mut result = if self.drag.is_some() || !tracking || override_grab {
            self.local_pointer(terminal, config, size, event, time, tracking)?
        } else {
            self.mouse_encoder
                .set_options_from_terminal(terminal)
                .set_any_button_pressed(self.app_button.is_some())
                .set_size(size);
            let button = if event.action == mouse::Action::Motion {
                self.app_button
            } else {
                event.button
            };
            let mut raw = mouse::Event::new()?;
            raw.set_action(event.action)
                .set_button(button)
                .set_mods(event.mods)
                .set_position(mouse::Position {
                    x: event.x.max(0.0) as f32,
                    y: event.y.max(0.0) as f32,
                });
            let mut result = InputResult::default();
            self.mouse_encoder.encode_to_vec(&raw, &mut result.bytes)?;
            match event.action {
                mouse::Action::Press if real_button(event.button) => self.app_button = event.button,
                mouse::Action::Release => self.app_button = None,
                _ => {}
            }
            result
        };
        if event.action == mouse::Action::Release
            && event.button == Some(mouse::Button::Left)
            && let Some(press) = self.link_press.take()
            && point == press.point
            && time.saturating_sub(press.time) <= CLICK_INTERVAL
            && self.selection.is_none()
            && self.gesture.click_count(terminal)? == 1
            && !self.gesture.dragged(terminal)?
            && let Some(target) = press.target
        {
            self.pending_link = Some((time + CLICK_INTERVAL, target.uri));
        }
        result.hover = self.hover(terminal, config, size, event.x, event.y, event.mods)?;
        Ok(result)
    }

    pub fn hover(
        &mut self,
        terminal: &Terminal<'_, '_>,
        config: &Config,
        size: EncoderSize,
        x: f64,
        y: f64,
        mods: key::Mods,
    ) -> Result<Option<HoverState>, Error> {
        let tracking = config.mouse.enabled && terminal.is_mouse_tracking()?;
        let dragging = self.drag.map(|drag| match drag {
            Drag::Select { rectangle, .. } => rectangle,
            Drag::Extend { .. } => self
                .selection
                .as_ref()
                .is_some_and(|selection| selection.rectangle),
        });
        let point = if over_grid(terminal, size, x, y)? {
            Some(pointer_point(terminal, size, x, y)?)
        } else {
            None
        };
        let query = HoverQuery {
            point,
            modifiers: mods,
            tracking,
            dragging,
            icon: self.pointer_shapes.current(),
            hyperlinks: config.protocols.hyperlinks,
        };
        if self.last_hover_query == Some(query) {
            return Ok(None);
        }
        self.last_hover_query = Some(query);
        let link = if config.protocols.hyperlinks
            && dragging.is_none()
            && clickable_modifiers(tracking, mods)
            && let Some(point) = point
        {
            self.links.lookup(terminal, point)?
        } else {
            None
        };
        let hover = HoverState {
            pointer: pointer_icon(
                tracking,
                dragging,
                link.is_some(),
                self.pointer_shapes.current(),
            ),
            link,
        };
        if self.hover == hover {
            return Ok(None);
        }
        self.hover = hover.clone();
        Ok(Some(hover))
    }

    pub fn synchronized_output_generation(&self) -> u64 {
        self.pointer_shapes.sync_generation()
    }

    pub fn observe_output(
        &mut self,
        terminal: &Terminal<'_, '_>,
        config: &Config,
        bytes: &[u8],
    ) -> Result<InputResult, Error> {
        self.links.invalidate();
        self.last_hover_query = None;
        let replies = self.pointer_shapes.observe(bytes);
        let pointer = pointer_icon(
            config.mouse.enabled && terminal.is_mouse_tracking()?,
            self.drag.map(|drag| {
                matches!(
                    drag,
                    Drag::Select {
                        rectangle: true,
                        ..
                    }
                )
            }),
            false,
            self.pointer_shapes.current(),
        );
        let hover = HoverState {
            pointer,
            link: None,
        };
        let changed = self.hover != hover;
        self.hover = hover.clone();
        Ok(InputResult {
            bytes: replies,
            hover: changed.then_some(hover),
            ..InputResult::default()
        })
    }

    pub fn clear_hover(&mut self) -> Option<HoverState> {
        let tracking = self.last_hover_query.is_some_and(|query| query.tracking);
        self.links.invalidate();
        self.last_hover_query = None;
        if self.hover.link.take().is_some() {
            self.hover.pointer = pointer_icon(tracking, None, false, self.pointer_shapes.current());
            return Some(self.hover.clone());
        }
        None
    }

    pub fn invalidate_links(&mut self) {
        self.links.invalidate();
        self.last_hover_query = None;
    }

    pub fn pending_link_deadline(&self) -> Option<Instant> {
        self.pending_link
            .as_ref()
            .map(|(time, _)| self.pressed_at + *time)
    }

    pub fn poll_link(&mut self) -> Option<String> {
        self.poll_link_at(self.pressed_at.elapsed())
    }

    pub fn poll_link_at(&mut self, time: Duration) -> Option<String> {
        if self
            .pending_link
            .as_ref()
            .is_some_and(|(deadline, _)| *deadline <= time)
        {
            self.pending_link.take().map(|(_, uri)| uri)
        } else {
            None
        }
    }

    pub fn autoscroll(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        config: &Config,
        size: EncoderSize,
    ) -> Result<InputResult, Error> {
        self.autoscroll_at(terminal, config, size, self.pressed_at.elapsed())
    }

    pub fn autoscroll_deadline(
        &self,
        terminal: &Terminal<'_, '_>,
        size: EncoderSize,
    ) -> Result<Option<Instant>, Error> {
        let Some(drag) = self.drag else {
            return Ok(None);
        };
        let Some(pointer) = self.last_pointer else {
            return Ok(None);
        };
        if matches!(drag, Drag::Select { .. }) && !self.gesture.dragged(terminal)? {
            return Ok(None);
        }
        if autoscroll_direction(size, pointer.y).is_none() {
            return Ok(None);
        }
        Ok(Some(
            self.pressed_at + self.last_autoscroll + Duration::from_millis(30),
        ))
    }

    pub fn autoscroll_at(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        config: &Config,
        size: EncoderSize,
        time: Duration,
    ) -> Result<InputResult, Error> {
        let Some(drag) = self.drag else {
            return Ok(InputResult::default());
        };
        let Some(mut pointer) = self.last_pointer else {
            return Ok(InputResult::default());
        };
        if time.saturating_sub(self.last_autoscroll) < Duration::from_millis(30) {
            return Ok(InputResult::default());
        }
        if matches!(drag, Drag::Select { .. }) && !self.gesture.dragged(terminal)? {
            return Ok(InputResult::default());
        }
        let Some(delta) = autoscroll_direction(size, pointer.y) else {
            return Ok(InputResult::default());
        };
        self.last_autoscroll = time;
        terminal.scroll_viewport(ScrollViewport::Delta(delta));
        self.invalidate_links();
        pointer.action = mouse::Action::Motion;
        let mut result = self.local_pointer(terminal, config, size, pointer, time, false)?;
        result.changed = true;
        Ok(result)
    }

    fn local_pointer(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        config: &Config,
        size: EncoderSize,
        event: PointerEvent,
        time: Duration,
        tracking: bool,
    ) -> Result<InputResult, Error> {
        let mut result = InputResult::default();
        let point = pointer_point(terminal, size, event.x, event.y)?;
        match (event.action, event.button) {
            (mouse::Action::Press, Some(mouse::Button::Four | mouse::Button::Five)) => {
                let up = event.button == Some(mouse::Button::Four);
                if terminal.active_screen()? == Screen::Alternate
                    && terminal.mode(Mode::ALT_SCROLL)?
                {
                    let key = if up {
                        key::Key::ArrowUp
                    } else {
                        key::Key::ArrowDown
                    };
                    for _ in 0..5 {
                        result.bytes.extend(
                            self.key(
                                terminal,
                                config,
                                key,
                                key::Mods::empty(),
                                key::Action::Press,
                                None,
                                None,
                            )?
                            .bytes,
                        );
                    }
                } else {
                    terminal.scroll_viewport(ScrollViewport::Delta(if up { -5 } else { 5 }));
                    self.invalidate_links();
                    result.changed = true;
                }
            }
            (mouse::Action::Press, Some(mouse::Button::Left)) => {
                let extend = !tracking && event.mods.contains(key::Mods::SHIFT);
                if extend && self.selection.is_some() {
                    self.begin_extend(terminal, point, mouse::Button::Left)?;
                    result.changed = true;
                } else {
                    let rectangle = event.mods.contains(key::Mods::CTRL | key::Mods::ALT);
                    let mut press = PressEvent::new()?;
                    press
                        .set_time(time)?
                        .set_position(event.x, event.y)?
                        .set_repeat_interval(Duration::from_millis(500))?
                        .set_word_boundary_codepoints(KITTY_WORD_BOUNDARIES)?
                        .set_repeat_distance(size.cell_width.max(1) as f64)?;
                    let grid_ref = terminal.grid_ref(Point::Viewport(point))?;
                    let selected = press.apply(&mut self.gesture, terminal, grid_ref)?;
                    self.install(terminal, selected.as_ref())?;
                    self.drag = Some(Drag::Select {
                        button: mouse::Button::Left,
                        rectangle,
                    });
                    result.changed = true;
                }
            }
            (mouse::Action::Press, Some(mouse::Button::Right)) => {
                if self.selection.is_some() {
                    self.begin_extend(terminal, point, mouse::Button::Right)?;
                    result.changed = true;
                }
            }
            (mouse::Action::Motion, _) => {
                if let Some(drag) = self.drag {
                    match drag {
                        Drag::Select { rectangle, .. } => {
                            let mut raw = DragEvent::new()?;
                            raw.set_position(event.x, event.y)?
                                .set_rectangle(rectangle)?
                                .set_word_boundary_codepoints(KITTY_WORD_BOUNDARIES)?;
                            let grid_ref = terminal.grid_ref(Point::Viewport(point))?;
                            let selected = raw.apply(
                                &mut self.gesture,
                                terminal,
                                grid_ref,
                                Geometry {
                                    columns: u32::from(terminal.cols()?),
                                    cell_width: size.cell_width,
                                    padding_left: size.padding_left,
                                    screen_height: size.screen_height,
                                },
                            )?;
                            if let Some(selected) = selected.as_ref() {
                                self.install(terminal, Some(selected))?;
                                result.changed = true;
                            }
                        }
                        Drag::Extend { start, .. } => {
                            self.extend(terminal, point, start)?;
                            result.changed = true;
                        }
                    }
                }
            }
            (mouse::Action::Release, Some(button)) => {
                if let Some(drag) = self.drag {
                    let active_button = match drag {
                        Drag::Select { button, .. } | Drag::Extend { button, .. } => button,
                    };
                    if active_button == button {
                        let mut release = ReleaseEvent::new()?;
                        release.apply(
                            &mut self.gesture,
                            terminal,
                            Some(terminal.grid_ref(Point::Viewport(point))?),
                        )?;
                        self.drag = None;
                        result.primary = self.selection_text(terminal)?;
                        result.changed = true;
                    }
                }
                if button == mouse::Button::Middle {
                    result.paste_primary = true;
                }
            }
            _ => {}
        }
        Ok(result)
    }

    pub fn clear_selection(&mut self, terminal: &Terminal<'_, '_>) -> Result<bool, Error> {
        let changed = self.selection.take().is_some() || self.drag.is_some();
        self.drag = None;
        self.gesture.reset(terminal);
        if changed {
            terminal.set_selection(None)?;
        }
        self.pending_link = None;
        self.link_press = None;
        Ok(changed)
    }

    pub fn selection_text(&self, terminal: &Terminal<'_, '_>) -> Result<Option<String>, Error> {
        if self.selection.is_none() {
            return Ok(None);
        }
        let output = terminal
            .format_selection_alloc(None, FormatOptions::new().with_unwrap(true).with_trim(true))?;
        Ok(output.map(|output| String::from_utf8_lossy(&output).into_owned()))
    }

    fn install(
        &mut self,
        terminal: &Terminal<'_, '_>,
        selection: Option<&Selection<'_>>,
    ) -> Result<(), Error> {
        let tracked = if let Some(selection) = selection {
            let start = terminal
                .point_from_grid_ref(&selection.start(), PointSpace::Screen)?
                .ok_or(Error::InvalidValue)?;
            let end = terminal
                .point_from_grid_ref(&selection.end(), PointSpace::Screen)?
                .ok_or(Error::InvalidValue)?;
            Some(TrackedSelection {
                start: terminal.track_grid_ref(Point::Screen(start))?,
                end: terminal.track_grid_ref(Point::Screen(end))?,
                rectangle: selection.is_rectangle(),
            })
        } else {
            None
        };
        terminal.set_selection(selection)?;
        self.selection = tracked;
        Ok(())
    }

    fn begin_extend(
        &mut self,
        terminal: &mut Terminal<'_, '_>,
        point: PointCoordinate,
        button: mouse::Button,
    ) -> Result<(), Error> {
        let Some(selection) = self.selection.as_ref() else {
            return Ok(());
        };
        let Some(start) = selection.start.point(PointSpace::Screen)? else {
            return Ok(());
        };
        let Some(end) = selection.end.point(PointSpace::Screen)? else {
            return Ok(());
        };
        let clicked = terminal.grid_ref(Point::Viewport(point))?;
        let clicked = terminal
            .point_from_grid_ref(&clicked, PointSpace::Screen)?
            .ok_or(Error::InvalidValue)?;
        let cols = i64::from(terminal.cols()?);
        let ordinal = |point: PointCoordinate| i64::from(point.y) * cols + i64::from(point.x);
        let move_start =
            (ordinal(clicked) - ordinal(start)).abs() < (ordinal(clicked) - ordinal(end)).abs();
        self.drag = Some(Drag::Extend {
            button,
            start: move_start,
        });
        self.extend(terminal, point, move_start)
    }

    fn extend(
        &mut self,
        terminal: &mut Terminal<'_, '_>,
        point: PointCoordinate,
        move_start: bool,
    ) -> Result<(), Error> {
        let Some(selection) = self.selection.as_mut() else {
            return Ok(());
        };
        if move_start {
            selection.start.set(terminal, Point::Viewport(point))?;
        } else {
            selection.end.set(terminal, Point::Viewport(point))?;
        }
        if let (Some(start), Some(end)) = (
            selection.start.snapshot(terminal)?,
            selection.end.snapshot(terminal)?,
        ) {
            let selected = Selection::new(start, end, selection.rectangle);
            terminal.set_selection(Some(&selected))?;
        }
        Ok(())
    }
}

fn autoscroll_direction(size: EncoderSize, pointer_y: f64) -> Option<isize> {
    let margin = f64::from(size.cell_height.max(1)) / 2.0;
    if pointer_y < f64::from(size.padding_top) + margin {
        Some(-1)
    } else if pointer_y
        >= f64::from(size.screen_height.saturating_sub(size.padding_bottom)) - margin
    {
        Some(1)
    } else {
        None
    }
}

pub fn pointer_point(
    terminal: &Terminal<'_, '_>,
    size: EncoderSize,
    x: f64,
    y: f64,
) -> Result<PointCoordinate, Error> {
    let cols = terminal.cols()?.max(1);
    let rows = terminal.rows()?.max(1);
    Ok(PointCoordinate {
        x: (((x - f64::from(size.padding_left)).max(0.0) / f64::from(size.cell_width.max(1)))
            as u16)
            .min(cols - 1),
        y: (((y - f64::from(size.padding_top)).max(0.0) / f64::from(size.cell_height.max(1)))
            as u32)
            .min(u32::from(rows - 1)),
    })
}

fn over_grid(
    terminal: &Terminal<'_, '_>,
    size: EncoderSize,
    x: f64,
    y: f64,
) -> Result<bool, Error> {
    Ok(x >= f64::from(size.padding_left)
        && y >= f64::from(size.padding_top)
        && x < f64::from(size.padding_left + u32::from(terminal.cols()?) * size.cell_width)
        && y < f64::from(size.padding_top + u32::from(terminal.rows()?) * size.cell_height))
}

fn real_button(button: Option<mouse::Button>) -> bool {
    matches!(
        button,
        Some(
            mouse::Button::Left
                | mouse::Button::Middle
                | mouse::Button::Right
                | mouse::Button::Eight
                | mouse::Button::Nine
                | mouse::Button::Ten
                | mouse::Button::Eleven
        )
    )
}

fn physical_codepoint(key: key::Key) -> Option<char> {
    let value = key as u32;
    if (key::Key::A as u32..=key::Key::Z as u32).contains(&value) {
        return char::from_u32(u32::from(b'a') + value - key::Key::A as u32);
    }
    if (key::Key::Digit0 as u32..=key::Key::Digit9 as u32).contains(&value) {
        return char::from_u32(u32::from(b'0') + value - key::Key::Digit0 as u32);
    }
    match key {
        key::Key::Space => Some(' '),
        key::Key::Backquote => Some('`'),
        key::Key::Backslash => Some('\\'),
        key::Key::BracketLeft => Some('['),
        key::Key::BracketRight => Some(']'),
        key::Key::Comma => Some(','),
        key::Key::Equal => Some('='),
        key::Key::Minus => Some('-'),
        key::Key::Period => Some('.'),
        key::Key::Quote => Some('\''),
        key::Key::Semicolon => Some(';'),
        key::Key::Slash => Some('/'),
        _ => None,
    }
}

pub fn map_numpad_key(
    physical: winit::keyboard::KeyCode,
    logical: &winit::keyboard::Key,
) -> Option<key::Key> {
    use winit::keyboard::{Key, KeyCode, NamedKey};
    if !matches!(
        physical,
        KeyCode::Numpad0
            | KeyCode::Numpad1
            | KeyCode::Numpad2
            | KeyCode::Numpad3
            | KeyCode::Numpad4
            | KeyCode::Numpad5
            | KeyCode::Numpad6
            | KeyCode::Numpad7
            | KeyCode::Numpad8
            | KeyCode::Numpad9
            | KeyCode::NumpadDecimal
    ) {
        return None;
    }
    Some(match logical {
        Key::Named(NamedKey::ArrowUp) => key::Key::NumpadUp,
        Key::Named(NamedKey::ArrowDown) => key::Key::NumpadDown,
        Key::Named(NamedKey::ArrowLeft) => key::Key::NumpadLeft,
        Key::Named(NamedKey::ArrowRight) => key::Key::NumpadRight,
        Key::Named(NamedKey::Home) => key::Key::NumpadHome,
        Key::Named(NamedKey::End) => key::Key::NumpadEnd,
        Key::Named(NamedKey::PageUp) => key::Key::NumpadPageUp,
        Key::Named(NamedKey::PageDown) => key::Key::NumpadPageDown,
        Key::Named(NamedKey::Insert) => key::Key::NumpadInsert,
        Key::Named(NamedKey::Delete) => key::Key::NumpadDelete,
        Key::Named(NamedKey::Clear) => key::Key::NumpadBegin,
        _ => return None,
    })
}

fn legacy_control_alias(encoded: &[u8], physical_alias: Option<u8>) -> Option<&'static [u8]> {
    match encoded {
        b"\x1b[105;5u" => Some(b"\t"),
        b"\x1b[109;5u" => Some(b"\r"),
        b"\x1b[91;5u" => Some(b"\x1b"),
        b"\x1b[105;7u" => Some(b"\x1b\t"),
        b"\x1b[109;7u" => Some(b"\x1b\r"),
        b"\x1b[91;7u" => Some(b"\x1b\x1b"),
        _ if encoded.starts_with(b"\x1b[") && encoded.ends_with(b";5u") => match physical_alias {
            Some(b'\t') => Some(b"\t"),
            Some(b'\r') => Some(b"\r"),
            Some(0x1b) => Some(b"\x1b"),
            _ => None,
        },
        _ if encoded.starts_with(b"\x1b[") && encoded.ends_with(b";7u") => match physical_alias {
            Some(b'\t') => Some(b"\x1b\t"),
            Some(b'\r') => Some(b"\x1b\r"),
            Some(0x1b) => Some(b"\x1b\x1b"),
            _ => None,
        },
        _ => None,
    }
}

fn legacy_function_key(key: key::Key, mods: key::Mods) -> (key::Key, key::Mods) {
    let key = match key {
        key::Key::F13 => key::Key::F1,
        key::Key::F14 => key::Key::F2,
        key::Key::F15 => key::Key::F3,
        key::Key::F16 => key::Key::F4,
        key::Key::F17 => key::Key::F5,
        key::Key::F18 => key::Key::F6,
        key::Key::F19 => key::Key::F7,
        key::Key::F20 => key::Key::F8,
        key::Key::F21 => key::Key::F9,
        key::Key::F22 => key::Key::F10,
        key::Key::F23 => key::Key::F11,
        key::Key::F24 => key::Key::F12,
        _ => return (key, mods),
    };
    (key, mods | key::Mods::SHIFT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal() -> Terminal<'static, 'static> {
        Terminal::new(libghostty_vt::TerminalOptions {
            cols: 20,
            rows: 4,
            max_scrollback: 1024 * 1024,
        })
        .unwrap()
    }

    fn size() -> EncoderSize {
        EncoderSize {
            screen_width: 160,
            screen_height: 80,
            cell_width: 8,
            cell_height: 20,
            padding_left: 0,
            padding_right: 0,
            padding_top: 0,
            padding_bottom: 0,
        }
    }

    fn pointer(
        action: mouse::Action,
        button: Option<mouse::Button>,
        x: f64,
        y: f64,
    ) -> PointerEvent {
        PointerEvent {
            action,
            button,
            mods: key::Mods::empty(),
            x,
            y,
        }
    }

    fn apply(
        state: &mut InputState,
        terminal: &mut Terminal<'static, 'static>,
        event: PointerEvent,
        millis: u64,
    ) -> InputResult {
        state
            .pointer_at(
                terminal,
                &Config::default(),
                size(),
                event,
                Duration::from_millis(millis),
            )
            .unwrap()
    }

    #[test]
    fn ordinary_program_receives_no_pointer_or_focus_sequences() {
        let mut terminal = terminal();
        let mut state = InputState::new().unwrap();
        for event in [
            pointer(mouse::Action::Press, Some(mouse::Button::Left), 16.0, 20.0),
            pointer(mouse::Action::Motion, None, 40.0, 20.0),
            pointer(
                mouse::Action::Release,
                Some(mouse::Button::Left),
                40.0,
                20.0,
            ),
            pointer(mouse::Action::Press, Some(mouse::Button::Four), 16.0, 20.0),
            pointer(
                mouse::Action::Release,
                Some(mouse::Button::Middle),
                16.0,
                20.0,
            ),
        ] {
            assert!(apply(&mut state, &mut terminal, event, 0).bytes.is_empty());
        }
        assert!(state.focus(&terminal, true).unwrap().bytes.is_empty());
        assert!(state.focus(&terminal, false).unwrap().bytes.is_empty());
    }

    #[test]
    fn kitty_sgr_mouse_and_wheel_bytes_are_exact() {
        let mut terminal = terminal();
        terminal.vt_write(b"\x1b[?1000h\x1b[?1006h");
        let mut state = InputState::new().unwrap();
        assert_eq!(
            apply(
                &mut state,
                &mut terminal,
                pointer(mouse::Action::Press, Some(mouse::Button::Left), 16.0, 20.0),
                0
            )
            .bytes,
            b"\x1b[<0;3;2M"
        );
        assert_eq!(
            apply(
                &mut state,
                &mut terminal,
                pointer(
                    mouse::Action::Release,
                    Some(mouse::Button::Left),
                    16.0,
                    20.0
                ),
                0
            )
            .bytes,
            b"\x1b[<0;3;2m"
        );
        assert_eq!(
            apply(
                &mut state,
                &mut terminal,
                pointer(mouse::Action::Press, Some(mouse::Button::Four), 16.0, 20.0),
                0
            )
            .bytes,
            b"\x1b[<64;3;2M"
        );
        assert_eq!(
            apply(
                &mut state,
                &mut terminal,
                pointer(mouse::Action::Press, Some(mouse::Button::Five), 16.0, 20.0),
                0
            )
            .bytes,
            b"\x1b[<65;3;2M"
        );
    }

    #[test]
    fn tracking_mode_changes_take_effect_before_next_pointer() {
        let mut terminal = terminal();
        let mut state = InputState::new().unwrap();
        let event = pointer(mouse::Action::Press, Some(mouse::Button::Left), 16.0, 20.0);
        assert!(apply(&mut state, &mut terminal, event, 0).bytes.is_empty());
        apply(
            &mut state,
            &mut terminal,
            pointer(
                mouse::Action::Release,
                Some(mouse::Button::Left),
                16.0,
                20.0,
            ),
            0,
        );
        terminal.vt_write(b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(
            apply(&mut state, &mut terminal, event, 1000).bytes,
            b"\x1b[<0;3;2M"
        );
        terminal.vt_write(b"\x1b[?1000l");
        assert!(
            apply(&mut state, &mut terminal, event, 2000)
                .bytes
                .is_empty()
        );
    }

    #[test]
    fn double_click_selects_word_triple_click_selects_line_and_single_clears() {
        let mut terminal = terminal();
        terminal.vt_write(b"alpha beta");
        let mut state = InputState::new().unwrap();
        let press = pointer(mouse::Action::Press, Some(mouse::Button::Left), 12.0, 5.0);
        let release = pointer(mouse::Action::Release, Some(mouse::Button::Left), 12.0, 5.0);
        apply(&mut state, &mut terminal, press, 0);
        apply(&mut state, &mut terminal, release, 10);
        assert_eq!(state.selection_text(&terminal).unwrap(), None);
        apply(&mut state, &mut terminal, press, 100);
        assert_eq!(
            state.selection_text(&terminal).unwrap().as_deref(),
            Some("alpha")
        );
        let released = apply(&mut state, &mut terminal, release, 110);
        assert_eq!(released.primary.as_deref(), Some("alpha"));
        apply(&mut state, &mut terminal, press, 200);
        assert_eq!(
            state.selection_text(&terminal).unwrap().as_deref(),
            Some("alpha beta")
        );
        apply(&mut state, &mut terminal, release, 210);
        apply(&mut state, &mut terminal, press, 1000);
        assert_eq!(state.selection_text(&terminal).unwrap(), None);
    }

    #[test]
    fn shift_overrides_mouse_grab_for_selection_without_bytes() {
        let mut terminal = terminal();
        terminal.vt_write(b"alpha beta\x1b[?1000h\x1b[?1006h");
        let mut state = InputState::new().unwrap();
        let mut press = pointer(mouse::Action::Press, Some(mouse::Button::Left), 1.0, 5.0);
        press.mods = key::Mods::SHIFT;
        let mut drag = pointer(mouse::Action::Motion, None, 39.0, 5.0);
        drag.mods = key::Mods::SHIFT;
        assert!(apply(&mut state, &mut terminal, press, 0).bytes.is_empty());
        assert!(apply(&mut state, &mut terminal, drag, 10).bytes.is_empty());
        assert_eq!(
            state.selection_text(&terminal).unwrap().as_deref(),
            Some("alpha")
        );
    }

    #[test]
    fn right_press_extends_nearest_selection_boundary() {
        let mut terminal = terminal();
        terminal.vt_write(b"alpha beta");
        let mut state = InputState::new().unwrap();
        let press = pointer(mouse::Action::Press, Some(mouse::Button::Left), 12.0, 5.0);
        let release = pointer(mouse::Action::Release, Some(mouse::Button::Left), 12.0, 5.0);
        apply(&mut state, &mut terminal, press, 0);
        apply(&mut state, &mut terminal, release, 10);
        apply(&mut state, &mut terminal, press, 100);
        apply(&mut state, &mut terminal, release, 110);
        apply(
            &mut state,
            &mut terminal,
            pointer(mouse::Action::Press, Some(mouse::Button::Right), 73.0, 5.0),
            200,
        );
        assert_eq!(
            state.selection_text(&terminal).unwrap().as_deref(),
            Some("alpha beta")
        );
    }

    #[test]
    fn middle_release_requests_primary_paste_and_keyboard_clears_selection() {
        let mut terminal = terminal();
        terminal.vt_write(b"alpha beta");
        let mut state = InputState::new().unwrap();
        let press = pointer(mouse::Action::Press, Some(mouse::Button::Left), 12.0, 5.0);
        let release = pointer(mouse::Action::Release, Some(mouse::Button::Left), 12.0, 5.0);
        apply(&mut state, &mut terminal, press, 0);
        apply(&mut state, &mut terminal, release, 10);
        apply(&mut state, &mut terminal, press, 100);
        apply(&mut state, &mut terminal, release, 110);
        assert!(
            apply(
                &mut state,
                &mut terminal,
                pointer(
                    mouse::Action::Release,
                    Some(mouse::Button::Middle),
                    0.0,
                    0.0
                ),
                200
            )
            .paste_primary
        );
        let result = state
            .key(
                &mut terminal,
                &Config::default(),
                key::Key::A,
                key::Mods::empty(),
                key::Action::Press,
                Some("a".to_owned()),
                Some('a'),
            )
            .unwrap();
        assert_eq!(result.bytes, b"a");
        assert!(result.changed);
        assert_eq!(state.selection_text(&terminal).unwrap(), None);
    }

    #[test]
    fn repeated_committed_text_is_never_deduplicated() {
        let mut terminal = terminal();
        let mut state = InputState::new().unwrap();
        for _ in 0..3 {
            assert_eq!(
                state
                    .text(&mut terminal, "啊".as_bytes().to_vec())
                    .unwrap()
                    .bytes,
                "啊".as_bytes()
            );
        }
    }

    #[test]
    fn control_alt_drag_is_rectangular() {
        let mut terminal = terminal();
        terminal.vt_write(b"alpha beta\r\nALPHA BETA");
        let mut state = InputState::new().unwrap();
        let mut press = pointer(mouse::Action::Press, Some(mouse::Button::Left), 1.0, 5.0);
        press.mods = key::Mods::CTRL | key::Mods::ALT;
        let mut drag = pointer(mouse::Action::Motion, None, 39.0, 25.0);
        drag.mods = press.mods;
        apply(&mut state, &mut terminal, press, 0);
        apply(&mut state, &mut terminal, drag, 10);
        assert_eq!(
            state.selection_text(&terminal).unwrap().as_deref(),
            Some("alpha\nALPHA")
        );
    }

    #[test]
    fn double_click_keeps_kittys_filename_punctuation_inside_word() {
        let mut terminal = terminal();
        terminal.vt_write(b"(foo.bar/baz)");
        let mut state = InputState::new().unwrap();
        let press = pointer(mouse::Action::Press, Some(mouse::Button::Left), 12.0, 5.0);
        apply(&mut state, &mut terminal, press, 0);
        apply(
            &mut state,
            &mut terminal,
            pointer(mouse::Action::Release, Some(mouse::Button::Left), 12.0, 5.0),
            10,
        );
        apply(&mut state, &mut terminal, press, 100);
        assert_eq!(
            state.selection_text(&terminal).unwrap().as_deref(),
            Some("foo.bar/baz")
        );
    }

    #[test]
    fn ordinary_wheel_scrolls_five_lines_and_alternate_scroll_sends_arrows() {
        let mut terminal = terminal();
        for line in 0..12 {
            terminal.vt_write(format!("line {line}\r\n").as_bytes());
        }
        terminal.scroll_viewport(ScrollViewport::Bottom);
        let before = terminal.scrollbar().unwrap().offset;
        let mut state = InputState::new().unwrap();
        let result = apply(
            &mut state,
            &mut terminal,
            pointer(mouse::Action::Press, Some(mouse::Button::Four), 0.0, 20.0),
            0,
        );
        assert!(result.bytes.is_empty());
        assert_eq!(terminal.scrollbar().unwrap().offset, before - 5);
        terminal.vt_write(b"\x1b[?1049h\x1b[?1007h");
        let result = apply(
            &mut state,
            &mut terminal,
            pointer(mouse::Action::Press, Some(mouse::Button::Four), 0.0, 20.0),
            0,
        );
        assert_eq!(result.bytes, b"\x1b[A\x1b[A\x1b[A\x1b[A\x1b[A");
    }

    #[test]
    fn dragging_at_top_edge_autoscrolls_with_tracked_anchor() {
        let mut terminal = terminal();
        for line in 0..12 {
            terminal.vt_write(format!("line {line}\r\n").as_bytes());
        }
        terminal.scroll_viewport(ScrollViewport::Bottom);
        let before = terminal.scrollbar().unwrap().offset;
        let mut state = InputState::new().unwrap();
        apply(
            &mut state,
            &mut terminal,
            pointer(mouse::Action::Press, Some(mouse::Button::Left), 1.0, 45.0),
            0,
        );
        apply(
            &mut state,
            &mut terminal,
            pointer(mouse::Action::Motion, None, 24.0, -5.0),
            10,
        );
        let result = state
            .autoscroll_at(
                &mut terminal,
                &Config::default(),
                size(),
                Duration::from_millis(50),
            )
            .unwrap();
        assert!(result.changed);
        assert_eq!(terminal.scrollbar().unwrap().offset, before - 1);
        assert!(
            state
                .selection_text(&terminal)
                .unwrap()
                .unwrap()
                .contains("line")
        );
    }

    #[test]
    fn focus_and_paste_follow_current_application_modes() {
        let mut terminal = terminal();
        let mut state = InputState::new().unwrap();
        assert_eq!(
            state
                .paste(&mut terminal, "a\nb".to_owned(), &Config::default())
                .unwrap()
                .bytes,
            b"a\rb"
        );
        terminal.vt_write(b"\x1b[?2004h\x1b[?1004h");
        assert_eq!(
            state
                .paste(&mut terminal, "a\nb".to_owned(), &Config::default())
                .unwrap()
                .bytes,
            b"\x1b[200~a\nb\x1b[201~"
        );
        assert_eq!(state.focus(&terminal, true).unwrap().bytes, b"\x1b[I");
        assert_eq!(state.focus(&terminal, false).unwrap().bytes, b"\x1b[O");
    }

    #[test]
    fn kitty_control_key_press_and_release_follow_negotiation() {
        let mut terminal = terminal();
        terminal.vt_write(b"\x1b[>3u");
        assert_eq!(terminal.kitty_keyboard_flags().unwrap().bits(), 3);
        let mut state = InputState::new().unwrap();
        let press = state
            .key(
                &mut terminal,
                &Config::default(),
                key::Key::C,
                key::Mods::CTRL,
                key::Action::Press,
                Some("c".to_owned()),
                None,
            )
            .unwrap();
        let release = state
            .key(
                &mut terminal,
                &Config::default(),
                key::Key::C,
                key::Mods::CTRL,
                key::Action::Release,
                None,
                None,
            )
            .unwrap();
        assert_eq!(press.bytes, b"\x1b[99;5u");
        assert_eq!(release.bytes, b"\x1b[99;5:3u");
    }

    #[test]
    fn plain_link_click_waits_to_disambiguate_from_double_click() {
        let mut terminal = terminal();
        terminal.vt_write(b"https://example.com");
        let mut state = InputState::new().unwrap();
        let motion = apply(
            &mut state,
            &mut terminal,
            pointer(mouse::Action::Motion, None, 25.0, 10.0),
            0,
        );
        assert_eq!(
            motion.hover.unwrap().pointer,
            winit::window::CursorIcon::Pointer
        );
        apply(
            &mut state,
            &mut terminal,
            pointer(mouse::Action::Press, Some(mouse::Button::Left), 25.0, 10.0),
            10,
        );
        let released = apply(
            &mut state,
            &mut terminal,
            pointer(
                mouse::Action::Release,
                Some(mouse::Button::Left),
                25.0,
                10.0,
            ),
            50,
        );
        assert!(released.open_url.is_none());
        assert!(state.poll_link_at(Duration::from_millis(549)).is_none());
        assert_eq!(
            state.poll_link_at(Duration::from_millis(550)).as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn double_click_selects_url_without_opening_it() {
        let mut terminal = terminal();
        terminal.vt_write(b"https://example.com");
        let mut state = InputState::new().unwrap();
        for (time, action) in [
            (0, mouse::Action::Press),
            (30, mouse::Action::Release),
            (100, mouse::Action::Press),
            (140, mouse::Action::Release),
        ] {
            apply(
                &mut state,
                &mut terminal,
                pointer(action, Some(mouse::Button::Left), 25.0, 10.0),
                time,
            );
        }
        assert!(state.poll_link_at(Duration::from_secs(1)).is_none());
        assert!(state.selection_text(&terminal).unwrap().is_some());
    }

    #[test]
    fn control_shift_link_opens_immediately_and_never_reaches_grabbed_program() {
        let mut terminal = terminal();
        terminal.vt_write(
            b"\x1b[?1000h\x1b[?1006h\x1b]8;;https://target.example\x1b\\label\x1b]8;;\x1b\\",
        );
        let mut state = InputState::new().unwrap();
        let mut event = pointer(mouse::Action::Press, Some(mouse::Button::Left), 9.0, 10.0);
        event.mods = key::Mods::CTRL | key::Mods::SHIFT;
        let press = apply(&mut state, &mut terminal, event, 0);
        assert!(press.bytes.is_empty());
        event.action = mouse::Action::Release;
        let release = apply(&mut state, &mut terminal, event, 50);
        assert!(release.bytes.is_empty());
        assert_eq!(release.open_url.as_deref(), Some("https://target.example"));
    }

    #[test]
    fn hovering_over_padding_never_activates_the_nearest_link() {
        let mut terminal = terminal();
        terminal.vt_write(b"https://example.com");
        let mut state = InputState::new().unwrap();
        let mut size = size();
        size.padding_left = 29;
        size.padding_top = 29;
        size.screen_width += 58;
        size.screen_height += 58;
        let padding = state
            .hover(
                &terminal,
                &Config::default(),
                size,
                28.0,
                35.0,
                key::Mods::empty(),
            )
            .unwrap();
        assert!(padding.is_none());
        let content = state
            .hover(
                &terminal,
                &Config::default(),
                size,
                30.0,
                35.0,
                key::Mods::empty(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(content.pointer, winit::window::CursorIcon::Pointer);
        let padding = state
            .hover(
                &terminal,
                &Config::default(),
                size,
                28.0,
                35.0,
                key::Mods::empty(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(padding.pointer, winit::window::CursorIcon::Text);
        assert!(padding.link.is_none());
    }

    #[test]
    fn application_pointer_shape_and_modifier_hover_refresh_without_motion_bytes() {
        let mut terminal = terminal();
        terminal.vt_write(b"\x1b[?1000hhttps://example.com");
        let mut state = InputState::new().unwrap();
        let output = state
            .observe_output(&terminal, &Config::default(), b"\x1b]22;wait\x1b\\")
            .unwrap();
        assert_eq!(
            output.hover.unwrap().pointer,
            winit::window::CursorIcon::Wait
        );
        let hover = state
            .hover(
                &terminal,
                &Config::default(),
                size(),
                9.0,
                10.0,
                key::Mods::SHIFT,
            )
            .unwrap()
            .unwrap();
        assert_eq!(hover.pointer, winit::window::CursorIcon::Pointer);
        let hover = state
            .hover(
                &terminal,
                &Config::default(),
                size(),
                9.0,
                10.0,
                key::Mods::empty(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(hover.pointer, winit::window::CursorIcon::Wait);
    }

    #[test]
    fn moving_within_the_same_link_cell_reuses_cached_hover_state() {
        let mut terminal = terminal();
        terminal.vt_write(b"https://example.com");
        let mut state = InputState::new().unwrap();
        assert!(
            state
                .hover(
                    &terminal,
                    &Config::default(),
                    size(),
                    9.0,
                    10.0,
                    key::Mods::empty()
                )
                .unwrap()
                .is_some()
        );
        assert!(
            state
                .hover(
                    &terminal,
                    &Config::default(),
                    size(),
                    10.0,
                    11.0,
                    key::Mods::empty()
                )
                .unwrap()
                .is_none()
        );
        state.invalidate_links();
        terminal.vt_write(b"\r\x1b[2Kplain");
        let hover = state
            .hover(
                &terminal,
                &Config::default(),
                size(),
                10.0,
                11.0,
                key::Mods::empty(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(hover.pointer, winit::window::CursorIcon::Text);
        assert!(hover.link.is_none());
    }

    #[test]
    fn autoscroll_timer_exists_only_for_a_drag_at_the_edge() {
        let mut terminal = terminal();
        terminal.vt_write(b"hello world");
        let mut state = InputState::new().unwrap();
        assert!(
            state
                .autoscroll_deadline(&terminal, size())
                .unwrap()
                .is_none()
        );
        apply(
            &mut state,
            &mut terminal,
            pointer(mouse::Action::Press, Some(mouse::Button::Left), 1.0, 45.0),
            0,
        );
        assert!(
            state
                .autoscroll_deadline(&terminal, size())
                .unwrap()
                .is_none()
        );
        apply(
            &mut state,
            &mut terminal,
            pointer(mouse::Action::Motion, None, 24.0, 40.0),
            10,
        );
        assert!(
            state
                .autoscroll_deadline(&terminal, size())
                .unwrap()
                .is_none()
        );
        apply(
            &mut state,
            &mut terminal,
            pointer(mouse::Action::Motion, None, 24.0, -5.0),
            20,
        );
        assert_eq!(
            state.autoscroll_deadline(&terminal, size()).unwrap(),
            Some(state.pressed_at + Duration::from_millis(30))
        );
        apply(
            &mut state,
            &mut terminal,
            pointer(
                mouse::Action::Release,
                Some(mouse::Button::Left),
                24.0,
                -5.0,
            ),
            25,
        );
        assert!(
            state
                .autoscroll_deadline(&terminal, size())
                .unwrap()
                .is_none()
        );
    }
}
