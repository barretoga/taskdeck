//! Spectrum bars for the active player's audio only (Spotify, or the browser
//! playing a video). Uses WASAPI *process loopback* (Windows 10 2004+): captures
//! what that app's process tree plays, before Voicemeeter or any device mixing.
//! The capture only runs while `set_active(true)` (something playing), so it
//! costs nothing when paused.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{implement, Interface, IUnknown, HRESULT, PROPVARIANT};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

pub const BANDS: usize = 10;
const FFT_N: usize = 2048;
const RATE: u32 = 48_000;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static LEVELS: Mutex<[f32; BANDS]> = Mutex::new([0.0; BANDS]);
/// Executable whose audio is captured, e.g. "spotify.exe" or "brave.exe".
static TARGET: Mutex<Option<String>> = Mutex::new(None);
static STATUS: Mutex<String> = Mutex::new(String::new());
static PACKETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Diagnostics: last capture result and number of audio packets received.
pub fn status() -> (String, u64) {
    (STATUS.lock().map(|s| s.clone()).unwrap_or_default(), PACKETS.load(Ordering::Relaxed))
}

pub fn set_active(on: bool) {
    ACTIVE.store(on, Ordering::Relaxed);
}

/// Which app to listen to. A running capture switches over within ~25 ms.
pub fn set_target(exe: Option<&str>) {
    if let Ok(mut t) = TARGET.lock() {
        *t = exe.map(|e| e.to_lowercase());
    }
}

fn target() -> Option<String> {
    TARGET.lock().ok()?.clone()
}

pub fn levels() -> [f32; BANDS] {
    LEVELS.lock().map(|l| *l).unwrap_or([0.0; BANDS])
}

pub fn start() {
    if STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        loop {
            let exe = target();
            let Some(exe) = exe.filter(|_| ACTIVE.load(Ordering::Relaxed)) else {
                clear();
                std::thread::sleep(Duration::from_millis(300));
                continue;
            };
            match root_pid(&exe) {
                Some(pid) => {
                    // Returns when paused, the target changes, the process exits, or capture fails.
                    let r = capture(pid, &exe);
                    if let Ok(mut s) = STATUS.lock() {
                        *s = format!("{exe} pid {pid}: {r:?}");
                    }
                    if let Err(e) = &r {
                        crate::diag::log(&format!("audio capture of {exe} (pid {pid}) failed: {e}"));
                    }
                    if r.is_err() {
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
                None => std::thread::sleep(Duration::from_secs(2)),
            }
            clear();
        }
    });
}

fn clear() {
    if let Ok(mut l) = LEVELS.lock() {
        *l = [0.0; BANDS];
    }
}

