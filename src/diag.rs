//! Diagnostics: an activity log, crash reports with minidumps, and a hang watchdog.
//!
//! Files, next to the exe:
//! - `taskdeck.log`     activity log (start, exit and why, sleep/resume, screens,
//!                      taskbar restarts, a heartbeat every 10 min with memory use).
//!                      Rotated to `taskdeck.log.1` at 1 MB.
//! - `crashes/*.dmp`    a minidump per crash or hang (last 5 kept), to open in
//!                      WinDbg / Visual Studio together with `taskdeck.pdb`.
//!
//! Reading the log after the app vanished: if the last line is a heartbeat with
//! no "exit" or "CRASH" line after it, the process was killed from outside
//! (Task Manager, a parent job closing, logoff), not crashed.
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_NONE,
};
use windows::Win32::System::Diagnostics::Debug::{
    MiniDumpWithThreadInfo, MiniDumpWithUnloadedModules, MiniDumpWriteDump, SetUnhandledExceptionFilter,
    EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION,
};
use windows::Win32::System::JobObjects::IsProcessInJob;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{SendMessageTimeoutW, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_NULL};

const LOG_MAX: u64 = 1024 * 1024;
const DUMPS_KEPT: usize = 5;
const WATCHDOG_EVERY: Duration = Duration::from_secs(10);
const HANG_AFTER_MS: u32 = 5000;

static LOG: Mutex<()> = Mutex::new(());
static STARTED: Mutex<Option<Instant>> = Mutex::new(None);
static UI_HWND: AtomicIsize = AtomicIsize::new(0);
static IN_CRASH: AtomicBool = AtomicBool::new(false);

fn dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn now() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

fn stamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Appends one line to taskdeck.log (thread-safe; never fails loudly).
pub fn log(msg: &str) {
    let _guard = LOG.lock();
    let path = dir().join("taskdeck.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > LOG_MAX) {
        let _ = std::fs::rename(&path, dir().join("taskdeck.log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let tid = unsafe { GetCurrentThreadId() };
        let _ = writeln!(f, "{} [t{tid}] {msg}", now());
    }
}

/// True when this process was started inside a Windows job object (e.g. by a
/// terminal or tool host). Closing that job kills every process in it.
pub fn in_job() -> bool {
    let mut r = BOOL(0);
    unsafe { IsProcessInJob(GetCurrentProcess(), HANDLE::default(), &mut r).is_ok() && r.as_bool() }
}

fn memory_mb() -> (f64, f64) {
    let mut c = PROCESS_MEMORY_COUNTERS { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb);
    }
    (c.WorkingSetSize as f64 / 1048576.0, c.PagefileUsage as f64 / 1048576.0)
}

fn uptime() -> String {
    let secs = STARTED.lock().ok().and_then(|s| *s).map(|s| s.elapsed().as_secs()).unwrap_or(0);
    format!("{}h{:02}m", secs / 3600, (secs / 60) % 60)
}

/// Installs crash handlers and writes the start line.
pub fn init() {
    if let Ok(mut s) = STARTED.lock() {
        *s = Some(Instant::now());
    }
    std::panic::set_hook(Box::new(|p| {
        let bt = std::backtrace::Backtrace::force_capture();
        log(&format!("CRASH panic: {p}\nbacktrace:\n{bt}"));
        write_dump(None, "panic");
    }));
    unsafe {
        SetUnhandledExceptionFilter(Some(on_exception));
    }
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    log(&format!(
        "start v{} pid={} exe={exe} in_job={}",
        env!("CARGO_PKG_VERSION"),
        unsafe { GetCurrentProcessId() },
        in_job()
    ));
}

/// Heartbeat line (called every 10 min): shows the app was alive and its memory.
pub fn heartbeat() {
    let (ws, private) = memory_mb();
    log(&format!("alive uptime={} memory={ws:.1}MB private={private:.1}MB", uptime()));
}

/// Last-chance handler for native crashes (access violations etc.).
unsafe extern "system" fn on_exception(info: *const EXCEPTION_POINTERS) -> i32 {
    if IN_CRASH.swap(true, Ordering::SeqCst) {
        return 0;
    }
    if let Some(rec) = info.as_ref().and_then(|i| i.ExceptionRecord.as_ref()) {
        let exe = GetModuleHandleW(PCWSTR::null()).map(|h| h.0 as usize).unwrap_or(0);
        let addr = rec.ExceptionAddress as usize;
        log(&format!(
            "CRASH exception 0x{:08X} at 0x{addr:X} (exe+0x{:X}) params={:X?} uptime={}",
            rec.ExceptionCode.0,
            addr.wrapping_sub(exe),
            &rec.ExceptionInformation[..rec.NumberParameters.min(4) as usize],
            uptime()
        ));
    }
    write_dump(Some(info), "crash");
    0 // EXCEPTION_CONTINUE_SEARCH: let Windows record it too
}

/// Writes crashes/taskdeck-<time>-<tag>.dmp and keeps only the newest few.
pub fn write_dump(info: Option<*const EXCEPTION_POINTERS>, tag: &str) {
    let folder = dir().join("crashes");
    let _ = std::fs::create_dir_all(&folder);
    let path = folder.join(format!("taskdeck-{}-{tag}.dmp", stamp()));
    unsafe {
        let Ok(file) = CreateFileW(
            &HSTRING::from(path.as_os_str()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            HANDLE::default(),
        ) else {
            return;
        };
        let exc = info.map(|p| MINIDUMP_EXCEPTION_INFORMATION {
            ThreadId: GetCurrentThreadId(),
            ExceptionPointers: p as *mut _,
            ClientPointers: BOOL(0),
        });
        let ok = MiniDumpWriteDump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            file,
            MiniDumpWithThreadInfo | MiniDumpWithUnloadedModules,
            exc.as_ref().map(|e| e as *const _),
            None,
            None,
        );
        let _ = CloseHandle(file);
        log(&format!("dump {} -> {}", if ok.is_ok() { "written" } else { "FAILED" }, path.display()));
    }
    prune_dumps(&folder);
}

fn prune_dumps(folder: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(folder) else { return };
    let mut dumps: Vec<_> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "dmp"))
        .collect();
    dumps.sort_by_key(|e| e.file_name());
    while dumps.len() > DUMPS_KEPT {
        let _ = std::fs::remove_file(dumps.remove(0).path());
    }
}

/// Watches the UI thread: if it stops answering for 5 s, logs it and writes a
/// dump of all threads (once per hang), then logs when it recovers.
pub fn start_watchdog(ui: HWND) {
    UI_HWND.store(ui.0 as isize, Ordering::Relaxed);
    std::thread::spawn(|| {
        let mut hung_since: Option<Instant> = None;
        loop {
            std::thread::sleep(WATCHDOG_EVERY);
            let hwnd = HWND(UI_HWND.load(Ordering::Relaxed) as *mut _);
            let mut result = 0usize;
            let answered = unsafe {
                SendMessageTimeoutW(hwnd, WM_NULL, WPARAM(0), LPARAM(0), SMTO_BLOCK | SMTO_ABORTIFHUNG, HANG_AFTER_MS, Some(&mut result))
            }
            .0 != 0;
            match (answered, hung_since) {
                (false, None) => {
                    hung_since = Some(Instant::now());
                    log("HANG ui thread not responding for 5s; writing dump");
                    write_dump(None, "hang");
                }
                (true, Some(t)) => {
                    log(&format!("ui thread responding again after {}s", t.elapsed().as_secs() + 5));
                    hung_since = None;
                }
                _ => {}
            }
        }
    });
}
