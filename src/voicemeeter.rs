//! Minimal binding to VoicemeeterRemote64.dll (the official Voicemeeter remote API),
//! loaded at runtime so TaskDeck still works when Voicemeeter is not installed.
use std::ffi::CString;
use windows::core::{HSTRING, PCSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

type FnVoid = unsafe extern "system" fn() -> i32;
type FnGetF = unsafe extern "system" fn(*const i8, *mut f32) -> i32;
type FnSetF = unsafe extern "system" fn(*const i8, f32) -> i32;
type FnGetSW = unsafe extern "system" fn(*const i8, *mut u16) -> i32;
type FnType = unsafe extern "system" fn(*mut i32) -> i32;

pub struct Voicemeeter {
    _lib: HMODULE,
    logout: FnVoid,
    dirty: FnVoid,
    get_f: FnGetF,
    set_f: FnSetF,
    get_sw: FnGetSW,
    vm_type: FnType,
}

impl Voicemeeter {
    pub fn connect(dll: &str) -> Option<Self> {
        unsafe {
            let lib = LoadLibraryW(&HSTRING::from(dll)).ok()?;
            macro_rules! sym {
                ($name:literal, $t:ty) => {
                    std::mem::transmute::<_, $t>(GetProcAddress(lib, PCSTR(concat!($name, "\0").as_ptr()))?)
                };
            }
            let login: FnVoid = sym!("VBVMR_Login", FnVoid);
            let vm = Self {
                _lib: lib,
                logout: sym!("VBVMR_Logout", FnVoid),
                dirty: sym!("VBVMR_IsParametersDirty", FnVoid),
                get_f: sym!("VBVMR_GetParameterFloat", FnGetF),
                set_f: sym!("VBVMR_SetParameterFloat", FnSetF),
                get_sw: sym!("VBVMR_GetParameterStringW", FnGetSW),
                vm_type: sym!("VBVMR_GetVoicemeeterType", FnType),
            };
            // 0 = logged in, 1 = logged in but Voicemeeter not running yet.
            if login() < 0 {
                return None;
            }
            Some(vm)
        }
    }

    /// 1 = Voicemeeter, 2 = Banana, 3 = Potato; None when it is not running.
    pub fn kind(&self) -> Option<i32> {
        let mut t = 0;
        (unsafe { (self.vm_type)(&mut t) } == 0).then_some(t)
    }

    /// Pulls fresh parameter values; must be called before reading.
    pub fn refresh(&self) {
        unsafe {
            (self.dirty)();
        }
    }

    pub fn get(&self, param: &str) -> Option<f32> {
        let name = CString::new(param).ok()?;
        let mut v = 0f32;
        (unsafe { (self.get_f)(name.as_ptr(), &mut v) } == 0).then_some(v)
    }

    pub fn set(&self, param: &str, value: f32) -> bool {
        let Ok(name) = CString::new(param) else { return false };
        unsafe { (self.set_f)(name.as_ptr(), value) == 0 }
    }

    pub fn get_string(&self, param: &str) -> Option<String> {
        let name = CString::new(param).ok()?;
        let mut buf = [0u16; 512];
        if unsafe { (self.get_sw)(name.as_ptr(), buf.as_mut_ptr()) } != 0 {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    /// Hardware input strips per edition (Voicemeeter 2, Banana 3, Potato 5).
    pub fn hardware_strips(&self) -> usize {
        match self.kind() {
            Some(1) => 2,
            Some(3) => 5,
            _ => 3,
        }
    }
}

impl Drop for Voicemeeter {
    fn drop(&mut self) {
        unsafe {
            (self.logout)();
        }
    }
}
