//! The widget window: a layered child window living inside a taskbar
//! (Shell_TrayWnd on the main screen, Shell_SecondaryTrayWnd on the others),
//! kept just left of the system tray area.
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use windows::core::{w, BSTR, VARIANT};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, TreeScope_Descendants, UIA_AutomationIdPropertyId,
    UIA_ClassNamePropertyId,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::render::{self, Item, Layout, View};

/// Posted to the owner window. wParam = item id.
pub const WM_WIDGET_CLICK: u32 = WM_APP + 10;
pub const WM_WIDGET_MENU: u32 = WM_APP + 11;
pub const WM_WIDGET_HOVER: u32 = WM_APP + 12;

const WM_MOUSELEAVE: u32 = 0x02A3;
/// Look the tray anchor up again every N repositions (tray items come and go).
const REFIND_EVERY: u32 = 20;

static OWNER: AtomicIsize = AtomicIsize::new(0);
static CLASS_REGISTERED: AtomicBool = AtomicBool::new(false);
/// Per taskbar, filled by the anchor thread: the tray's screen rect and the right
/// edge of the last app icon (Start button when no apps are pinned/open).
static ANCHORS: Mutex<Vec<Anchor>> = Mutex::new(Vec::new());
static ANCHOR_THREAD: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct Anchor {
    bar: isize,
    tray: RECT,
    apps_right: Option<i32>,
}

/// UI Automation queries must not run on the thread that owns the widget windows:
/// UIA asks every window in the taskbar (ours included) for its info, and would
/// wait on our own blocked thread. So the taskbar is measured on a background thread.
pub fn start_anchor_thread() {
    if ANCHOR_THREAD.swap(true, Ordering::Relaxed) {
        return;
    }
    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(uia) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) else {
            return;
        };
        let prop = |id, value: &str| uia.CreatePropertyCondition(id, &VARIANT::from(BSTR::from(value)));
        let Ok(tray_cond) = prop(UIA_AutomationIdPropertyId, "SystemTrayIcon") else { return };
        // App buttons, plus Start so an empty task list still has a left boundary.
        let (Ok(apps), Ok(start)) = (
            prop(UIA_ClassNamePropertyId, "Taskbar.TaskListButtonAutomationPeer"),
            prop(UIA_AutomationIdPropertyId, "StartButton"),
        ) else {
            return;
        };
        let Ok(apps_cond) = uia.CreateOrCondition(&apps, &start) else { return };

        let mut cache: Vec<(isize, IUIAutomationElement)> = Vec::new();
        let mut last_apps: Vec<(isize, Option<i32>)> = Vec::new();
        let mut tick: u32 = 0;
        loop {
            let bars = find_taskbars();
            cache.retain(|(k, _)| bars.iter().any(|b| b.0 as isize == *k));
            let mut found = Vec::new();
            for bar in bars {
                let key = bar.0 as isize;
                let cached = if tick % REFIND_EVERY != 0 {
                    cache
                        .iter()
                        .find(|(k, _)| *k == key)
                        .and_then(|(_, el)| el.CurrentBoundingRectangle().ok())
                        .filter(|r| r.right > r.left)
                } else {
                    None
                };
                let root = uia.ElementFromHandle(bar).ok();
                let tray = cached.or_else(|| {
                    let all = root.as_ref()?.FindAll(TreeScope_Descendants, &tray_cond).ok()?;
                    let mut best: Option<(RECT, IUIAutomationElement)> = None;
                    for i in 0..all.Length().unwrap_or(0) {
                        let Ok(el) = all.GetElement(i) else { continue };
                        let Ok(r) = el.CurrentBoundingRectangle() else { continue };
                        if r.right > r.left && best.as_ref().map_or(true, |(b, _)| r.left < b.left) {
                            best = Some((r, el));
                        }
                    }
                    let (r, el) = best?;
                    cache.retain(|(k, _)| *k != key);
                    cache.push((key, el));
                    Some(r)
                });
                // App icons move whenever a program opens/closes: re-measure every
                // other tick (~0.8 s). Icons right of the tray (none on Win11) are ignored.
                let apps_right = if tick % 2 == 0 {
                    let right = root.as_ref().and_then(|root| {
                        let all = root.FindAll(TreeScope_Descendants, &apps_cond).ok()?;
                        let limit = tray.map(|t| t.left).unwrap_or(i32::MAX);
                        (0..all.Length().unwrap_or(0))
                            .filter_map(|i| all.GetElement(i).ok()?.CurrentBoundingRectangle().ok())
                            .filter(|r| r.right > r.left && r.right <= limit)
                            .map(|r| r.right)
                            .max()
                    });
                    last_apps.retain(|(k, _)| *k != key);
                    last_apps.push((key, right));
                    right
                } else {
                    last_apps.iter().find(|(k, _)| *k == key).and_then(|(_, r)| *r)
                };
                if let Some(tray) = tray {
                    found.push(Anchor { bar: key, tray, apps_right });
                }
            }
            if let Ok(mut a) = ANCHORS.lock() {
                *a = found;
            }
            tick = tick.wrapping_add(1);
            std::thread::sleep(Duration::from_millis(400));
        }
    });
}

