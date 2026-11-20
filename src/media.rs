//! "Now playing" via Windows' Global System Media Transport Controls (the same
//! source as the volume flyout): Spotify, and browsers playing YouTube or any
//! other site that uses the Media Session API. No logins or API keys needed.
//!
//! The player follows the *active* source: whatever is playing (Windows' own
//! "current session" wins a tie); when nothing plays it stays on the last one.
//! Runs on its own thread; the UI thread is notified with a posted message.
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use windows::core::Interface;
use windows::Graphics::Imaging::{
    BitmapAlphaMode, BitmapBounds, BitmapDecoder, BitmapInterpolationMode, BitmapPixelFormat, BitmapTransform,
    ColorManagementMode, ExifOrientationMode,
};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::{IRandomAccessStream, IRandomAccessStreamReference};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::render::Cover;

/// Cover art is decoded at this size (centre-cropped to a square) and scaled when drawn.
const COVER_PX: u32 = 96;
const POLL: Duration = Duration::from_millis(1000);

/// A media app TaskDeck can follow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Source {
    /// Name used in taskdeck.ini (`media_sources`).
    pub key: &'static str,
    /// Text found in the app's media id (lower case).
    patterns: &'static [&'static str],
    /// Process to capture audio from / bring to the front.
    pub exe: &'static str,
    pub name: &'static str,
}

pub const SOURCES: &[Source] = &[
    Source { key: "spotify", patterns: &["spotify"], exe: "spotify.exe", name: "Spotify" },
    Source { key: "brave", patterns: &["brave"], exe: "brave.exe", name: "Brave" },
    Source { key: "chrome", patterns: &["chrome"], exe: "chrome.exe", name: "Chrome" },
    Source { key: "edge", patterns: &["msedge"], exe: "msedge.exe", name: "Edge" },
    // Firefox registers under a hash of its install path.
    Source { key: "firefox", patterns: &["firefox", "308046b0af4a39cb"], exe: "firefox.exe", name: "Firefox" },
    Source { key: "opera", patterns: &["opera"], exe: "opera.exe", name: "Opera" },
    Source { key: "vivaldi", patterns: &["vivaldi"], exe: "vivaldi.exe", name: "Vivaldi" },
];

pub fn source_by_key(key: &str) -> Option<&'static Source> {
    SOURCES.iter().find(|s| s.key.eq_ignore_ascii_case(key.trim()))
}

