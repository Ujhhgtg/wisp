use std::error::Error;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsFd;
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

#[derive(Default)]
struct State {
    manager: Option<ZwpVirtualKeyboardManagerV1>,
    seat: Option<wl_seat::WlSeat>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &(),
        _connection: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "zwp_virtual_keyboard_manager_v1" => {
                    state.manager = Some(registry.bind(name, version.min(1), queue, ()));
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.min(1), queue, ()));
                }
                _ => {}
            }
        }
    }
}

delegate_noop!(State: ignore ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ignore ZwpVirtualKeyboardV1);
delegate_noop!(State: ignore wl_seat::WlSeat);

fn timestamp() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u32
}

fn main() -> Result<(), Box<dyn Error>> {
    let operation = std::env::args().nth(1).unwrap_or_default();
    let (modifiers, key) = match operation.as_str() {
        "copy" => (5, 46),
        "paste" => (5, 47),
        "eof" => (4, 32),
        "enter" => (0, 28),
        "escape" => (0, 1),
        "tab" => (0, 15),
        "left" => (0, 105),
        "right" => (0, 106),
        _ => {
            return Err("usage: keyboard_driver copy|paste|eof|enter|escape|tab|left|right".into());
        }
    };
    let output = Command::new("xkbcli")
        .args(["compile-keymap", "--format", "1", "--layout", "us"])
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let mut keymap = output.stdout;
    keymap.push(0);
    let filename = std::env::temp_dir().join(format!(
        "wisp-keyboard-{}-{}.xkb",
        std::process::id(),
        timestamp()
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&filename)?;
    std::fs::remove_file(filename)?;
    file.write_all(&keymap)?;

    let connection = Connection::connect_to_env()?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());
    let mut state = State::default();
    queue.roundtrip(&mut state)?;
    let manager = state
        .manager
        .as_ref()
        .ok_or("compositor has no virtual keyboard protocol")?;
    let seat = state.seat.as_ref().ok_or("compositor has no input seat")?;
    let keyboard = manager.create_virtual_keyboard(seat, &handle, ());
    keyboard.keymap(1, file.as_fd(), keymap.len() as u32);
    queue.roundtrip(&mut state)?;
    thread::sleep(Duration::from_millis(25));
    keyboard.modifiers(modifiers, 0, 0, 0);
    queue.roundtrip(&mut state)?;
    thread::sleep(Duration::from_millis(25));
    keyboard.key(timestamp(), key, 1);
    queue.roundtrip(&mut state)?;
    thread::sleep(Duration::from_millis(25));
    keyboard.key(timestamp(), key, 0);
    keyboard.modifiers(0, 0, 0, 0);
    queue.roundtrip(&mut state)?;
    thread::sleep(Duration::from_millis(25));
    keyboard.destroy();
    queue.roundtrip(&mut state)?;
    Ok(())
}
