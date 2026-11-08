use std::sync::OnceLock;
use std::time::Instant;

pub fn trace(stage: &str) {
    static START: OnceLock<Option<Instant>> = OnceLock::new();
    if let Some(start) =
        START.get_or_init(|| std::env::var_os("WISP_TRACE_TIMING").map(|_| Instant::now()))
    {
        eprintln!("wisp-timing\t{}\t{stage}", start.elapsed().as_micros());
    }
}
