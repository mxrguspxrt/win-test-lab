//! smoke.exe - a tiny, self-contained test program for `wtl run`, buildable for x64 and
//! Arm64 Windows from any host with rustup (no Windows SDK, no MinGW, no C runtime).
//! Prints who it runs as, which CPU the exe is built for and which CPU the machine has (an
//! x64 exe on Windows on Arm runs translated), then "RESULT OK" and exits 0.
#![no_std]
#![no_main]

use core::ffi::c_void;

#[link(name = "kernel32", kind = "raw-dylib")]
extern "system" {
    fn GetStdHandle(n: u32) -> *mut c_void;
    fn WriteFile(h: *mut c_void, buf: *const u8, n: u32, written: *mut u32, ov: *mut c_void) -> i32;
    fn ExitProcess(code: u32) -> !;
    fn GetEnvironmentVariableA(name: *const u8, buf: *mut u8, size: u32) -> u32;
    fn GetModuleHandleA(name: *const u8) -> *mut c_void;
    fn GetProcAddress(m: *mut c_void, name: *const u8) -> *mut c_void;
    fn GetCurrentProcess() -> *mut c_void;
}

fn out(s: &[u8]) {
    unsafe {
        let mut w = 0;
        WriteFile(GetStdHandle(-11i32 as u32), s.as_ptr(), s.len() as u32, &mut w, core::ptr::null_mut());
    }
}

/// An environment variable (NAME must end in \0), or "?".
fn env<'a>(name: &[u8], buf: &'a mut [u8; 128]) -> &'a [u8] {
    let n = unsafe { GetEnvironmentVariableA(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if n == 0 || n >= buf.len() { b"?" } else { &buf[..n] }
}

fn machine_name(m: u16) -> &'static [u8] {
    match m {
        0x8664 => b"x64",
        0xAA64 => b"arm64",
        0x014c => b"x86",
        _ => b"other",
    }
}

/// The machine's own CPU: IsWow64Process2's native machine (Windows 10 1511+; looked up at
/// run time so the exe still starts on Server 2012 R2, where it reports "unknown").
fn native_machine() -> &'static [u8] {
    type F = unsafe extern "system" fn(*mut c_void, *mut u16, *mut u16) -> i32;
    unsafe {
        let k = GetModuleHandleA(b"kernel32.dll\0".as_ptr());
        let p = GetProcAddress(k, b"IsWow64Process2\0".as_ptr());
        if p.is_null() {
            return b"unknown (no IsWow64Process2)";
        }
        let f: F = core::mem::transmute(p);
        let (mut proc_m, mut native) = (0u16, 0u16);
        if f(GetCurrentProcess(), &mut proc_m, &mut native) == 0 {
            return b"unknown";
        }
        machine_name(native)
    }
}

#[no_mangle]
pub extern "system" fn start() -> ! {
    let (mut a, mut b) = ([0u8; 128], [0u8; 128]);
    out(b"smoke.exe running as ");
    out(env(b"USERDOMAIN\0", &mut a));
    out(b"\\");
    out(env(b"USERNAME\0", &mut b));
    out(b"\r\nexe built for ");
    out(if cfg!(target_arch = "aarch64") { b"arm64" } else { b"x64" });
    out(b", machine CPU ");
    let native = native_machine();
    out(native);
    let exe: &[u8] = if cfg!(target_arch = "aarch64") { b"arm64" } else { b"x64" };
    if native == b"arm64" && exe == b"x64" {
        out(b" (running translated)");
    }
    out(b"\r\nRESULT OK\r\n");
    unsafe { ExitProcess(0) }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    unsafe { ExitProcess(101) }
}
