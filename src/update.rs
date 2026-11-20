//! Updates from GitHub releases: check (at most once a day, can be turned
//! off with `config.check_updates`, or on demand), download, verify,
//! install.
//!
//! This is ZoneLock's ONLY network use. It talks to api.github.com (latest
//! release of OhLasFar/ZoneLock) and downloads that release's assets.
//! Nothing about the user is sent: a GET with a generic User-Agent.
//!
//! Install:
//!   - installed copy (Inno Setup, `unins000.exe` next to the exe):
//!     download `zonelock-X-setup.exe`, verify its SHA-256 against the
//!     release's SHA256SUMS.txt, run it with /VERYSILENT /update=1 (the
//!     setup restarts ZoneLock), quit.
//!   - portable copy: download `zonelock-X-win-x64.exe`, verify, rename the
//!     running exe to `.old`, put the new one in place, start it with
//!     `--after-update <pid>` (it waits for us to exit, then deletes
//!     `.old`), quit.
//!
//! The pure part (versions, release JSON, checksums, SHA-256, schedule)
//! is unit-tested; the WinHTTP part runs on worker threads and reports to
//! the UI thread through `tray::post_update`.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

pub const REPO: &str = "OhLasFar/ZoneLock";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const DAY_SECS: u64 = 24 * 3600;

// ---------- pure ----------

/// "v1.2.3" / "1.2.3" -> (1, 2, 3). Pre-release suffixes are not used by
/// this project and are rejected.
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let mut it = s.split('.');
    let v = (
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    );
    if it.next().is_some() {
        return None;
    }
    Some(v)
}

pub fn is_newer(remote: &str, current: &str) -> bool {
    match (parse_version(remote), parse_version(current)) {
        (Some(r), Some(c)) => r > c,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

/// What we need from a release.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub setup_url: Option<String>,
    pub exe_url: Option<String>,
    pub sums_url: Option<String>,
}

impl Release {
    pub fn setup_name(&self) -> String {
        format!("zonelock-{}-setup.exe", self.version)
    }
    pub fn exe_name(&self) -> String {
        format!("zonelock-{}-win-x64.exe", self.version)
    }
}

/// Parse the GitHub "latest release" JSON. Only https download links on
/// github.com are accepted.
pub fn parse_release(json: &str) -> Result<Release, String> {
    let r: GhRelease = serde_json::from_str(json).map_err(|e| format!("bad release data ({e})"))?;
    if r.draft || r.prerelease {
        return Err("no stable release".into());
    }
    let version = r.tag_name.trim_start_matches(['v', 'V']).to_string();
    if parse_version(&version).is_none() {
        return Err(format!("unexpected version '{}'", r.tag_name));
    }
    let url_of = |name: &str| {
        r.assets
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
            .map(|a| a.browser_download_url.clone())
            .filter(|u| u.starts_with("https://github.com/"))
    };
    Ok(Release {
        setup_url: url_of(&format!("zonelock-{version}-setup.exe")),
        exe_url: url_of(&format!("zonelock-{version}-win-x64.exe")),
        sums_url: url_of("SHA256SUMS.txt"),
        page: if r.html_url.starts_with("https://github.com/") {
            r.html_url
        } else {
            format!("https://github.com/{REPO}/releases/latest")
        },
        version,
    })
}

/// The hex SHA-256 listed for `file` in a `sha256sum`-style text.
pub fn sum_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name.eq_ignore_ascii_case(file) && hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
}

/// Due for an automatic check: never checked, a day passed, or the clock
/// went backwards.
pub fn due(last_check: u64, now: u64) -> bool {
    last_check == 0 || now < last_check || now - last_check >= DAY_SECS
}

/// SHA-256 (FIPS 180-4), hex lowercase. Small and dependency-free.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, b) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

// ---------- state shown in the settings page ----------

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct UpdateView {
    pub current: String,
    /// idle | checking | uptodate | available | downloading | installing | error
    pub state: String,
    pub latest: String,
    pub message: String,
    /// Release page of `latest` (https://github.com/... only).
    pub page: String,
    /// Unix seconds of the last successful check (0 = never).
    pub last_check: u64,
}