fn source_of(app_id: &str, enabled: &[&'static Source]) -> Option<&'static Source> {
    let id = app_id.to_lowercase();
    enabled.iter().copied().find(|s| s.patterns.iter().any(|p| id.contains(p)))
}

#[derive(Clone, Copy, Debug)]
pub enum Cmd {
    PlayPause,
    Next,
    Previous,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Info {
    pub title: String,
    /// Artist, or the channel for videos; the app name when the site gives none.
    pub artist: String,
    pub playing: bool,
    pub cover: Option<Arc<Cover>>,
    pub source: &'static Source,
}

static CURRENT: Mutex<Option<Info>> = Mutex::new(None);
static COMMANDS: OnceLock<Mutex<Sender<Cmd>>> = OnceLock::new();
static ENABLED: Mutex<Vec<&'static Source>> = Mutex::new(Vec::new());
static LAST_COMMAND: Mutex<String> = Mutex::new(String::new());

/// Diagnostics: the last command sent and what the app answered.
pub fn last_command() -> String {
    LAST_COMMAND.lock().map(|s| s.clone()).unwrap_or_default()
}

pub fn current() -> Option<Info> {
    CURRENT.lock().ok()?.clone()
}

pub fn send(cmd: Cmd) {
    if let Some(tx) = COMMANDS.get() {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(cmd);
        }
    }
}

/// Which apps to follow (keys from `SOURCES`). Can be changed while running.
pub fn set_sources(keys: &[String]) {
    if let Ok(mut e) = ENABLED.lock() {
        *e = keys.iter().filter_map(|k| source_by_key(k)).collect();
    }
}

pub fn start(owner: HWND, notify_msg: u32) {
    if COMMANDS.get().is_some() {
        return;
    }
    let (tx, rx) = channel::<Cmd>();
    let _ = COMMANDS.set(Mutex::new(tx));
    let owner = owner.0 as isize;
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let notify = || {
            let _ = PostMessageW(HWND(owner as *mut _), notify_msg, WPARAM(0), LPARAM(0));
        };
        let mut chosen_id = String::new();
        let mut last_track = (String::new(), String::new(), String::new());
        let mut cover: Option<Arc<Cover>> = None;
        loop {
            let cmd = match rx.recv_timeout(POLL) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            // A fresh manager every time: a long-lived one keeps reporting the play
            // state from when its sessions were first read (a play click then turns
            // into a second pause). Asking again is cheap and always current.
            let Ok(manager) = Manager::RequestAsync().and_then(|op| op.get()) else {
                continue;
            };            let enabled = ENABLED.lock().map(|e| e.clone()).unwrap_or_default();
            let picked = pick_session(&manager, &enabled, &chosen_id);
            if let Some((s, id, _)) = &picked {
                chosen_id = id.clone();
                if let Some(c) = cmd {
                    // Explicit play/pause: Spotify does not always honour "toggle".
                    let playing = is_playing(s);
                    let r = match c {
                        Cmd::PlayPause if playing => s.TryPauseAsync().and_then(|o| o.get()),
                        Cmd::PlayPause => s.TryPlayAsync().and_then(|o| o.get()),
                        Cmd::Next => s.TrySkipNextAsync().and_then(|o| o.get()),
                        Cmd::Previous => s.TrySkipPreviousAsync().and_then(|o| o.get()),
                    };
                    if let Ok(mut l) = LAST_COMMAND.lock() {
                        *l = format!("{c:?} on {id} (was playing: {playing}) -> {r:?}");
                    }
                    std::thread::sleep(Duration::from_millis(150)); // let the app update
                }
            }
            let info = picked.and_then(|(s, id, source)| {
                let playing = is_playing(&s);
                let props = s.TryGetMediaPropertiesAsync().ok()?.get().ok()?;
                let title = props.Title().map(|t| t.to_string()).unwrap_or_default();
                let mut artist = props.Artist().map(|t| t.to_string()).unwrap_or_default();
                if (id.clone(), title.clone(), artist.clone()) != last_track || cover.is_none() {
                    // New song/video or new source: fetch its cover once.
                    cover = props.Thumbnail().ok().and_then(|t| decode(&t)).map(Arc::new);
                    last_track = (id, title.clone(), artist.clone());
                }
                if artist.is_empty() {
                    artist = source.name.to_string();
                }
                Some(Info { title, artist, playing, cover: cover.clone(), source })
            });
            let changed = match CURRENT.lock() {
                Ok(mut cur) if *cur != info => {
                    *cur = info;
                    true
                }
                _ => false,
            };
            if changed {
                notify();
            }
        }
    });
}

fn is_playing(s: &Session) -> bool {
    s.GetPlaybackInfo()
        .and_then(|i| i.PlaybackStatus())
        .map(|st| st == Status::Playing)
        .unwrap_or(false)
}

