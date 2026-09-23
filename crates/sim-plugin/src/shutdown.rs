//! Signal handlers only set a flag; producer loops perform normal cleanup.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};

static STOP: OnceLock<Arc<AtomicBool>> = OnceLock::new();

pub fn install() -> Result<(), String> {
    let flag = STOP.get_or_init(|| Arc::new(AtomicBool::new(false)));
    for signal in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(signal, flag.clone()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn requested() -> bool {
    STOP.get().is_some_and(|flag| flag.load(Ordering::Relaxed))
}

pub fn request() {
    if let Some(flag) = STOP.get() {
        flag.store(true, Ordering::Relaxed);
    }
}