struct Status {
    view: UpdateView,
    release: Option<Release>,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

fn with_status<R>(f: impl FnOnce(&mut Status) -> R) -> R {
    let mut g = STATUS.lock().unwrap();
    let s = g.get_or_insert_with(|| Status {
        view: UpdateView {
            current: CURRENT.to_string(),
            state: "idle".into(),
            last_check: load_memo().last_check,
            ..Default::default()
        },
        release: None,
    });
    f(s)
}

pub fn view() -> UpdateView {
    with_status(|s| s.view.clone())
}

fn set_state(state: &str, message: impl Into<String>) {
    let m = message.into();
    with_status(|s| {
        s.view.state = state.into();
        s.view.message = m;
    });
    crate::tray::post_update(false);
}

/// update.json: when we last checked, and which version we already
/// announced (so the tray balloon shows once per version).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct Memo {
    #[serde(default)]
    last_check: u64,
    #[serde(default)]
    announced: String,
}

fn memo_path() -> std::path::PathBuf {
    crate::config::config_dir().join("update.json")
}

fn load_memo() -> Memo {
    std::fs::read_to_string(memo_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_memo(m: &Memo) {
    if let Ok(t) = serde_json::to_string_pretty(m) {
        let _ = std::fs::write(memo_path(), t);
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Check GitHub on a worker thread. `auto` = scheduled check: shows the
/// tray balloon once per new version.
pub fn check(auto: bool) {
    if BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        set_state("checking", "Checking GitHub…");
        let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
        let res = net::get(&url, "application/vnd.github+json")
            .and_then(|b| String::from_utf8(b).map_err(|_| "bad response".to_string()))
            .and_then(|t| parse_release(&t));
        let mut memo = load_memo();
        match res {
            Ok(rel) => {
                memo.last_check = now_secs();
                let newer = is_newer(&rel.version, CURRENT);
                crate::log::line(format!("update check: latest {} (current {CURRENT})", rel.version));
                let announce = newer && auto && memo.announced != rel.version;
                if announce {
                    memo.announced = rel.version.clone();
                }
                with_status(|s| {
                    s.view.last_check = memo.last_check;
                    s.view.latest = rel.version.clone();
                    s.view.page = rel.page.clone();
                    s.release = Some(rel.clone());
                });
                save_memo(&memo);
                BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                if newer {
                    set_state("available", format!("ZoneLock {} is available.", rel.version));
                } else {
                    set_state("uptodate", "You have the latest version.");
                }
                if announce {
                    crate::tray::post_update(true);
                }
            }
            Err(e) => {
                crate::log::line(format!("update check failed: {e}"));
                BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
                set_state("error", format!("Could not check for updates ({e})."));
            }
        }
    });
}

/// Background schedule: first look shortly after start, then hourly
/// wake-ups that only hit the network once a day.
pub fn spawn_auto(shared: std::sync::Arc<crate::watcher::Shared>) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(20));
        loop {
            if !shared.running.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let on = shared.config.read().unwrap().check_updates;
            if on && due(load_memo().last_check, now_secs()) {
                check(true);
            }
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    });
}

/// Was ZoneLock installed by its setup (vs. a portable copy)?
fn installed(exe: &std::path::Path) -> bool {
    exe.parent().is_some_and(|d| d.join("unins000.exe").exists())
}

/// Download, verify and install the release found by the last check.
pub fn install() {
    let Some(rel) = with_status(|s| s.release.clone()) else {
        return;
    };
    if !is_newer(&rel.version, CURRENT) || BUSY.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let r = install_blocking(&rel);
        BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
        match r {
            Ok(()) => {
                crate::log::line(format!("update: installing {}, quitting", rel.version));
                crate::tray::post_quit();
            }
            Err(e) => {
                crate::log::line(format!("update failed: {e}"));
                set_state("error", format!("Update failed: {e}"));
            }
        }
    });
}