fn anchor_for(taskbar: HWND) -> Option<Anchor> {
    let key = taskbar.0 as isize;
    ANCHORS.lock().ok()?.iter().find(|a| a.bar == key).copied()
}
thread_local! {
    /// Hit-test boxes per widget window.
    static HIT: RefCell<Vec<(isize, Vec<(Item, i32, i32)>)>> = const { RefCell::new(Vec::new()) };
    /// Which item of which widget the mouse is over.
    static HOVER: Cell<Option<(isize, Item)>> = const { Cell::new(None) };
}

pub fn hover_for(hwnd: HWND) -> Option<Item> {
    HOVER.with(|h| h.get()).filter(|(w, _)| *w == hwnd.0 as isize).map(|(_, it)| it)
}

/// All taskbars: the main one first, then one per extra screen.
pub fn find_taskbars() -> Vec<HWND> {
    let mut bars = Vec::new();
    unsafe {
        if let Ok(main) = FindWindowW(w!("Shell_TrayWnd"), None) {
            bars.push(main);
        }
        let mut prev = HWND::default();
        while let Ok(h) = FindWindowExW(None, prev, w!("Shell_SecondaryTrayWnd"), None) {
            if h.is_invalid() {
                break;
            }
            bars.push(h);
            prev = h;
        }
    }
    bars
}

pub struct Widget {
    pub hwnd: HWND,
    taskbar: HWND,
    scale: f32,
    h: i32,
    layout: Layout,
    pos: (i32, i32, i32, i32),
    /// Free width between the last app icon and the tray, in pixels (None = unknown).
    room: Option<i32>,
    /// Static image (everything except the spectrum bars).
    base: Vec<u32>,
    bar_color: render::Rgb,
    canvas: Option<Canvas>,
}

/// Reusable 32-bit DIB for UpdateLayeredWindow (avoids per-frame GDI allocations).
struct Canvas {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
}

impl Canvas {
    unsafe fn new(w: i32, h: i32) -> Option<Canvas> {
        let screen = GetDC(None);
        let dc = CreateCompatibleDC(screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, None, 0);
        ReleaseDC(None, screen);
        let Ok(bmp) = bmp else {
            let _ = DeleteDC(dc);
            return None;
        };
        let old = SelectObject(dc, bmp);
        Some(Canvas { dc, bmp, old, bits: bits as *mut u32, w, h })
    }

    fn pixels(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize) }
    }

    unsafe fn present(&self, hwnd: HWND) {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let size = SIZE { cx: self.w, cy: self.h };
        let src = POINT { x: 0, y: 0 };
        let _ = UpdateLayeredWindow(hwnd, HDC::default(), None, Some(&size), self.dc, Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp);
            let _ = DeleteDC(self.dc);
        }
    }
}

