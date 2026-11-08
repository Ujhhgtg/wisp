use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind, SetExtLinux};
use portable_pty::CommandBuilder;
use terminal::{Command, Session};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{CursorIcon, Window, WindowId};
use wisp::{config, renderer, terminal};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let options = StartupOptions::parse(std::env::args_os().skip(1))?;
    if options.help {
        println!("Usage: wisp [--config PATH] [-e|--execute COMMAND [ARGS...]]");
        return Ok(());
    }
    let config = if let Some(path) = options.config {
        toml::from_str(&std::fs::read_to_string(path)?)?
    } else {
        config::load()?
    };
    let command = if let Some((program, arguments)) = options.command.split_first() {
        let mut command = CommandBuilder::new(program);
        command.args(arguments);
        Some(command)
    } else {
        None
    };
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let mut app = WispApp::new(config, command, event_loop.create_proxy());
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[derive(Clone, Copy, Debug)]
enum UserEvent {
    TerminalUpdate,
}

struct WispApp {
    config: config::Config,
    window: Option<Arc<Window>>,
    renderer: Option<renderer::Renderer>,
    session: Option<Session>,
    modifiers: ModifiersState,
    cursor_position: (f64, f64),
    clipboard: Option<Clipboard>,
    ime_composing: bool,
    wheel_fraction: f64,
    command: Option<CommandBuilder>,
    proxy: EventLoopProxy<UserEvent>,
    pointer: CursorIcon,
    pointer_inside: bool,
}

impl WispApp {
    fn new(
        config: config::Config,
        command: Option<CommandBuilder>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        Self {
            config,
            window: None,
            renderer: None,
            session: None,
            modifiers: ModifiersState::default(),
            cursor_position: (0.0, 0.0),
            clipboard: Clipboard::new().ok(),
            ime_composing: false,
            wheel_fraction: 0.0,
            command,
            proxy,
            pointer: CursorIcon::Text,
            pointer_inside: false,
        }
    }

