//! Tiny file logger. All diagnostics stay on the local disk.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ENABLED: AtomicBool = AtomicBool::new(false);
static PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static BUF: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn init(path: PathBuf, enabled: bool) {
    *PATH.lock().unwrap() = Some(path);
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Append one line. Best-effort; never panics on IO error.
pub fn line(msg: impl AsRef<str>) {
    if !is_enabled() {
        return;
    }
    let msg = msg.as_ref();
    let stamp = {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        format!("{}.{:03}", t.as_secs(), t.subsec_millis())
    };
    let line = format!("{stamp} {msg}\n");
    if let Some(p) = PATH.lock().unwrap().as_ref() {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
            // rotate crudely at ~1 MiB
            if let Ok(meta) = f.metadata() {
                if meta.len() > 1_048_576 {
                    drop(f);
                    let bak = p.with_extension("log.bak");
                    let _ = std::fs::rename(&p, &bak);
                    if let Ok(mut f2) =
                        OpenOptions::new().create(true).append(true).open(&p)
                    {
                        let _ = f2.write_all(line.as_bytes());
                    }
                    return;
                }
            }
            let _ = f.write_all(line.as_bytes());
            return;
        }
    }
    BUF.lock().unwrap().push(line);
}
