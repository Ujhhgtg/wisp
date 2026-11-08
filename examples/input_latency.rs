use std::error::Error;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use libghostty_vt::key;
use portable_pty::CommandBuilder;
use wisp::config::Config;
use wisp::terminal::{Command, Session, SessionEvent};

const CHILD: &str = r#"
import os, sys, termios, tty
original = termios.tcgetattr(0)
tty.setraw(0)
try:
    os.write(1, b'\x1b[2J\x1b[H\x1b]2;latency-ready\x1b\\')
    count = 0
    while True:
        data = os.read(0, 1)
        if not data or data == b'\x04':
            break
        count += 1
        os.write(1, data + b'\x1b]2;latency-echo:' + str(count).encode() + b'\x1b\\')
finally:
    termios.tcsetattr(0, termios.TCSANOW, original)
"#;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let samples = arguments
        .first()
        .map(|argument| argument.parse::<u16>())
        .transpose()?
        .unwrap_or(100);
    if samples == 0 || samples > 1000 {
        return Err("sample count must be between 1 and 1000".into());
    }
    let intervals = if arguments.len() > 1 {
        arguments[1..]
            .iter()
            .map(|argument| argument.parse::<u64>())
            .collect::<Result<Vec<_>, _>>()?
    } else {
        vec![20, 35, 50]
    };
    println!("Production Session PTY key-to-echo and key-to-snapshot latency");
    println!("samples={samples}; GUI/GPU presentation is measured separately");
    for interval in intervals {
        run(samples, Duration::from_millis(interval))?;
    }
    Ok(())
}

fn run(samples: u16, interval: Duration) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (wake_tx, wake_rx) = mpsc::channel();
    let wake = Arc::new(move || {
        let _ = wake_tx.send(());
    });
    let mut command = CommandBuilder::new("python3");
    command.args(["-u", "-c", CHILD]);
    let session =
        Session::spawn_command_with_waker(Config::default(), samples + 16, 8, command, wake)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ready = false;
    while !ready {
        check_timeout(deadline, "child startup")?;
        while let Some(event) = session.try_event() {
            match event {
                SessionEvent::Title(title) if title == "latency-ready" => ready = true,
                SessionEvent::Error(error) => return Err(error.into()),
                SessionEvent::Exited => return Err("latency child exited before ready".into()),
                _ => {}
            }
        }
        if !ready {
            let _ = wake_rx.recv_timeout(Duration::from_millis(10));
        }
    }
    while session.try_snapshot().is_some() {}
    while wake_rx.try_recv().is_ok() {}

    let started = Instant::now();
    let mut echo_latencies = Vec::with_capacity(usize::from(samples));
    let mut snapshot_latencies = Vec::with_capacity(usize::from(samples));
    for sample in 1..=samples {
        let scheduled = started + interval * u32::from(sample);
        if let Some(delay) = scheduled.checked_duration_since(Instant::now()) {
            thread::sleep(delay);
        }
        let sent = Instant::now();
        session.send(Command::Key {
            key: key::Key::X,
            mods: key::Mods::empty(),
            action: key::Action::Press,
            text: Some("x".to_owned()),
            unshifted: Some('x'),
        });
        let mut echo = None;
        let mut snapshot = None;
        let deadline = sent + Duration::from_secs(2);
        while echo.is_none() || snapshot.is_none() {
            check_timeout(deadline, "key echo/snapshot")?;
            while let Some(event) = session.try_event() {
                match event {
                    SessionEvent::Title(title) if title == format!("latency-echo:{sample}") => {
                        echo = Some(sent.elapsed());
                    }
                    SessionEvent::Error(error) => return Err(error.into()),
                    SessionEvent::Exited => return Err("latency child exited before echo".into()),
                    _ => {}
                }
            }
            if let Some(frame) = session.try_snapshot() {
                if frame.cursor.column == sample
                    && frame.cursor.row == 0
                    && frame.cells[usize::from(sample - 1)].text == "x"
                {
                    snapshot = Some(sent.elapsed());
                }
            }
            if echo.is_none() || snapshot.is_none() {
                let _ = wake_rx.recv_timeout(Duration::from_millis(2));
            }
        }
        echo_latencies.push(echo.expect("measured echo"));
        snapshot_latencies.push(snapshot.expect("measured snapshot"));
    }
    session.send(Command::Input(vec![4]));
    print_latencies(interval, "echo", &mut echo_latencies);
    print_latencies(interval, "snapshot", &mut snapshot_latencies);
    Ok(())
}

fn check_timeout(deadline: Instant, stage: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    if Instant::now() >= deadline {
        Err(format!("timed out during {stage}").into())
    } else {
        Ok(())
    }
}

fn print_latencies(interval: Duration, stage: &str, latencies: &mut [Duration]) {
    latencies.sort_unstable();
    let percentile = |percentage: usize| {
        latencies[((latencies.len() * percentage).div_ceil(100)).saturating_sub(1)].as_secs_f64()
            * 1000.0
    };
    println!(
        "interval={:>2}ms {stage:>8}: p50={:.3}ms p95={:.3}ms p99={:.3}ms max={:.3}ms",
        interval.as_millis(),
        percentile(50),
        percentile(95),
        percentile(99),
        latencies.last().expect("nonempty samples").as_secs_f64() * 1000.0,
    );
}
