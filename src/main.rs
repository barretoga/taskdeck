//! TaskDeck: a strip of controls embedded in the Windows taskbar — mic and speaker
//! mute (Voicemeeter or Windows), a media player (Spotify or browser video) with
//! audio bars, and the mouse battery.
#![windows_subsystem = "windows"]

use std::cell::RefCell;
use std::time::{Duration, Instant};

use taskdeck::audio::{Audio, Flow};
use taskdeck::config::{self, Config, Mode};
use taskdeck::media::{self, Cmd};
use taskdeck::render::{Item, MediaView, Rgb, View};
use taskdeck::spectrum::{self, BANDS};
use taskdeck::voicemeeter::Voicemeeter;
use taskdeck::widget::{self, Widget, WM_WIDGET_CLICK, WM_WIDGET_HOVER, WM_WIDGET_MENU};
use taskdeck::demo::{self, Demo};
use taskdeck::{battery, diag, resolve_mic, targets};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

const TIMER_STATE: usize = 1;
const TIMER_POSITION: usize = 2;
const TIMER_BATTERY: usize = 3;
const STATE_MS: u32 = 250;
const POSITION_MS: u32 = 500;
const BATTERY_MS: u32 = 60_000;
const TIMER_BARS: usize = 4;
const BARS_MS: u32 = 33; // ~30 fps, only while music plays
const WM_MEDIA: u32 = WM_APP + 20;
const WM_BATTERY: u32 = WM_APP + 21;
const TIMER_HEARTBEAT: usize = 5;
const HEARTBEAT_MS: u32 = 10 * 60 * 1000;
/// Power events (WM_POWERBROADCAST).
const PBT_APMSUSPEND: usize = 0x4;
const PBT_APMRESUMESUSPEND: usize = 0x7;
const PBT_APMRESUMEAUTOMATIC: usize = 0x12;

const CMD_EXIT: usize = 100;
const CMD_AUTOSTART: usize = 101;
const CMD_OPEN_SETTINGS: usize = 102;
const CMD_RELOAD: usize = 103;

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const APP_NAME: PCWSTR = w!("TaskDeck");

struct App {
    hwnd: HWND,
    cfg: Config,
    vm: Option<Voicemeeter>,
    mic_targets: Option<Vec<String>>, // resolved lazily once Voicemeeter is up
    audio: Option<Audio>,
    view: View,
    battery_name: String,
    ignore_poll_until: Instant,
    widgets: Vec<Widget>, // one per taskbar (one per screen)
    taskbar_created: u32,
    bars: [f32; BANDS],
    bars_running: bool,
    media_source: Option<&'static str>,
    /// Demo mode: made-up data for screenshots (see demo.rs).
    demo: Option<Demo>,
    demo_muted: (bool, bool),
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app state. Some Win32/COM calls pump messages while they wait,
/// so a timer can fire while a handler is still running. That nested call is
/// skipped (the timer simply runs again on its next tick) instead of panicking.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| {
        let mut guard = a.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

/// A resident taskbar app must not live inside someone else's job object: when a
/// terminal or tool host that started it closes its job, Windows kills every
/// process in it, with no crash report. If we were started like that, start a
/// fresh copy whose parent is the taskbar's Explorer (outside any job; a child
/// inherits the job of the parent it is created for) and let this one exit.
///
/// The copy gets `--detached` and never relaunches again, so this cannot loop.
/// Pass `--attached` to stay attached on purpose (e.g. while debugging).
fn relaunch_outside_job() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if !diag::in_job() || args.iter().any(|a| a == "--attached") {
        return false;
    }
    if args.iter().any(|a| a == "--detached") {
        diag::log("still inside a job after relaunching; staying in this process");
        return false;
    }
    let r = unsafe { spawn_from_shell() };
    diag::log(&format!("started inside a job object; relaunched as a child of Explorer: {r:?}"));
    r.is_ok()
}

