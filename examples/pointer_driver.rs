use std::error::Error;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wayland_client::protocol::{wl_pointer, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, delegate_noop};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

const STEP_DELAY: Duration = Duration::from_millis(25);
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

#[derive(Default)]
struct State {
    manager: Option<ZwlrVirtualPointerManagerV1>,
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
            && interface == "zwlr_virtual_pointer_manager_v1"
        {
            state.manager = Some(registry.bind(name, version.min(2), queue, ()));
        }
    }
}

delegate_noop!(State: ignore ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ignore ZwlrVirtualPointerV1);

struct Driver {
    queue: EventQueue<State>,
    state: State,
    pointer: ZwlrVirtualPointerV1,
}

impl Driver {
    fn new() -> Result<Self, Box<dyn Error>> {
        let connection = Connection::connect_to_env()?;
        let mut queue = connection.new_event_queue();
        let queue_handle = queue.handle();
        connection.display().get_registry(&queue_handle, ());
        let mut state = State::default();
        queue.roundtrip(&mut state)?;
        let manager = state
            .manager
            .as_ref()
            .ok_or("compositor does not advertise wlr virtual-pointer protocol")?;
        let pointer = manager.create_virtual_pointer(None, &queue_handle, ());
        queue.roundtrip(&mut state)?;
        Ok(Self {
            queue,
            state,
            pointer,
        })
    }

    fn frame(&mut self) -> Result<(), Box<dyn Error>> {
        self.pointer.frame();
        self.queue.roundtrip(&mut self.state)?;
        thread::sleep(STEP_DELAY);
        Ok(())
    }

    fn move_to(&mut self, x: u32, y: u32, width: u32, height: u32) -> Result<(), Box<dyn Error>> {
        if width == 0 || height == 0 || x > width || y > height {
            return Err("absolute coordinates must be within nonzero WIDTH and HEIGHT".into());
        }
        self.pointer.motion_absolute(time(), x, y, width, height);
        self.frame()
    }

    fn button(&mut self, button: u32, pressed: bool) -> Result<(), Box<dyn Error>> {
        self.pointer.button(
            time(),
            button,
            if pressed {
                wl_pointer::ButtonState::Pressed
            } else {
                wl_pointer::ButtonState::Released
            },
        );
        self.frame()
    }

    fn finish(mut self) -> Result<(), Box<dyn Error>> {
        self.pointer.destroy();
        self.queue.roundtrip(&mut self.state)?;
        Ok(())
    }
}

fn time() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_millis() as u32
}

fn coordinate(args: &[String], index: usize) -> Result<u32, Box<dyn Error>> {
    args.get(index)
        .ok_or_else(|| format!("missing argument {}", index + 1).into())
        .and_then(|value| value.parse::<u32>().map_err(Into::into))
}

fn button(name: Option<&String>) -> Result<u32, Box<dyn Error>> {
    match name.map(String::as_str).unwrap_or("left") {
        "left" => Ok(BTN_LEFT),
        "right" => Ok(BTN_RIGHT),
        "middle" => Ok(BTN_MIDDLE),
        other => Err(format!("unknown mouse button: {other}").into()),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let operation = args.first().map(String::as_str).unwrap_or("");
    if !matches!(operation, "click" | "drag" | "wheel" | "move") {
        return Err("usage: pointer_driver click X Y WIDTH HEIGHT [left|right|middle]\n       pointer_driver drag X1 Y1 X2 Y2 WIDTH HEIGHT [left|right|middle]\n       pointer_driver wheel X Y WIDTH HEIGHT [up|down]\n       pointer_driver move X Y WIDTH HEIGHT".into());
    }
    let mut driver = Driver::new()?;
    let start_x = coordinate(&args, 1)?;
    let start_y = coordinate(&args, 2)?;
    if operation == "drag" {
        let end_x = coordinate(&args, 3)?;
        let end_y = coordinate(&args, 4)?;
        let width = coordinate(&args, 5)?;
        let height = coordinate(&args, 6)?;
        let button = button(args.get(7))?;
        driver.move_to(start_x, start_y, width, height)?;
        driver.button(button, true)?;
        for step in 1..=16 {
            let interpolate = |start: u32, end: u32| {
                (i64::from(start) + (i64::from(end) - i64::from(start)) * step / 16) as u32
            };
            driver.move_to(
                interpolate(start_x, end_x),
                interpolate(start_y, end_y),
                width,
                height,
            )?;
        }
        driver.button(button, false)?;
    } else {
        let width = coordinate(&args, 3)?;
        let height = coordinate(&args, 4)?;
        driver.move_to(start_x, start_y, width, height)?;
        match operation {
            "click" => {
                let button = button(args.get(5))?;
                driver.button(button, true)?;
                driver.button(button, false)?;
            }
            "wheel" => {
                let discrete = match args.get(5).map(String::as_str).unwrap_or("up") {
                    "up" => -1,
                    "down" => 1,
                    other => return Err(format!("unknown wheel direction: {other}").into()),
                };
                driver.pointer.axis_source(wl_pointer::AxisSource::Wheel);
                driver.pointer.axis_discrete(
                    time(),
                    wl_pointer::Axis::VerticalScroll,
                    f64::from(discrete) * 15.0,
                    discrete,
                );
                driver.frame()?;
            }
            _ => {}
        }
    }
    driver.finish()
}
