use std::collections::HashSet;
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
use wisp::close_confirmation::{Choice, CloseDialog, DialogKind};
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
    horizontal_wheel_fraction: f64,
    command: Option<CommandBuilder>,
    proxy: EventLoopProxy<UserEvent>,
    pointer: CursorIcon,
    pointer_inside: bool,
    consumed_keys: HashSet<KeyCode>,
    default_font_size: f32,
    close_dialog: Option<CloseDialog>,
    pending_paste: Option<(String, String)>,
    terminal_title: String,
}

impl WispApp {
    fn new(
        config: config::Config,
        command: Option<CommandBuilder>,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        let default_font_size = config.font.size;
        Self {
            config,
            window: None,
            renderer: None,
            session: None,
            modifiers: ModifiersState::default(),
            cursor_position: (0.0, 0.0),
            clipboard: Clipboard::new()
                .map_err(|error| eprintln!("failed to initialize clipboard: {error}"))
                .ok(),
            ime_composing: false,
            wheel_fraction: 0.0,
            horizontal_wheel_fraction: 0.0,
            command,
            proxy,
            pointer: CursorIcon::Text,
            pointer_inside: false,
            consumed_keys: HashSet::new(),
            default_font_size,
            close_dialog: None,
            pending_paste: None,
            terminal_title: "Wisp".to_owned(),
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

    fn request_close(&mut self, event_loop: &ActiveEventLoop) {
        if self.close_dialog.is_some() {
            if self
                .close_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.kind == DialogKind::Close)
            {
                return;
            }
            self.pending_paste = None;
        }
        let needs_confirmation = self.config.window.confirm_on_close
            && self.session.as_ref().is_some_and(|session| {
                session.needs_close_confirmation().unwrap_or_else(|error| {
                    eprintln!("failed to inspect terminal jobs: {error}");
                    true
                })
            });
        if !needs_confirmation {
            event_loop.exit();
            return;
        }
        let message = self
            .session
            .as_ref()
            .map(Session::close_message)
            .unwrap_or_default();
        self.show_dialog(CloseDialog::new(message));
        wisp::diagnostics::trace("close-dialog-open");
    }

    fn show_dialog(&mut self, dialog: CloseDialog) {
        if let Some(window) = &self.window {
            window.set_title(match dialog.kind {
                DialogKind::Close => "Close window?",
                DialogKind::Paste => "Allow paste?",
            });
        }
        self.close_dialog = Some(dialog);
        self.ime_composing = false;
        if let Some(renderer) = &mut self.renderer {
            renderer.set_preedit(String::new());
        }
        if let Some(window) = &self.window {
            window.set_ime_allowed(false);
            window.set_cursor_visible(true);
            window.set_cursor(CursorIcon::Default);
        }
        self.refresh_close_dialog();
    }

    fn refresh_close_dialog(&mut self) {
        if let Some(renderer) = &mut self.renderer {
            renderer.set_close_confirmation(self.close_dialog.clone());
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn choose_close_dialog(&mut self, choice: Choice, event_loop: &ActiveEventLoop) {
        let kind = self.close_dialog.as_ref().map(|dialog| dialog.kind);
        if kind == Some(DialogKind::Close) && choice == Choice::Close {
            event_loop.exit();
        } else {
            if let Some((original, sanitized)) = self.pending_paste.take() {
                match choice {
                    Choice::Sanitize => self.send(Command::PasteApproved(sanitized)),
                    Choice::PasteAnyway => self.send(Command::PasteApproved(original)),
                    _ => {}
                }
            }
            self.close_dialog = None;
            wisp::diagnostics::trace("close-dialog-cancelled");
            if let Some(window) = &self.window {
                window.set_ime_allowed(true);
                window.set_cursor(self.pointer);
                window.set_title(&self.terminal_title);
            }
            self.refresh_close_dialog();
        }
    }

    fn close_dialog_event(&mut self, event: &WindowEvent, event_loop: &ActiveEventLoop) -> bool {
        if self.close_dialog.is_none() {
            return false;
        }
        let mut choice = None;
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if event.state == ElementState::Released {
                        self.consumed_keys.remove(&code);
                    } else {
                        self.consumed_keys.insert(code);
                    }
                }
                if event.state == ElementState::Pressed
                    && !event.repeat
                    && !self.modifiers.alt_key()
                    && !self.modifiers.super_key()
                {
                    choice = if self.modifiers.control_key() {
                        match &event.logical_key {
                            Key::Character(text) if text.eq_ignore_ascii_case("c") => {
                                Some(Choice::Cancel)
                            }
                            _ => None,
                        }
                    } else {
                        self.close_dialog.as_mut().unwrap().key(&event.logical_key)
                    };
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer_inside = true;
                self.cursor_position = (position.x, position.y);
                let hit = self
                    .renderer
                    .as_ref()
                    .and_then(|renderer| renderer.dialog_hit(position.x, position.y));
                if let Some(window) = &self.window {
                    window.set_cursor(if hit.is_some() {
                        CursorIcon::Pointer
                    } else {
                        CursorIcon::Default
                    });
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let hit = self.renderer.as_ref().and_then(|renderer| {
                    renderer.dialog_hit(self.cursor_position.0, self.cursor_position.1)
                });
                choice = self
                    .close_dialog
                    .as_mut()
                    .unwrap()
                    .mouse(hit, *state == ElementState::Pressed);
            }
            WindowEvent::Ime(_)
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. } => {}
            WindowEvent::CursorEntered { .. } => self.pointer_inside = true,
            WindowEvent::CursorLeft { .. } => self.pointer_inside = false,
            WindowEvent::Focused(focused) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_focused(*focused);
                }
            }
            _ => return false,
        }
        if let Some(choice) = choice {
            self.choose_close_dialog(choice, event_loop);
        }
        true
    }

    fn set_font_size(&mut self, points: f32) {
        if let Some(renderer) = &mut self.renderer {
            if let Err(error) = renderer.set_font_size(points) {
                eprintln!("failed to change font size: {error}");
                return;
            }
            self.config.font.size = points.clamp(2.0, 96.0);
            if let Some(session) = &self.session {
                session.send(Command::Geometry(renderer.geometry()));
            }
            if let Some(window) = &self.window {
                window.request_redraw();
            }
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
                if self.pointer_inside && self.close_dialog.is_none() {
                    session.send(Command::Hover {
                        x: self.cursor_position.0,
                        y: self.cursor_position.1,
                        mods: map_modifiers(self.modifiers),
                    });
                }
            }
            let (position, size) = renderer.ime_cursor_area();
            if let Some(window) = &self.window {
                window.set_ime_cursor_area(position, size);
            }
        }
        let mut paste_warning = false;
        while let Some(event) = session.try_event() {
            match event {
                terminal::SessionEvent::Bell => {
                    if let Some(window) = &self.window {
                        window.request_user_attention(Some(
                            winit::window::UserAttentionType::Informational,
                        ));
                    }
                    if let Some(renderer) = &mut self.renderer {
                        renderer.bell();
                    }
                }
                terminal::SessionEvent::Clipboard(text) => {
                    wisp::diagnostics::trace("clipboard-write");
                    if let Some(clipboard) = self.clipboard.as_mut()
                        && let Err(error) = clipboard.set_text(text)
                    {
                        eprintln!("failed to copy to clipboard: {error}");
                    }
                }
                terminal::SessionEvent::PrimarySelection(text) => {
                    if let Some(clipboard) = self.clipboard.as_mut()
                        && let Err(error) = clipboard
                            .set()
                            .clipboard(LinuxClipboardKind::Primary)
                            .text(text)
                    {
                        eprintln!("failed to copy primary selection: {error}");
                    }
                }
                terminal::SessionEvent::PastePrimary => {
                    if self.close_dialog.is_some() {
                        continue;
                    }
                    if let Some(clipboard) = self.clipboard.as_mut()
                        && let Ok(text) = clipboard
                            .get()
                            .clipboard(LinuxClipboardKind::Primary)
                            .text()
                    {
                        session.send(Command::RequestPaste(text));
                    }
                }
                terminal::SessionEvent::PasteWarning {
                    original,
                    sanitized,
                } => {
                    if self.close_dialog.is_none() && self.pending_paste.is_none() {
                        self.pending_paste = Some((original, sanitized));
                        paste_warning = true;
                    }
                }
                terminal::SessionEvent::Error(error) => {
                    eprintln!("terminal error: {error}");
                }
                terminal::SessionEvent::Title(title) => {
                    if self.close_dialog.is_none()
                        && let Some(window) = self.window.as_ref()
                    {
                        window.set_title(&title);
                    }
                    self.terminal_title = title;
                }
                terminal::SessionEvent::Hover(hover) => {
                    if self.close_dialog.is_some() {
                        continue;
                    }
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
        if paste_warning {
            self.show_dialog(CloseDialog::paste());
            wisp::diagnostics::trace("paste-dialog-open");
        }
        if let Some(renderer) = self.renderer.as_mut()
            && matches!(renderer.render(), renderer::RenderOutcome::Reconfigure)
            && let Some(window) = self.window.as_ref()
        {
            renderer.resize(window.inner_size());
        }
    }
}

impl ApplicationHandler<UserEvent> for WispApp {
    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. })
            && let Some(window) = self.window.as_ref()
        {
            window.request_redraw();
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
        if self.close_dialog_event(&event, event_loop) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => self.request_close(event_loop),
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
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_preedit(text);
                }
                window.request_redraw();
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.ime_composing = false;
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_preedit(String::new());
                }
                self.send(Command::Input(text.into_bytes()));
                window.request_redraw();
            }
            WindowEvent::Ime(Ime::Disabled) => {
                self.ime_composing = false;
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_preedit(String::new());
                }
                window.request_redraw();
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
                if let PhysicalKey::Code(code) = event.physical_key
                    && ((event.state == ElementState::Released && self.consumed_keys.remove(&code))
                        || (event.repeat && self.consumed_keys.contains(&code)))
                {
                    return;
                }
                if event.state == ElementState::Pressed
                    && self.modifiers.shift_key()
                    && !self.modifiers.control_key()
                    && !self.modifiers.alt_key()
                    && !self.modifiers.super_key()
                {
                    let page = self.renderer.as_ref().map_or(1, |renderer| {
                        renderer.terminal_size().1.saturating_sub(1).max(1) as isize
                    });
                    let delta = match event.physical_key {
                        PhysicalKey::Code(KeyCode::PageUp) => Some((-page, KeyCode::PageUp)),
                        PhysicalKey::Code(KeyCode::PageDown) => Some((page, KeyCode::PageDown)),
                        _ => None,
                    };
                    if let Some((delta, code)) = delta {
                        self.consumed_keys.insert(code);
                        self.send(Command::Scroll(delta));
                        return;
                    }
                }
                if event.state == ElementState::Pressed
                    && self.modifiers.control_key()
                    && self.modifiers.shift_key()
                    && !self.modifiers.alt_key()
                    && !self.modifiers.super_key()
                {
                    let shortcut =
                        terminal_shortcut(event.key_without_modifiers(), event.physical_key);
                    if shortcut.is_some()
                        && let PhysicalKey::Code(code) = event.physical_key
                    {
                        self.consumed_keys.insert(code);
                    }
                    match shortcut {
                        Some(KeyCode::KeyC) => {
                            wisp::diagnostics::trace("clipboard-copy-key");
                            self.send(Command::CopySelection);
                            return;
                        }
                        Some(KeyCode::KeyV) => {
                            wisp::diagnostics::trace("clipboard-paste-key");
                            if let Some(clipboard) = self.clipboard.as_mut()
                                && let Ok(text) = clipboard.get_text()
                            {
                                self.send(Command::RequestPaste(text));
                            }
                            return;
                        }
                        Some(KeyCode::Equal) => {
                            self.set_font_size(self.config.font.size + 1.0);
                            return;
                        }
                        Some(KeyCode::Minus) => {
                            self.set_font_size(self.config.font.size - 1.0);
                            return;
                        }
                        Some(KeyCode::Digit0) => {
                            self.set_font_size(self.default_font_size);
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
                    PhysicalKey::Code(code) => {
                        wisp::input::map_numpad_key(code, &event.logical_key)
                            .or_else(|| map_key_code(code))
                    }
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
                let (horizontal, lines) = match delta {
                    MouseScrollDelta::LineDelta(horizontal, vertical) => {
                        (f64::from(horizontal), f64::from(vertical))
                    }
                    MouseScrollDelta::PixelDelta(position) => {
                        let (cell_width, cell_height) = self
                            .renderer
                            .as_ref()
                            .map(renderer::Renderer::cell_geometry)
                            .unwrap_or((1, 1));
                        (
                            position.x / f64::from(cell_width.max(1)),
                            position.y / f64::from(cell_height.max(1)),
                        )
                    }
                };
                self.wheel_fraction += lines;
                self.horizontal_wheel_fraction += horizontal;
                let steps = self.wheel_fraction.trunc() as isize;
                let horizontal_steps = self.horizontal_wheel_fraction.trunc() as isize;
                self.wheel_fraction -= steps as f64;
                self.horizontal_wheel_fraction -= horizontal_steps as f64;
                for (steps, positive, negative) in [
                    (
                        steps,
                        libghostty_vt::mouse::Button::Four,
                        libghostty_vt::mouse::Button::Five,
                    ),
                    (
                        horizontal_steps,
                        libghostty_vt::mouse::Button::Six,
                        libghostty_vt::mouse::Button::Seven,
                    ),
                ] {
                    let button = if steps > 0 { positive } else { negative };
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
            WindowEvent::Focused(focused) => {
                if !focused {
                    self.consumed_keys.clear();
                    self.ime_composing = false;
                    if let Some(renderer) = &mut self.renderer {
                        renderer.set_preedit(String::new());
                    }
                }
                if let Some(renderer) = &mut self.renderer {
                    renderer.set_focused(focused);
                }
                if self.config.protocols.focus_events {
                    self.send(Command::Focus(focused));
                }
                window.request_redraw();
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
            if deadline <= Instant::now()
                && let Some(window) = self.window.as_ref()
            {
                window.request_redraw();
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

fn terminal_shortcut(logical: Key, physical: PhysicalKey) -> Option<KeyCode> {
    if let Key::Character(text) = logical
        && text.is_ascii()
    {
        return match text.to_ascii_lowercase().as_str() {
            "c" => Some(KeyCode::KeyC),
            "v" => Some(KeyCode::KeyV),
            "=" | "+" => Some(KeyCode::Equal),
            "-" => Some(KeyCode::Minus),
            "0" => Some(KeyCode::Digit0),
            _ => None,
        };
    }
    match physical {
        PhysicalKey::Code(
            code @ (KeyCode::KeyC
            | KeyCode::KeyV
            | KeyCode::Equal
            | KeyCode::Minus
            | KeyCode::Digit0),
        ) => Some(code),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_shortcuts_follow_the_active_latin_layout() {
        assert_eq!(
            terminal_shortcut(Key::Character("c".into()), PhysicalKey::Code(KeyCode::KeyJ)),
            Some(KeyCode::KeyC)
        );
        assert_eq!(
            terminal_shortcut(Key::Character("v".into()), PhysicalKey::Code(KeyCode::KeyK)),
            Some(KeyCode::KeyV)
        );
        assert_eq!(
            terminal_shortcut(Key::Character("j".into()), PhysicalKey::Code(KeyCode::KeyC)),
            None
        );
    }

    #[test]
    fn non_latin_shortcuts_retain_the_physical_copy_and_paste_bindings() {
        assert_eq!(
            terminal_shortcut(Key::Character("с".into()), PhysicalKey::Code(KeyCode::KeyC)),
            Some(KeyCode::KeyC)
        );
        assert_eq!(
            terminal_shortcut(Key::Character("м".into()), PhysicalKey::Code(KeyCode::KeyV)),
            Some(KeyCode::KeyV)
        );
        assert_eq!(
            terminal_shortcut(Key::Character("ц".into()), PhysicalKey::Code(KeyCode::KeyW)),
            None
        );
    }
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
        MouseButton::Back => Some(libghostty_vt::mouse::Button::Eight),
        MouseButton::Forward => Some(libghostty_vt::mouse::Button::Nine),
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