/// Starts this exe with `--detached`, parented to the shell (taskbar) Explorer.
unsafe fn spawn_from_shell() -> windows::core::Result<()> {
    use std::ffi::c_void;
    use windows::core::PWSTR;
    use windows::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, OpenProcess,
        UpdateProcThreadAttribute, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_CREATE_PROCESS,
        PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PARENT_PROCESS, STARTUPINFOEXW,
    };

    let shell = GetShellWindow();
    if shell.is_invalid() {
        return Err(windows::core::Error::from_win32());
    }
    let mut shell_pid = 0u32;
    GetWindowThreadProcessId(shell, Some(&mut shell_pid));
    let parent = OpenProcess(PROCESS_CREATE_PROCESS, false, shell_pid)?;

    let mut size = 0usize;
    let _ = InitializeProcThreadAttributeList(LPPROC_THREAD_ATTRIBUTE_LIST::default(), 1, 0, &mut size);
    let mut buf = vec![0u8; size];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr() as *mut c_void);
    let result = (|| {
        InitializeProcThreadAttributeList(list, 1, 0, &mut size)?;
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_PARENT_PROCESS as usize,
            Some(&parent as *const HANDLE as *const c_void),
            std::mem::size_of::<HANDLE>(),
            None,
            None,
        )?;
        let exe = std::env::current_exe().map_err(|_| windows::core::Error::from_win32())?;
        let dir = HSTRING::from(exe.parent().map(|d| d.as_os_str()).unwrap_or_default());
        let mut cmd: Vec<u16> = format!("\"{}\" --detached", exe.display()).encode_utf16().chain([0]).collect();
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = list;
        let mut pi = PROCESS_INFORMATION::default();
        CreateProcessW(
            PCWSTR::null(),
            PWSTR(cmd.as_mut_ptr()),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT,
            None,
            &dir,
            &si.StartupInfo,
            &mut pi,
        )?;
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
        Ok(())
    })();
    DeleteProcThreadAttributeList(list);
    let _ = CloseHandle(parent);
    result
}
fn main() {
    unsafe {
        diag::init();
        // Self-tests for the crash reporting (run with --attached so they stay in this process).
        if std::env::args().any(|a| a == "--test-panic") {
            panic!("test panic requested with --test-panic");
        }
        // Before taking the single-instance mutex, so the relaunched copy can take it.
        if relaunch_outside_job() {
            return;
        }
        let _mutex = CreateMutexW(None, true, w!("Local\\TaskDeck.SingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            diag::log("exit: another TaskDeck is already running");
            return;
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        migrate_autostart();

        let hinstance: HINSTANCE = GetModuleHandleW(None).unwrap().into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: w!("TaskDeckMain"),
            ..Default::default()
        };
        RegisterClassW(&class);
        // Hidden top-level window: receives timers, widget events and TaskbarCreated.
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("TaskDeckMain"),
            w!("TaskDeck"),
            WS_OVERLAPPED,
            0, 0, 0, 0,
            None, None, hinstance, None,
        )
        .expect("CreateWindowExW");

        let cfg = config::load();
        let demo = demo::from_args();
        if let Some(d) = demo {
            diag::log(&format!("demo mode: {d:?}"));
        }
        let app = App {
            hwnd,
            vm: if demo.is_some() { None } else { connect_vm(&cfg) },
            cfg,
            mic_targets: None,
            audio: Audio::new().ok(),
            view: View { light: light_theme(), ..Default::default() },
            battery_name: String::new(),
            ignore_poll_until: Instant::now(),
            widgets: Vec::new(),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            bars: [0.0; BANDS],
            bars_running: false,
            media_source: None,
            demo,
            demo_muted: demo.map(|d| (d.muted, d.muted)).unwrap_or_default(),
        };
        if app.demo.is_some() {
            // Nothing real is read or controlled in demo mode.
        } else if app.cfg.show_battery {
            let (vid, pid) = app.cfg.mouse_receiver;
            battery::start_receiver(hwnd, WM_BATTERY, vid, pid);
        }
        if app.cfg.show_media && app.demo.is_none() {
            media::set_sources(&app.cfg.media_sources);
            media::start(hwnd, WM_MEDIA);
            if app.cfg.show_bars {
                spectrum::start();
            }
        }
        APP.with(|a| *a.borrow_mut() = Some(app));

        with_app(|app| {
            if app.demo.is_some() {
                update_media(app);
            }
            poll_state(app);
            refresh_battery(app);
            sync_widgets(app);
            redraw(app);
        });
        SetTimer(hwnd, TIMER_STATE, STATE_MS, None);
        SetTimer(hwnd, TIMER_POSITION, POSITION_MS, None);
        SetTimer(hwnd, TIMER_BATTERY, BATTERY_MS, None);
        SetTimer(hwnd, TIMER_HEARTBEAT, HEARTBEAT_MS, None);
        diag::start_watchdog(hwnd);
        diag::heartbeat();
        if std::env::args().any(|a| a == "--test-power") {
            // Simulates sleep + resume to exercise the power handling.
            let _ = PostMessageW(hwnd, WM_POWERBROADCAST, WPARAM(PBT_APMSUSPEND), LPARAM(0));
            let _ = PostMessageW(hwnd, WM_POWERBROADCAST, WPARAM(PBT_APMRESUMEAUTOMATIC), LPARAM(0));
        }
        if std::env::args().any(|a| a == "--test-hang") {
            // Blocks the UI thread so the watchdog reports a hang, then recovers.
            std::thread::sleep(Duration::from_secs(20));
        }

        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0).0;
            if r == 0 {
                diag::log(&format!("exit: message loop ended normally (code {})", msg.wParam.0));
                break;
            }
            if r == -1 {
                diag::log(&format!("exit: GetMessage failed: {:?}", windows::core::Error::from_win32()));
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn connect_vm(cfg: &Config) -> Option<Voicemeeter> {
    if cfg.mode == Mode::Windows {
        None
    } else {
        Voicemeeter::connect(&cfg.voicemeeter_dll)
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            with_app(|app| match wparam.0 {
                TIMER_STATE => {
                    if poll_state(app) {
                        redraw(app);
                    }
                }
                TIMER_POSITION => {
                    if sync_widgets(app) {
                        redraw(app);
                    }
                }
                TIMER_BATTERY => {
                    if refresh_battery(app) {
                        redraw(app);
                    }
                }
                TIMER_BARS => tick_bars(app),
                TIMER_HEARTBEAT => diag::heartbeat(),
                _ => {}
            });
            LRESULT(0)
        }
        WM_BATTERY => {
            with_app(|app| {
                if refresh_battery(app) {
                    redraw(app);
                }
            });
            LRESULT(0)
        }
        WM_MEDIA => {
            with_app(|app| {
                update_media(app);
                redraw(app);
            });
            LRESULT(0)
        }
        WM_WIDGET_CLICK => {
            with_app(|app| {
                match Item::from_id(wparam.0) {
                    Some(Item::Mic) => toggle(app, Item::Mic),
                    Some(Item::Speaker) => toggle(app, Item::Speaker),
                    Some(Item::Battery) => {
                        refresh_battery(app);
                    }
                    Some(Item::Previous) => media::send(Cmd::Previous),
                    Some(Item::Next) => media::send(Cmd::Next),
                    Some(Item::PlayPause) => {
                        if app.demo.is_none() {
                            media::send(Cmd::PlayPause);
                        } else if !app.bars_running {
                            SetTimer(app.hwnd, TIMER_BARS, BARS_MS, None);
                            app.bars_running = true;
                        }
                        // Show the new state right away; the media thread confirms it.
                        if let Some(m) = &mut app.view.media {
                            m.playing = !m.playing;
                        }
                    }
                    Some(Item::Media) => {
                        // Bring the playing app (Spotify or the browser) to the front.
                        if let Some(info) = media::current() {
                            media::show_app(info.source, info.title);
                        }
                    }
                    Some(Item::Bars) | None => {}
                }
                redraw(app);
            });
            LRESULT(0)
        }
        WM_WIDGET_HOVER => {
            with_app(redraw);
            LRESULT(0)
        }
        WM_WIDGET_MENU => {
            show_menu(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            match wparam.0 & 0xFFFF {
                CMD_EXIT => {
                    diag::log("exit requested from the menu");
                    let _ = DestroyWindow(hwnd);
                }
                CMD_AUTOSTART => set_autostart(!autostart_enabled()),
                CMD_OPEN_SETTINGS => {
                    let _ = config::load(); // make sure the file exists
                    let path = HSTRING::from(config::path().as_os_str());
                    ShellExecuteW(None, w!("open"), &path, None, None, SW_SHOWNORMAL);
                }
                CMD_RELOAD => {
                    diag::log("settings reloaded");
                    with_app(|app| {
                        app.cfg = config::load();
                        app.vm = None; // log out before logging in again
                        app.vm = connect_vm(&app.cfg);
                        app.mic_targets = None;
                        poll_state(app);
                        refresh_battery(app);
                        if app.cfg.show_media {
                            media::set_sources(&app.cfg.media_sources);
                            media::start(app.hwnd, WM_MEDIA);
                            if app.cfg.show_bars {
                                spectrum::start();
                            }
                        }
                        update_media(app);
                        redraw(app);
                    });
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            with_app(|app| {
                app.view.light = light_theme();
                redraw(app);
            });
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_POWERBROADCAST => {
            match wparam.0 {
                PBT_APMSUSPEND => diag::log("power: going to sleep"),
                PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC => {
                    diag::log("power: resumed from sleep");
                    with_app(|app| {
                        // Audio devices and Voicemeeter may have restarted while asleep.
                        app.mic_targets = None;
                        poll_state(app);
                        sync_widgets(app);
                        redraw(app);
                    });
                }
                _ => {}
            }
            LRESULT(1)
        }
        WM_QUERYENDSESSION => {
            diag::log("windows is ending the session (logoff/shutdown)");
            LRESULT(1)
        }
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                diag::log("exit: session ending");
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            diag::log(&format!("display changed: {}x{}", lparam.0 & 0xFFFF, (lparam.0 >> 16) & 0xFFFF));
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            diag::log("main window destroyed; shutting down");
            with_app(|app| {
                app.widgets.clear();
                app.vm = None;
            });
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => {
            let is_taskbar_created = with_app(|app| msg != 0 && msg == app.taskbar_created).unwrap_or(false);
            if is_taskbar_created {
                diag::log("taskbar recreated (Explorer restarted); rebuilding widgets");
                with_app(|app| {
                    app.widgets.clear();
                    sync_widgets(app);
                    redraw(app);
                });
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

fn redraw(app: &mut App) {
    app.view.media_text_w = app.cfg.media_width;
    let bar_color = match app.cfg.bars_color {
        Some((r, g, b)) => Rgb(r as u32, g as u32, b as u32),
        None if app.view.light => Rgb(0x1A, 0x1A, 0x1A),
        None => Rgb(0xFF, 0xFF, 0xFF),
    };
    for w in &mut app.widgets {
        w.set_bar_color(bar_color);
        let view = View { hover: widget::hover_for(w.hwnd), ..app.view.clone() };
        w.render(&view, &app.bars);
    }
}

fn update_media(app: &mut App) {
    if app.demo.is_some() {
        let playing = app.view.media.as_ref().map_or(true, |m| m.playing);
        app.view.media = Some(demo::media(playing));
        if !app.bars_running {
            unsafe { SetTimer(app.hwnd, TIMER_BARS, BARS_MS, None) };
            app.bars_running = true;
        }
        return;
    }
    let info = if app.cfg.show_media { media::current() } else { None };
    let source = info.as_ref().map(|i| i.source.name);
    let shown = app.view.media.is_some();
    if source.is_some() != shown || source.is_some_and(|s| Some(s) != app.media_source) {
        diag::log(&format!("player source: {}", source.unwrap_or("none")));
    }
    app.media_source = source;
    let playing = info.as_ref().is_some_and(|i| i.playing);
    // The bars listen to whichever app the player shows.
    spectrum::set_target(info.as_ref().map(|i| i.source.exe));
    app.view.media = info.map(|i| MediaView { title: i.title, artist: i.artist, playing: i.playing, cover: i.cover });
    spectrum::set_active(playing && app.cfg.show_bars);
    if playing && app.cfg.show_bars && !app.bars_running {
        unsafe { SetTimer(app.hwnd, TIMER_BARS, BARS_MS, None) };
        app.bars_running = true;
    }
}

/// One animation frame: rise fast, fall slowly. Stops itself once paused and settled.
fn tick_bars(app: &mut App) {
    let playing = app.view.media.as_ref().is_some_and(|m| m.playing);
    let target = match app.demo {
        Some(_) if playing => demo::levels(),
        Some(_) => [0.0; BANDS],
        None => spectrum::levels(),
    };
    let mut moving = false;
    for (d, &t) in app.bars.iter_mut().zip(target.iter()) {
        *d = if t > *d { *d + (t - *d) * 0.6 } else { (*d - 0.045).max(t) };
        moving |= *d > 0.002;
    }
    for w in &mut app.widgets {
        w.render_bars(&app.bars);
    }
    if !moving && !playing {
        unsafe {
            let _ = KillTimer(app.hwnd, TIMER_BARS);
        }
        app.bars_running = false;
    }
}

/// Adds widgets for new taskbars (screen plugged in, Explorer restarted), drops
/// widgets whose taskbar is gone, and keeps the rest in place.
/// Returns true when a redraw is needed.
fn sync_widgets(app: &mut App) -> bool {
    let bars = widget::find_taskbars();
    let before = app.widgets.len();
    app.widgets.retain(|w| w.alive() && bars.contains(&w.taskbar()));
    if app.widgets.len() != before {
        diag::log(&format!("{} widget(s) lost their taskbar", before - app.widgets.len()));
    }
    let mut need = false;
    for tb in bars {
        if !app.widgets.iter().any(|w| w.taskbar() == tb) {
            match Widget::create(app.hwnd, tb) {
                Some(w) => {
                    app.widgets.push(w);
                    need = true;
                    diag::log(&format!("widget created on taskbar {:?} ({} total)", tb.0, app.widgets.len()));
                }
                None => diag::log(&format!("could not create widget on taskbar {:?}", tb.0)),
            }
        }
    }
    for w in &mut app.widgets {
        need |= w.reposition();
    }
    need
}

/// Voicemeeter, if it should be used right now.
fn active_vm(app: &mut App) -> Option<&Voicemeeter> {
    let vm = app.vm.as_ref()?;
    vm.kind()?; // not running
    vm.refresh();
    if app.mic_targets.is_none() {
        app.mic_targets = Some(targets(&resolve_mic(vm, &app.cfg.mic)));
    }
    app.vm.as_ref()
}

fn vm_muted(vm: &Voicemeeter, list: &[String]) -> Option<bool> {
    list.first().and_then(|t| vm.get(&format!("{t}.Mute"))).map(|v| v >= 0.5)
}

/// Reads mute states. Returns true when something changed.
fn poll_state(app: &mut App) -> bool {
    if app.demo.is_some() {
        let (mic, spk) = (Some(app.demo_muted.0), Some(app.demo_muted.1));
        let changed = mic != app.view.mic || spk != app.view.speaker;
        app.view.mic = mic;
        app.view.speaker = spk;
        return changed;
    }
    if Instant::now() < app.ignore_poll_until {
        return false; // Voicemeeter applies sets asynchronously; avoid flicker
    }
    let speakers = targets(&app.cfg.speakers);
    let (mic, spk) = if let Some(vm) = active_vm(app) {
        let vm: *const Voicemeeter = vm;
        let vm = unsafe { &*vm };
        let mic_list = app.mic_targets.clone().unwrap_or_default();
        (vm_muted(vm, &mic_list), vm_muted(vm, &speakers))
    } else if app.cfg.mode == Mode::Voicemeeter {
        (None, None)
    } else {
        let a = app.audio.as_ref();
        (a.and_then(|a| a.is_muted(Flow::Mic)), a.and_then(|a| a.is_muted(Flow::Speakers)))
    };
    let changed = mic != app.view.mic || spk != app.view.speaker;
    app.view.mic = mic;
    app.view.speaker = spk;
    changed
}

fn toggle(app: &mut App, item: Item) {
    if app.demo.is_some() {
        // Picture only: the real microphone/speakers are never touched in demo mode.
        if item == Item::Mic {
            app.demo_muted.0 = !app.demo_muted.0;
        } else {
            app.demo_muted.1 = !app.demo_muted.1;
        }
        poll_state(app);
        return;
    }
    let current = if item == Item::Mic { app.view.mic } else { app.view.speaker };
    let Some(current) = current else { return };
    let target = !current;
    let ok = if active_vm(app).is_some() {
        let list = if item == Item::Mic { app.mic_targets.clone().unwrap_or_default() } else { targets(&app.cfg.speakers) };
        let vm = app.vm.as_ref().unwrap();
        let value = if target { 1.0 } else { 0.0 };
        list.iter().fold(!list.is_empty(), |ok, t| vm.set(&format!("{t}.Mute"), value) && ok)
    } else if app.cfg.mode != Mode::Voicemeeter {
        let flow = if item == Item::Mic { Flow::Mic } else { Flow::Speakers };
        app.audio.as_ref().is_some_and(|a| a.set_muted(flow, target))
    } else {
        false
    };
    if ok {
        if item == Item::Mic {
            app.view.mic = Some(target);
        } else {
            app.view.speaker = Some(target);
        }
        app.ignore_poll_until = Instant::now() + Duration::from_millis(400);
    }
}

/// Returns true when the shown value changed.
fn refresh_battery(app: &mut App) -> bool {
    let now = match app.demo {
        Some(d) => Some(("Demo Mouse".to_string(), d.battery)),
        None if app.cfg.show_battery => battery::read(),
        None => None,
    };
    let pct = now.as_ref().map(|(_, p)| *p);
    app.battery_name = now.map(|(n, _)| n).unwrap_or_default();
    let changed = pct != app.view.battery;
    app.view.battery = pct;
    changed
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let info = with_app(|app| {
            let backend = match app.vm.as_ref().and_then(|v| v.kind()) {
                Some(_) => format!(
                    "Voicemeeter: mic {} / speakers {}",
                    app.mic_targets.as_ref().map(|t| t.join(",")).unwrap_or_default(),
                    app.cfg.speakers
                ),
                None => "Windows default devices".to_string(),
            };
            let battery = match app.view.battery {
                Some(p) => match battery::receiver_raw() {
                    Some(raw) => format!("{}: {p}% battery  (report {raw})", app.battery_name),
                    None => format!("{}: {p}% battery", app.battery_name),
                },
                None => "Mouse battery: no report yet (mouse off or asleep?)".to_string(),
            };
            (backend, battery)
        });
        if let Some((backend, battery)) = info {
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &HSTRING::from(backend));
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &HSTRING::from(battery));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        }
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN_SETTINGS, w!("Open settings"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_RELOAD, w!("Reload settings"));
        let auto = if autostart_enabled() { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(menu, auto, CMD_AUTOSTART, w!("Start with Windows"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("Exit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

fn light_theme() -> bool {
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
        .is_ok()
            && value == 1
    }
}

/// The app used to be called MouseBar: if it was set to start with Windows,
/// move that entry to TaskDeck (pointing at this exe).
fn migrate_autostart() {
    unsafe {
        let legacy = w!("MouseBar");
        if RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, legacy, RRF_RT_REG_SZ, None, None, None).is_ok() {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, legacy);
            set_autostart(true);
        }
    }
}

fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, APP_NAME, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

fn set_autostart(enable: bool) {
    unsafe {
        if enable {
            let Ok(exe) = std::env::current_exe() else { return };
            let value: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                APP_NAME,
                REG_SZ.0,
                Some(value.as_ptr() as *const _),
                (value.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, APP_NAME);
        }
    }
}
