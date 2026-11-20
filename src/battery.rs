//! Mouse battery level.
//!
//! 2.4 GHz receiver (Attack Shark X11, 1D57:FA60): the mouse sends a status input
//! report on its own every ~2 s on interface 2, collection with usage page 0x000A:
//!     03 55 40 01 <battery %>
//! We only listen; nothing is ever written to the mouse.
//!
//! Bluetooth: Windows publishes the level in the device property
//! {104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2 (same value as Settings > Bluetooth).
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::{w, GUID, PCWSTR};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDevicePropertyW, DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SP_DEVINFO_DATA,
};
use windows::Win32::Devices::Properties::{
    DEVPKEY_Device_FriendlyName, DEVPROPKEY, DEVPROPTYPE, DEVPROP_TYPE_BYTE,
};
use windows::Win32::Foundation::HWND;

const BATTERY_KEY: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0x104EA319_6EE2_4701_BD47_8DDBF425BBE5),
    pid: 2,
};

/// Last receiver reading: (percent, when, raw report).
static RECEIVER: Mutex<Option<(u8, Instant, Vec<u8>)>> = Mutex::new(None);
static RECEIVER_STARTED: AtomicBool = AtomicBool::new(false);
/// A reading older than this is dropped (mouse off or receiver unplugged).
const STALE: Duration = Duration::from_secs(120);

/// Listens to the 2.4 GHz receiver on a background thread. Posts `notify_msg`
/// to `owner` whenever the percentage changes.
pub fn start_receiver(owner: HWND, notify_msg: u32, vid: u16, pid: u16) {
    if RECEIVER_STARTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let owner = owner.0 as isize;
    std::thread::spawn(move || loop {
        if let Ok(api) = hidapi::HidApi::new() {
            let info = api
                .device_list()
                .find(|d| d.vendor_id() == vid && d.product_id() == pid && d.usage_page() == 0x000A);
            if let Some(dev) = info.and_then(|d| d.open_device(&api).ok()) {
                let mut buf = [0u8; 64];
                loop {
                    match dev.read_timeout(&mut buf, 5000) {
                        Ok(n) if n >= 5 && buf[0] == 0x03 && buf[1] == 0x55 => {
                            let pct = buf[4].min(100);
                            let changed = match RECEIVER.lock() {
                                Ok(mut r) => {
                                    let old = r.as_ref().map(|x| x.0);
                                    *r = Some((pct, Instant::now(), buf[..n].to_vec()));
                                    old != Some(pct)
                                }
                                Err(_) => false,
                            };
                            if changed {
                                unsafe {
                                    let _ = PostMessageW(HWND(owner as *mut _), notify_msg, WPARAM(0), LPARAM(0));
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(_) => break, // unplugged: look for it again
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    });
}

/// Raw last receiver report, for the menu ("03 55 40 01 57").
pub fn receiver_raw() -> Option<String> {
    let r = RECEIVER.lock().ok()?;
    let (_, _, raw) = r.as_ref()?;
    Some(raw.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" "))
}

fn receiver_level() -> Option<u8> {
    let r = RECEIVER.lock().ok()?;
    let (pct, at, _) = r.as_ref()?;
    (at.elapsed() < STALE).then_some(*pct)
}

/// Returns (device name, percent): the 2.4 GHz receiver first, then Bluetooth.
pub fn read() -> Option<(String, u8)> {
    if let Some(p) = receiver_level() {
        return Some(("X11 Mouse".into(), p));
    }
    let mut best: Option<(u8, String, u8)> = None;
    for en in [w!("BTHLE"), w!("BTHENUM")] {
        scan(en, &mut best);
    }
    best.map(|(_, name, pct)| (name, pct.min(100)))
}

fn scan(enumerator: PCWSTR, best: &mut Option<(u8, String, u8)>) {
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(None, enumerator, HWND::default(), DIGCF_PRESENT | DIGCF_ALLCLASSES)
        else {
            return;
        };
        let mut i = 0;
        loop {
            let mut data = SP_DEVINFO_DATA {
                cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInfo(set, i, &mut data).is_err() {
                break;
            }
            i += 1;
            let mut ty = DEVPROPTYPE::default();
            let mut b = [0u8; 4];
            let ok = SetupDiGetDevicePropertyW(set, &data, &BATTERY_KEY, &mut ty, Some(&mut b), None, 0).is_ok();
            if !ok || ty != DEVPROP_TYPE_BYTE {
                continue;
            }
            let name = friendly_name(set, &data);
            let lower = name.to_lowercase();
            // Only mice: never show a headset's battery by accident.
            let score = if lower.contains("x11") || lower.contains("shark") {
                2
            } else if lower.contains("mouse") {
                1
            } else {
                0
            };
            if score > 0 && best.as_ref().map_or(true, |(s, _, _)| score > *s) {
                *best = Some((score, name, b[0]));
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
}

unsafe fn friendly_name(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> String {
    let mut ty = DEVPROPTYPE::default();
    let mut buf = [0u8; 512];
    if SetupDiGetDevicePropertyW(set, data, &DEVPKEY_Device_FriendlyName, &mut ty, Some(&mut buf), None, 0).is_err() {
        return String::new();
    }
    let wide: Vec<u16> = buf.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
    String::from_utf16_lossy(&wide)
}