/// Picks the session to show: Windows' current session if it is ours and playing,
/// else any of ours that is playing, else the one shown before, else the first.
fn pick_session(manager: &Manager, enabled: &[&'static Source], previous: &str) -> Option<(Session, String, &'static Source)> {
    let sessions = manager.GetSessions().ok()?;
    let ours: Vec<(Session, String, &'static Source)> = (0..sessions.Size().ok()?)
        .filter_map(|i| sessions.GetAt(i).ok())
        .filter_map(|s| {
            let id = s.SourceAppUserModelId().ok()?.to_string();
            let src = source_of(&id, enabled)?;
            Some((s, id, src))
        })
        .collect();
    if ours.is_empty() {
        return None;
    }
    let current = manager.GetCurrentSession().ok().and_then(|s| s.SourceAppUserModelId().ok()).map(|h| h.to_string());
    let idx = ours
        .iter()
        .position(|(s, id, _)| Some(id) == current.as_ref() && is_playing(s))
        .or_else(|| ours.iter().position(|(s, _, _)| is_playing(s)))
        .or_else(|| ours.iter().position(|(_, id, _)| id == previous))
        .unwrap_or(0);
    ours.into_iter().nth(idx)
}

/// Decodes the thumbnail, centre-cropped to a square (video thumbnails are 16:9).
fn decode(reference: &IRandomAccessStreamReference) -> Option<Cover> {
    let stream = reference.OpenReadAsync().ok()?.get().ok()?;
    let stream: IRandomAccessStream = stream.cast().ok()?;
    let decoder = BitmapDecoder::CreateAsync(&stream).ok()?.get().ok()?;
    let (w, h) = (decoder.PixelWidth().ok()?.max(1), decoder.PixelHeight().ok()?.max(1));
    // Scale so the short side is COVER_PX, then crop the middle.
    let (sw, sh) = if w >= h {
        ((w as u64 * COVER_PX as u64 / h as u64) as u32, COVER_PX)
    } else {
        (COVER_PX, (h as u64 * COVER_PX as u64 / w as u64) as u32)
    };
    let t = BitmapTransform::new().ok()?;
    t.SetScaledWidth(sw.max(COVER_PX)).ok()?;
    t.SetScaledHeight(sh.max(COVER_PX)).ok()?;
    t.SetInterpolationMode(BitmapInterpolationMode::Fant).ok()?;
    t.SetBounds(BitmapBounds {
        X: (sw.max(COVER_PX) - COVER_PX) / 2,
        Y: (sh.max(COVER_PX) - COVER_PX) / 2,
        Width: COVER_PX,
        Height: COVER_PX,
    })
    .ok()?;
    let data = decoder
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Premultiplied,
            &t,
            ExifOrientationMode::IgnoreExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )
        .ok()?
        .get()
        .ok()?;
    let bytes = data.DetachPixelData().ok()?;
    let px: Vec<u32> = bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    (px.len() == (COVER_PX * COVER_PX) as usize).then_some(Cover { size: COVER_PX as i32, px })
}

/// Brings the source app to the front (cover click). Runs on its own thread:
/// launching/activating talks to Explorer and must not block the UI thread.
pub fn show_app(source: &'static Source, title_hint: String) {
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if source.key == "spotify" {
            ShellExecuteW(None, windows::core::w!("open"), windows::core::w!("spotify:"), None, None, SW_SHOWNORMAL);
            return;
        }
        if let Some(hwnd) = find_window(source.exe, &title_hint) {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
    });
}

/// A visible top-level window of `exe`; prefers the one whose title contains
/// `hint` (a browser window shows the playing tab's title only if it is the active tab).
fn find_window(exe: &str, hint: &str) -> Option<HWND> {
    struct Search {
        pids: Vec<u32>,
        hint: String,
        best: Option<(bool, HWND)>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let s = &mut *(lparam.0 as *mut Search);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if !s.pids.contains(&pid) || !IsWindowVisible(hwnd).as_bool() || GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return true.into();
        }
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n == 0 {
            return true.into();
        }
        let title = String::from_utf16_lossy(&buf[..n as usize]).to_lowercase();
        let matches = !s.hint.is_empty() && title.contains(&s.hint);
        if s.best.map_or(true, |(m, _)| matches && !m) {
            s.best = Some((matches, hwnd));
        }
        true.into()
    }
    let mut search = Search { pids: crate::spectrum::pids_of(exe), hint: hint.to_lowercase(), best: None };
    if search.pids.is_empty() {
        return None;
    }
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut search as *mut Search as isize));
    }
    search.best.map(|(_, h)| h)
}