    fn resize(&mut self, size: winit::dpi::PhysicalSize<u32>) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        renderer.resize(size);
        if let Some(session) = self.session.as_ref() {
            session.send(Command::Geometry(renderer.geometry()));
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn send(&self, command: Command) {
        if let Some(session) = self.session.as_ref() {
            session.send(command);
        }
    }

    fn initialize_terminal(&mut self, event_loop: &ActiveEventLoop) {
        if self.renderer.is_some() || self.session.is_some() {
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        let renderer = match renderer::Renderer::new(window.clone(), self.config.clone()) {
            Ok(renderer) => renderer,
            Err(error) => {
                eprintln!("failed to initialize GPU renderer: {error}");
                event_loop.exit();
                return;
            }
        };
        let (cols, rows) = renderer.terminal_size();
        let proxy = self.proxy.clone();
        let waker: terminal::SessionWake = Arc::new(move || {
            let _ = proxy.send_event(UserEvent::TerminalUpdate);
        });
        let session_result = if let Some(command) = self.command.take() {
            Session::spawn_command_with_waker(self.config.clone(), cols, rows, command, waker)
        } else {
            Session::spawn_with_waker(self.config.clone(), cols, rows, waker)
        };
        let session = match session_result {
            Ok(session) => session,
            Err(error) => {
                eprintln!("failed to start terminal session: {error}");
                event_loop.exit();
                return;
            }
        };
        session.send(Command::Geometry(renderer.geometry()));
        self.renderer = Some(renderer);
        self.session = Some(session);
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        self.initialize_terminal(event_loop);
        let Some(session) = self.session.as_ref() else {
            return;
        };
        if let Some(renderer) = self.renderer.as_mut() {
            if let Some(snapshot) = session.try_snapshot() {
                renderer.set_snapshot(snapshot);
                if self.pointer_inside {
                    session.send(Command::Hover {
                        x: self.cursor_position.0,
                        y: self.cursor_position.1,
                        mods: map_modifiers(self.modifiers),
                    });
                }
            }
        }
        while let Some(event) = session.try_event() {
            match event {
                terminal::SessionEvent::Clipboard(text) => {
                    if let Some(clipboard) = self.clipboard.as_mut() {
                        let _ = clipboard.set_text(text);
                    }
                }
                terminal::SessionEvent::PrimarySelection(text) => {
                    if let Some(clipboard) = self.clipboard.as_mut() {
                        let _ = clipboard
                            .set()
                            .clipboard(LinuxClipboardKind::Primary)
                            .text(text);
                    }
                }
                terminal::SessionEvent::PastePrimary => {
                    if let Some(clipboard) = self.clipboard.as_mut() {
                        if let Ok(text) = clipboard
                            .get()
                            .clipboard(LinuxClipboardKind::Primary)
                            .text()
                        {
                            session.send(Command::Paste(text));
                        }
                    }
                }
                terminal::SessionEvent::Error(error) => {
                    eprintln!("terminal error: {error}");
                }
                terminal::SessionEvent::Title(title) => {
                    if let Some(window) = self.window.as_ref() {
                        window.set_title(&title);
                    }
                }
                terminal::SessionEvent::Hover(hover) => {
                    if self.pointer != hover.pointer {
                        self.pointer = hover.pointer;
                        if let Some(window) = self.window.as_ref() {
                            window.set_cursor(self.pointer);
                        }
                    }
                    if let Some(renderer) = self.renderer.as_mut() {
                        renderer.set_hover(hover.link);
                    }
                }
                terminal::SessionEvent::OpenUrl(uri) => {
                    if let Err(error) = wisp::links::open_url(&uri) {
                        eprintln!("failed to open link: {error}");
                    }
                }
                terminal::SessionEvent::Exited => event_loop.exit(),
            }
        }
        if let Some(renderer) = self.renderer.as_mut() {
            if matches!(renderer.render(), renderer::RenderOutcome::Reconfigure) {
                if let Some(window) = self.window.as_ref() {
                    renderer.resize(window.inner_size());
                }
            }
        }
    }
}

impl ApplicationHandler<UserEvent> for WispApp {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: UserEvent) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("Wisp")
            .with_min_inner_size(winit::dpi::PhysicalSize::new(320_u32, 200_u32));
        let attributes = winit::platform::wayland::WindowAttributesExtWayland::with_name(
            attributes, "wisp", "wisp",
        );
        let attributes =
            winit::platform::x11::WindowAttributesExtX11::with_name(attributes, "wisp", "wisp");
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("failed to create Wisp window: {error}");
                event_loop.exit();
                return;
            }
        };
        window.set_visible(true);
        window.set_cursor(CursorIcon::Text);
        window.set_ime_allowed(true);
        self.window = Some(window.clone());
        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if window_id != window.id() {
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Occluded(false) => window.request_redraw(),
            WindowEvent::Resized(size) => {
                self.resize(size);
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.set_scale_factor(scale_factor);
                }
                self.resize(window.inner_size());
            }
            WindowEvent::ModifiersChanged(new_modifiers) => {
                self.modifiers = new_modifiers.state();
                if self.pointer_inside {
                    self.send(Command::Hover {
                        x: self.cursor_position.0,
                        y: self.cursor_position.1,
                        mods: map_modifiers(self.modifiers),
                    });
                }
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => {
                self.ime_composing = !text.is_empty();
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.ime_composing = false;
                self.send(Command::Input(text.into_bytes()));
            }
            WindowEvent::Ime(Ime::Disabled) => {
                self.ime_composing = false;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                wisp::diagnostics::trace("keyboard-event");
                if self.config.mouse.hide_when_typing
                    && event.state == ElementState::Pressed
                    && event.text.as_ref().is_some_and(|text| !text.is_empty())
                {
                    window.set_cursor_visible(false);
                }
                let action = match event.state {
                    ElementState::Pressed if event.repeat => libghostty_vt::key::Action::Repeat,
                    ElementState::Pressed => libghostty_vt::key::Action::Press,
                    ElementState::Released => libghostty_vt::key::Action::Release,
                };
                if event.state == ElementState::Pressed
                    && self.modifiers.control_key()
                    && self.modifiers.shift_key()
                {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::KeyC) => {
                            self.send(Command::CopySelection);
                            return;
                        }
                        PhysicalKey::Code(KeyCode::KeyV) => {
                            if let Some(clipboard) = self.clipboard.as_mut() {
                                if let Ok(text) = clipboard.get_text() {
                                    self.send(Command::Paste(text));
                                }
                            }
                            return;
                        }
                        _ => {}
                    }
                }
                if self.ime_composing {
                    return;
                }
                if event.state == ElementState::Pressed
                    && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Escape))
                {
                    self.send(Command::ClearSelection);
                }
                let unshifted = match event.key_without_modifiers() {
                    Key::Character(text) => text.chars().next(),
                    _ => None,
                };
                let text = match &event.logical_key {
                    Key::Character(text) => Some(text.to_string()),
                    _ => event.text.as_ref().map(ToString::to_string),
                };
                let key = match event.physical_key {
                    PhysicalKey::Code(code) => map_key_code(code),
                    _ => None,
                }
                .unwrap_or(libghostty_vt::key::Key::Unidentified);
                self.send(Command::Key {
                    key,
                    mods: map_modifiers(self.modifiers),
                    action,
                    text,
                    unshifted,
                });
            }
            WindowEvent::CursorMoved { position, .. } => {
                window.set_cursor_visible(true);
                self.pointer_inside = true;
                self.cursor_position = (position.x, position.y);
                self.send(Command::Mouse {
                    action: libghostty_vt::mouse::Action::Motion,
                    button: None,
                    mods: map_modifiers(self.modifiers),
                    x: position.x,
                    y: position.y,
                });
            }
            WindowEvent::CursorEntered { .. } => {
                self.pointer_inside = true;
                window.set_cursor_visible(true);
                self.send(Command::Hover {
                    x: self.cursor_position.0,
                    y: self.cursor_position.1,
                    mods: map_modifiers(self.modifiers),
                });
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer_inside = false;
                self.send(Command::Leave);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                window.set_cursor_visible(true);
                let action = match state {
                    ElementState::Pressed => libghostty_vt::mouse::Action::Press,
                    ElementState::Released => libghostty_vt::mouse::Action::Release,
                };
                let terminal_button = map_mouse_button(button);
                self.send(Command::Mouse {
                    action,
                    button: terminal_button,
                    mods: map_modifiers(self.modifiers),
                    x: self.cursor_position.0,
                    y: self.cursor_position.1,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    MouseScrollDelta::PixelDelta(position) => {
                        let cell_height = self
                            .renderer
                            .as_ref()
                            .map(renderer::Renderer::cell_geometry)
                            .map_or(1, |(_, height)| height.max(1));
                        position.y / f64::from(cell_height)
                    }
                };
                self.wheel_fraction += lines;
                let steps = self.wheel_fraction.trunc() as isize;
                self.wheel_fraction -= steps as f64;
                if steps != 0 {
                    let button = if steps > 0 {
                        libghostty_vt::mouse::Button::Four
                    } else {
                        libghostty_vt::mouse::Button::Five
                    };
                    for _ in 0..steps.unsigned_abs() {
                        self.send(Command::Mouse {
                            action: libghostty_vt::mouse::Action::Press,
                            button: Some(button),
                            mods: map_modifiers(self.modifiers),
                            x: self.cursor_position.0,
                            y: self.cursor_position.1,
                        });
                    }
                }
            }
            WindowEvent::Focused(focused) if self.config.protocols.focus_events => {
                self.send(Command::Focus(focused));
            }
            WindowEvent::RedrawRequested => {
                self.redraw(event_loop);
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(deadline) = self
            .renderer
            .as_ref()
            .and_then(renderer::Renderer::next_frame_deadline)
        {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            if deadline <= Instant::now() {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }
}

fn map_modifiers(modifiers: ModifiersState) -> libghostty_vt::key::Mods {
    let mut result = libghostty_vt::key::Mods::empty();
    if modifiers.shift_key() {
        result |= libghostty_vt::key::Mods::SHIFT;
    }
    if modifiers.control_key() {
        result |= libghostty_vt::key::Mods::CTRL;
    }
    if modifiers.alt_key() {
        result |= libghostty_vt::key::Mods::ALT;
    }
    if modifiers.super_key() {
        result |= libghostty_vt::key::Mods::SUPER;
    }
    result
}

#[derive(Debug, Default)]
struct StartupOptions {
    config: Option<PathBuf>,
    command: Vec<OsString>,
    help: bool,
}

impl StartupOptions {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut arguments = arguments.into_iter();
        let mut options = Self::default();
        while let Some(argument) = arguments.next() {
            match argument.to_str() {
                Some("--help" | "-h") => options.help = true,
                Some("--config") => {
                    options.config =
                        Some(arguments.next().ok_or("--config requires a path")?.into());
                }
                Some("-e" | "--execute" | "--") => {
                    options.command.extend(arguments);
                    if options.command.is_empty() {
                        return Err("--execute requires a command".to_owned());
                    }
                    break;
                }
                _ => return Err(format!("unknown option: {}", argument.to_string_lossy())),
            }
        }
        Ok(options)
    }
}

fn map_mouse_button(button: MouseButton) -> Option<libghostty_vt::mouse::Button> {
    match button {
        MouseButton::Left => Some(libghostty_vt::mouse::Button::Left),
        MouseButton::Right => Some(libghostty_vt::mouse::Button::Right),
        MouseButton::Middle => Some(libghostty_vt::mouse::Button::Middle),
        MouseButton::Back => Some(libghostty_vt::mouse::Button::Four),
        MouseButton::Forward => Some(libghostty_vt::mouse::Button::Five),
        MouseButton::Other(_) => None,
    }
}

fn map_key_code(code: KeyCode) -> Option<libghostty_vt::key::Key> {
    use libghostty_vt::key::Key;
    Some(match code {
        KeyCode::Backquote => Key::Backquote,
        KeyCode::Backslash => Key::Backslash,
        KeyCode::BracketLeft => Key::BracketLeft,
        KeyCode::BracketRight => Key::BracketRight,
        KeyCode::Comma => Key::Comma,
        KeyCode::Digit0 => Key::Digit0,
        KeyCode::Digit1 => Key::Digit1,
        KeyCode::Digit2 => Key::Digit2,
        KeyCode::Digit3 => Key::Digit3,
        KeyCode::Digit4 => Key::Digit4,
        KeyCode::Digit5 => Key::Digit5,
        KeyCode::Digit6 => Key::Digit6,
        KeyCode::Digit7 => Key::Digit7,
        KeyCode::Digit8 => Key::Digit8,
        KeyCode::Digit9 => Key::Digit9,
        KeyCode::Equal => Key::Equal,
        KeyCode::IntlBackslash => Key::IntlBackslash,
        KeyCode::IntlRo => Key::IntlRo,
        KeyCode::IntlYen => Key::IntlYen,
        KeyCode::KeyA => Key::A,
        KeyCode::KeyB => Key::B,
        KeyCode::KeyC => Key::C,
        KeyCode::KeyD => Key::D,
        KeyCode::KeyE => Key::E,
        KeyCode::KeyF => Key::F,
        KeyCode::KeyG => Key::G,
        KeyCode::KeyH => Key::H,
        KeyCode::KeyI => Key::I,
        KeyCode::KeyJ => Key::J,
        KeyCode::KeyK => Key::K,
        KeyCode::KeyL => Key::L,
        KeyCode::KeyM => Key::M,
        KeyCode::KeyN => Key::N,
        KeyCode::KeyO => Key::O,
        KeyCode::KeyP => Key::P,
        KeyCode::KeyQ => Key::Q,
        KeyCode::KeyR => Key::R,
        KeyCode::KeyS => Key::S,
        KeyCode::KeyT => Key::T,
        KeyCode::KeyU => Key::U,
        KeyCode::KeyV => Key::V,
        KeyCode::KeyW => Key::W,
        KeyCode::KeyX => Key::X,
        KeyCode::KeyY => Key::Y,
        KeyCode::KeyZ => Key::Z,
        KeyCode::Minus => Key::Minus,
        KeyCode::Period => Key::Period,
        KeyCode::Quote => Key::Quote,
        KeyCode::Semicolon => Key::Semicolon,
        KeyCode::Slash => Key::Slash,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::CapsLock => Key::CapsLock,
        KeyCode::AltLeft => Key::AltLeft,
        KeyCode::AltRight => Key::AltRight,
        KeyCode::ControlLeft => Key::ControlLeft,
        KeyCode::ControlRight => Key::ControlRight,
        KeyCode::SuperLeft => Key::MetaLeft,
        KeyCode::SuperRight => Key::MetaRight,
        KeyCode::ShiftLeft => Key::ShiftLeft,
        KeyCode::ShiftRight => Key::ShiftRight,
        KeyCode::Enter => Key::Enter,
        KeyCode::Space => Key::Space,
        KeyCode::Tab => Key::Tab,
        KeyCode::Delete => Key::Delete,
        KeyCode::End => Key::End,
        KeyCode::Home => Key::Home,
        KeyCode::Insert => Key::Insert,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::ArrowDown => Key::ArrowDown,
        KeyCode::ArrowLeft => Key::ArrowLeft,
        KeyCode::ArrowRight => Key::ArrowRight,
        KeyCode::ArrowUp => Key::ArrowUp,
        KeyCode::NumLock => Key::NumLock,
        KeyCode::Numpad0 => Key::Numpad0,
        KeyCode::Numpad1 => Key::Numpad1,
        KeyCode::Numpad2 => Key::Numpad2,
        KeyCode::Numpad3 => Key::Numpad3,
        KeyCode::Numpad4 => Key::Numpad4,
        KeyCode::Numpad5 => Key::Numpad5,
        KeyCode::Numpad6 => Key::Numpad6,
        KeyCode::Numpad7 => Key::Numpad7,
        KeyCode::Numpad8 => Key::Numpad8,
        KeyCode::Numpad9 => Key::Numpad9,
        KeyCode::NumpadEnter => Key::NumpadEnter,
        KeyCode::NumpadAdd => Key::NumpadAdd,
        KeyCode::NumpadSubtract => Key::NumpadSubtract,
        KeyCode::NumpadMultiply => Key::NumpadMultiply,
        KeyCode::NumpadDivide => Key::NumpadDivide,
        KeyCode::NumpadDecimal => Key::NumpadDecimal,
        KeyCode::Escape => Key::Escape,
        KeyCode::F1 => Key::F1,
        KeyCode::F2 => Key::F2,
        KeyCode::F3 => Key::F3,
        KeyCode::F4 => Key::F4,
        KeyCode::F5 => Key::F5,
        KeyCode::F6 => Key::F6,
        KeyCode::F7 => Key::F7,
        KeyCode::F8 => Key::F8,
        KeyCode::F9 => Key::F9,
        KeyCode::F10 => Key::F10,
        KeyCode::F11 => Key::F11,
        KeyCode::F12 => Key::F12,
        KeyCode::F13 => Key::F13,
        KeyCode::F14 => Key::F14,
        KeyCode::F15 => Key::F15,
        KeyCode::F16 => Key::F16,
        KeyCode::F17 => Key::F17,
        KeyCode::F18 => Key::F18,
        KeyCode::F19 => Key::F19,
        KeyCode::F20 => Key::F20,
        KeyCode::F21 => Key::F21,
        KeyCode::F22 => Key::F22,
        KeyCode::F23 => Key::F23,
        KeyCode::F24 => Key::F24,
        _ => return None,
    })
}