fn install_blocking(rel: &Release) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let setup = installed(&exe);
    let (url, name) = if setup {
        (rel.setup_url.clone(), rel.setup_name())
    } else {
        (rel.exe_url.clone(), rel.exe_name())
    };
    let url = url.ok_or_else(|| format!("{name} is missing from the release"))?;
    let sums_url = rel.sums_url.clone().ok_or("SHA256SUMS.txt is missing from the release")?;
    set_state("downloading", format!("Downloading ZoneLock {}…", rel.version));
    let sums = net::get(&sums_url, "*/*").and_then(|b| String::from_utf8(b).map_err(|_| "bad checksums".into()))?;
    let want = sum_for(&sums, &name).ok_or_else(|| format!("no checksum for {name}"))?;
    let bytes = net::get(&url, "application/octet-stream")?;
    let got = sha256_hex(&bytes);
    if got != want {
        return Err("the download does not match its checksum".into());
    }
    set_state("installing", format!("Installing ZoneLock {}…", rel.version));
    if setup {
        let dir = std::env::temp_dir().join("ZoneLock-update");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(&name);
        std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
        // the setup closes us anyway (CloseApplications); /update=1 makes
        // it start the new version when done
        win::run(&path, "/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /update=1")?;
    } else {
        let old = exe.with_extension("exe.old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(&exe, &old).map_err(|e| format!("cannot replace the exe ({e})"))?;
        if let Err(e) = std::fs::write(&exe, &bytes) {
            let _ = std::fs::rename(&old, &exe);
            return Err(format!("cannot write the new exe ({e})"));
        }
        let pid = std::process::id().to_string();
        std::process::Command::new(&exe)
            .args(["--after-update", &pid])
            .spawn()
            .map_err(|e| format!("cannot start the new version ({e})"))?;
    }
    Ok(())
}

/// Called first thing in main: `--after-update <pid>` waits for the old
/// process to exit (it holds the single-instance mutex) and removes the
/// old exe left by a portable update.
pub fn after_update_startup() {
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--after-update" {
            if let Some(pid) = args.next().and_then(|p| p.parse::<u32>().ok()) {
                win::wait_pid(pid, 15_000);
            }
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::fs::remove_file(exe.with_extension("exe.old"));
            }
        }
    }
}

/// Open a release page in the browser (only github.com URLs).
pub fn open_page() {
    let page = with_status(|s| s.view.page.clone());
    let url = if page.starts_with("https://github.com/") {
        page
    } else {
        format!("https://github.com/{REPO}/releases/latest")
    };
    win::open_url(&url);
}

mod win {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub fn run(path: &std::path::Path, args: &str) -> Result<(), String> {
        let p = wide(&path.to_string_lossy());
        let a = wide(args);
        let r = unsafe {
            ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), PCWSTR(a.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL)
        };
        if r.0 as isize > 32 {
            Ok(())
        } else {
            Err("could not start the installer".into())
        }
    }

    pub fn open_url(url: &str) {
        let u = wide(url);
        unsafe {
            let _ = ShellExecuteW(None, w!("open"), PCWSTR(u.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }

    pub fn wait_pid(pid: u32, ms: u32) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                let _ = WaitForSingleObject(h, ms);
                let _ = CloseHandle(h);
            }
        }
    }
}

/// HTTPS GET through WinHTTP (system proxy settings, TLS by Windows).
mod net {
    use std::ffi::c_void;

    use windows::core::{w, PCWSTR};
    use windows::Win32::Networking::WinHttp::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    struct H(*mut c_void);
    impl Drop for H {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }

    /// Max download: the installer is ~2.5 MB.
    const MAX: usize = 64 * 1024 * 1024;

