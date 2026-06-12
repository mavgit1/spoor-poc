use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn init(verbose: bool) {
    VERBOSE.store(verbose, Ordering::SeqCst);
    if verbose {
        info("verbose logging enabled");
    }
}

pub fn info(msg: impl AsRef<str>) {
    eprintln!("[spoor] {}", msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    eprintln!("[spoor:warn] {}", msg.as_ref());
}

pub fn error(msg: impl AsRef<str>) {
    eprintln!("[spoor:error] {}", msg.as_ref());
}

pub fn debug(msg: impl AsRef<str>) {
    if VERBOSE.load(Ordering::SeqCst) {
        eprintln!("[spoor:debug] {}", msg.as_ref());
    }
}