impl Widget {
    pub fn create(owner: HWND, taskbar: HWND) -> Option<Widget> {
        unsafe {
            OWNER.store(owner.0 as isize, Ordering::Relaxed);
            let hinstance: HINSTANCE = GetModuleHandleW(None).ok()?.into();
            if !CLASS_REGISTERED.swap(true, Ordering::Relaxed) {
                let class = WNDCLASSW {
                    lpfnWndProc: Some(wndproc),
                    hInstance: hinstance,
                    hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
                    lpszClassName: w!("TaskDeckWidget"),
                    ..Default::default()
                };
                RegisterClassW(&class);
            }
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE,
                w!("TaskDeckWidget"),
                w!("TaskDeck"),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                0, 0, 1, 1,
                taskbar, None, hinstance, None,
            )
            .ok()?;
            start_anchor_thread();
            let mut w = Widget {
                hwnd,
                taskbar,
                scale: 1.0,
                h: 0,
                layout: Layout::default(),
                pos: (i32::MIN, 0, 0, 0),
                room: None,
                base: Vec::new(),
                bar_color: render::SPOTIFY_GREEN,
                canvas: None,
            };
            w.reposition();
            Some(w)
        }
    }

    pub fn taskbar(&self) -> HWND {
        self.taskbar
    }

    pub fn alive(&self) -> bool {
        unsafe { IsWindow(self.hwnd).as_bool() && IsWindow(self.taskbar).as_bool() }
    }

    /// Draws the view. Size changes (battery shown/hidden, DPI) also move the window.
    pub fn render(&mut self, view: &View, bars: &[f32]) {
        // Drop parts (shorter song text, cover only, no bars, ...) until it fits
        // between the app icons and the tray.
        let view = &render::fit(view, self.scale, self.h, self.room);
        let lay = render::layout(view, self.scale, self.h);
        self.base = render::compose(view, &lay, self.scale);
        let key = self.hwnd.0 as isize;
        HIT.with(|h| {
            let mut h = h.borrow_mut();
            h.retain(|(k, _)| *k != key);
            h.push((key, lay.items.clone()));
        });
        let resized = lay.w != self.layout.w || lay.h != self.layout.h;
        self.layout = lay;
        self.render_bars(bars);
        if resized {
            self.pos.0 = i32::MIN;
            self.reposition();
        }
    }

    pub fn set_bar_color(&mut self, c: render::Rgb) {
        self.bar_color = c;
    }

    /// Fast path for animation: static image + bars, no text rendering.
    pub fn render_bars(&mut self, bars: &[f32]) {
        let (w, h) = (self.layout.w, self.layout.h);
        if w <= 0 || h <= 0 || self.base.len() != (w * h) as usize {
            return;
        }
        if self.canvas.as_ref().map_or(true, |c| c.w != w || c.h != h) {
            self.canvas = unsafe { Canvas::new(w, h) };
        }
        let span = self.layout.span(Item::Bars);
        let (scale, color) = (self.scale, self.bar_color);
        let Some(canvas) = self.canvas.as_mut() else { return };
        let px = canvas.pixels();
        px.copy_from_slice(&self.base);
        if let Some(span) = span {
            render::draw_bars(px, w, h, span, bars, scale, color);
        }
        unsafe { canvas.present(self.hwnd) };
    }

    /// Keeps the widget just left of the tray. Returns true when DPI or taskbar
    /// height changed (the caller should re-render).
    pub fn reposition(&mut self) -> bool {
        unsafe {
            let dpi = GetDpiForWindow(self.taskbar);
            let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
            let mut tb = RECT::default();
            let _ = GetWindowRect(self.taskbar, &mut tb);
            let h = tb.bottom - tb.top;
            let changed = scale != self.scale || h != self.h;
            self.scale = scale;
            self.h = h;

            let gap = (4.0 * scale) as i32;
            let anchor = anchor_for(self.taskbar);
            let x = match anchor {
                Some(a) => a.tray.left - tb.left - self.layout.w - gap,
                None => (tb.right - tb.left) - (300.0 * scale) as i32 - self.layout.w,
            }
            .max(0);

            // Space left for the widget: from the last app icon (plus a margin) to the tray.
            let margin = (12.0 * scale) as i32;
            let room = anchor.and_then(|a| Some(a.tray.left - gap - (a.apps_right? + margin)));
            let room_changed = match (room, self.room) {
                (Some(a), Some(b)) => (a - b).abs() >= 3, // ignore 1-2 px jitter
                (a, b) => a.is_some() != b.is_some(),
            };
            if room_changed {
                self.room = room;
            }
            let changed = changed || room_changed;
            let pos = (x, 0, self.layout.w.max(1), h);
            let on_top = GetWindow(self.taskbar, GW_CHILD).ok() == Some(self.hwnd);
            if pos != self.pos || !on_top {
                let _ = SetWindowPos(self.hwnd, HWND_TOP, pos.0, pos.1, pos.2, pos.3, SWP_NOACTIVATE | SWP_SHOWWINDOW);
                self.pos = pos;
            }
            changed
        }
    }
}

impl Drop for Widget {
    fn drop(&mut self) {
        let key = self.hwnd.0 as isize;
        HIT.with(|h| h.borrow_mut().retain(|(k, _)| *k != key));
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn hit(hwnd: HWND, x: i32) -> Option<Item> {
    let key = hwnd.0 as isize;
    HIT.with(|h| {
        h.borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .and_then(|(_, items)| items.iter().find(|(_, x0, x1)| x >= *x0 && x < *x1).map(|(it, _, _)| *it))
    })
}

fn post(msg: u32, wparam: usize) {
    let owner = HWND(OWNER.load(Ordering::Relaxed) as *mut _);
    unsafe {
        let _ = PostMessageW(owner, msg, WPARAM(wparam), LPARAM(0));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let x = (lparam.0 & 0xFFFF) as i16 as i32;
    let key = hwnd.0 as isize;
    match msg {
        WM_MOUSEMOVE => {
            let now = hit(hwnd, x).map(|it| (key, it));
            if now != HOVER.with(|h| h.get()) {
                HOVER.with(|h| h.set(now));
                post(WM_WIDGET_HOVER, 0);
            }
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut tme);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            // Only clear if the hover still belongs to this widget.
            if HOVER.with(|h| h.get()).is_some_and(|(w, _)| w == key) {
                HOVER.with(|h| h.set(None));
                post(WM_WIDGET_HOVER, 0);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(item) = hit(hwnd, x) {
                post(WM_WIDGET_CLICK, item as usize);
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            post(WM_WIDGET_MENU, 0);
            LRESULT(0)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