/// (pid, parent pid) of every running process named `exe`.
fn processes(exe: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                if String::from_utf16_lossy(&e.szExeFile[..len]).eq_ignore_ascii_case(exe) {
                    out.push((e.th32ProcessID, e.th32ParentProcessID));
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// All process ids of `exe` (used to find its windows).
pub fn pids_of(exe: &str) -> Vec<u32> {
    processes(exe).into_iter().map(|p| p.0).collect()
}

/// The top process of the app (whose parent is not the same app); loopback
/// includes its children, e.g. a browser's audio service process.
fn root_pid(exe: &str) -> Option<u32> {
    let ours = processes(exe);
    ours.iter().find(|p| !ours.iter().any(|q| q.0 == p.1)).map(|p| p.0)
}
#[implement(IActivateAudioInterfaceCompletionHandler, windows::Win32::System::Com::IAgileObject)]
struct Activated(SyncSender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Activated_Impl {
    fn ActivateCompleted(&self, _op: Option<&IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        let _ = self.0.try_send(());
        Ok(())
    }
}

impl windows::Win32::System::Com::IAgileObject_Impl for Activated_Impl {}

/// PROPVARIANT holding a VT_BLOB (layout of the x64 PROPVARIANT).
#[repr(C)]
struct BlobVariant {
    vt: u16,
    r1: u16,
    r2: u16,
    r3: u16,
    size: u32,
    data: *const u8,
}

unsafe fn capture(pid: u32, exe: &str) -> windows::core::Result<()> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    let blob = BlobVariant {
        vt: 65, // VT_BLOB
        r1: 0,
        r2: 0,
        r3: 0,
        size: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        data: &params as *const _ as *const u8,
    };
    let (tx, rx) = sync_channel(1);
    let handler: IActivateAudioInterfaceCompletionHandler = Activated(tx).into();
    let op = ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&blob as *const BlobVariant as *const PROPVARIANT),
        &handler,
    )?;
    rx.recv_timeout(Duration::from_secs(5)).map_err(|_| windows::core::Error::empty())?;
    let mut hr = HRESULT(0);
    let mut unk: Option<IUnknown> = None;
    op.GetActivateResult(&mut hr, &mut unk)?;
    hr.ok()?;
    let client: IAudioClient = unk.ok_or_else(windows::core::Error::empty)?.cast()?;

    let format = WAVEFORMATEX {
        wFormatTag: 3, // IEEE float
        nChannels: 2,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * 8,
        nBlockAlign: 8,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        2_000_000, // 200 ms buffer
        0,
        &format,
        None,
    )?;
    let cap: IAudioCaptureClient = client.GetService()?;
    client.Start()?;

    let mut ring = vec![0f32; FFT_N];
    let mut pos = 0usize;
    let mut last_data = Instant::now();
    let mut last_check = Instant::now();
    let mut envelope = 0.3f32;
    let window: Vec<f32> = (0..FFT_N)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_N - 1) as f32).cos())
        .collect();

    while ACTIVE.load(Ordering::Relaxed) && target().as_deref() == Some(exe) {
        std::thread::sleep(Duration::from_millis(25));
        let mut got = false;
        while cap.GetNextPacketSize()? > 0 {
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
            let samples = std::slice::from_raw_parts(data as *const f32, frames as usize * 2);
            for f in 0..frames as usize {
                ring[pos] = if silent { 0.0 } else { (samples[f * 2] + samples[f * 2 + 1]) * 0.5 };
                pos = (pos + 1) % FFT_N;
            }
            cap.ReleaseBuffer(frames)?;
            PACKETS.fetch_add(1, Ordering::Relaxed);
            got = true;
        }
        if got {
            last_data = Instant::now();
            let raw = analyze(&ring, pos, &window);
            // Auto gain: follow the recent loudest band, show the top part of the
            // range, so bars use their full height at any volume.
            let loudest = raw.iter().cloned().fold(0f32, f32::max);
            envelope = (envelope * 0.996).max(loudest).max(0.3);
            const RANGE: f32 = 0.32;
            let bands = raw.map(|r| ((r - (envelope - RANGE)) / RANGE).clamp(0.0, 1.0));
            if let Ok(mut l) = LEVELS.lock() {
                *l = bands;
            }
        } else if last_data.elapsed() > Duration::from_millis(150) {
            clear();
        }
        if last_check.elapsed() > Duration::from_secs(3) {
            last_check = Instant::now();
            if root_pid(exe) != Some(pid) {
                break; // Spotify restarted
            }
        }
    }
    let _ = client.Stop();
    Ok(())
}

/// FFT of the latest FFT_N samples -> BANDS log-spaced levels in 0..1.
fn analyze(ring: &[f32], pos: usize, window: &[f32]) -> [f32; BANDS] {
    let mut re: Vec<f32> = (0..FFT_N).map(|i| ring[(pos + i) % FFT_N] * window[i]).collect();
    let mut im = vec![0f32; FFT_N];
    fft(&mut re, &mut im);
    let bin_hz = RATE as f32 / FFT_N as f32;
    let (lo, hi) = (45f32, 12_000f32);
    let mut out = [0f32; BANDS];
    for (b, slot) in out.iter_mut().enumerate() {
        let f0 = lo * (hi / lo).powf(b as f32 / BANDS as f32);
        let f1 = lo * (hi / lo).powf((b + 1) as f32 / BANDS as f32);
        let i0 = ((f0 / bin_hz) as usize).max(1);
        let i1 = ((f1 / bin_hz) as usize).max(i0 + 1).min(FFT_N / 2);
        let peak = (i0..i1).map(|i| (re[i] * re[i] + im[i] * im[i]).sqrt()).fold(0f32, f32::max);
        // Normalise: full-scale sine with Hann window peaks near FFT_N / 4.
        let db = 20.0 * (peak / (FFT_N as f32 / 4.0)).max(1e-6).log10();
        // Treble gets a tilt boost so high bars are not always flat.
        let tilt = 1.5 * b as f32;
        *slot = ((db + tilt + 70.0) / 70.0).clamp(0.0, 1.0);
    }
    out
}

/// In-place iterative radix-2 FFT.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1f32, 0f32);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
        }
        len <<= 1;
    }
}