    pub fn get(url: &str, accept: &str) -> Result<Vec<u8>, String> {
        let rest = url.strip_prefix("https://").ok_or("https only")?;
        let (host, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host_w, path_w) = (wide(host), wide(path));
        let headers = wide(&format!("Accept: {accept}\r\nX-GitHub-Api-Version: 2022-11-28\r\n"));
        unsafe {
            let session = H(WinHttpOpen(
                w!("ZoneLock-updater"),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ));
            if session.0.is_null() {
                return Err("network unavailable".into());
            }
            let _ = WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 60_000);
            let conn = H(WinHttpConnect(session.0, PCWSTR(host_w.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0));
            if conn.0.is_null() {
                return Err("cannot reach GitHub".into());
            }
            let req = H(WinHttpOpenRequest(
                conn.0,
                w!("GET"),
                PCWSTR(path_w.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ));
            if req.0.is_null() {
                return Err("cannot open the request".into());
            }
            WinHttpSendRequest(req.0, Some(&headers[..headers.len() - 1]), None, 0, 0, 0)
                .map_err(|_| "no connection to GitHub".to_string())?;
            WinHttpReceiveResponse(req.0, std::ptr::null_mut()).map_err(|_| "no answer from GitHub".to_string())?;
            let mut status = 0u32;
            let mut len = std::mem::size_of::<u32>() as u32;
            let _ = WinHttpQueryHeaders(
                req.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut c_void),
                &mut len,
                std::ptr::null_mut(),
            );
            if status != 200 {
                return Err(format!("GitHub answered {status}"));
            }
            let mut out = Vec::new();
            loop {
                let mut avail = 0u32;
                WinHttpQueryDataAvailable(req.0, &mut avail).map_err(|_| "download interrupted".to_string())?;
                if avail == 0 {
                    break;
                }
                let start = out.len();
                out.resize(start + avail as usize, 0);
                let mut read = 0u32;
                WinHttpReadData(req.0, out[start..].as_mut_ptr() as *mut c_void, avail, &mut read)
                    .map_err(|_| "download interrupted".to_string())?;
                out.truncate(start + read as usize);
                if out.len() > MAX {
                    return Err("download too large".into());
                }
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.0.10"), Some((1, 0, 10)));
        assert_eq!(parse_version("1.0"), None);
        assert_eq!(parse_version("1.0.0-beta"), None);
        assert!(is_newer("v1.0.10", "1.0.9"));
        assert!(is_newer("2.0.0", "1.9.9"));
        assert!(!is_newer("1.0.2", "1.0.2"));
        assert!(!is_newer("1.0.1", "1.0.2"));
        assert!(!is_newer("garbage", "1.0.0"));
    }

    #[test]
    fn release_json_is_read() {
        let json = r#"{"tag_name":"v1.0.4","html_url":"https://github.com/OhLasFar/ZoneLock/releases/tag/v1.0.4",
          "draft":false,"prerelease":false,"assets":[
          {"name":"SHA256SUMS.txt","browser_download_url":"https://github.com/OhLasFar/ZoneLock/releases/download/v1.0.4/SHA256SUMS.txt"},
          {"name":"zonelock-1.0.4-setup.exe","browser_download_url":"https://github.com/OhLasFar/ZoneLock/releases/download/v1.0.4/zonelock-1.0.4-setup.exe"},
          {"name":"zonelock-1.0.4-win-x64.exe","browser_download_url":"https://evil.example/zonelock.exe"}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.version, "1.0.4");
        assert!(r.setup_url.unwrap().ends_with("zonelock-1.0.4-setup.exe"));
        assert!(r.sums_url.is_some());
        assert_eq!(r.exe_url, None, "links outside github.com are refused");
        assert!(parse_release(r#"{"tag_name":"v2.0.0","prerelease":true}"#).is_err());
        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn checksums_and_schedule() {
        let sums = "AbC123  other.zip\n0123456789abcdef0123456789abcdef0123456789abcdef0123456789ABCDEF  zonelock-1.0.4-setup.exe\n";
        assert_eq!(
            sum_for(sums, "zonelock-1.0.4-setup.exe").as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert_eq!(sum_for(sums, "other.zip"), None, "not a sha-256");
        assert_eq!(sum_for(sums, "missing.exe"), None);
        assert!(due(0, 100));
        assert!(!due(1000, 1000 + 3600));
        assert!(due(1000, 1000 + DAY_SECS));
        assert!(due(5000, 10), "clock went back");
    }

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(sha256_hex(&million_a), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
    }
}
