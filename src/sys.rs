//! The few OS calls std does not offer: liveness / stopping of a process by pid (QEMU
//! started by an earlier `wtl` call), starting QEMU in its own session, and Ctrl-C.

use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
/// QEMU processes started by this process: their exit is seen with try_wait (a dead child
/// stays a zombie, which `kill -0` would still call alive).
static CHILDREN: Mutex<Vec<Child>> = Mutex::new(Vec::new());

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Ctrl-C / SIGTERM only set a flag; the waits poll it and stop cleanly.
pub fn install_interrupt_handler() {
    let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst));
}

/// Sleep in small steps; returns false early if Ctrl-C / SIGTERM arrived.
pub fn nap(secs: f64) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(secs.max(0.0));
    loop {
        if interrupted() {
            return false;
        }
        let now = Instant::now();
        if now >= end {
            return true;
        }
        std::thread::sleep((end - now).min(Duration::from_millis(100)));
    }
}

/// Spawn a background process detached from our terminal's Ctrl-C (own session on Unix,
/// own process group on Windows) and remember it for [`pid_alive`].
pub fn spawn_detached(cmd: &mut Command) -> std::io::Result<u32> {
    imp::detach(cmd);
    let child = cmd.spawn()?;
    let pid = child.id();
    CHILDREN.lock().unwrap_or_else(|e| e.into_inner()).push(child);
    Ok(pid)
}

pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    {
        let mut kids = CHILDREN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = kids.iter_mut().find(|c| c.id() == pid) {
            return matches!(c.try_wait(), Ok(None));
        }
    }
    imp::alive(pid)
}

/// Polite stop (SIGTERM; TerminateProcess on Windows).
pub fn terminate(pid: u32) -> bool {
    imp::kill(pid, false)
}

/// Hard stop (SIGKILL; TerminateProcess on Windows).
pub fn kill_hard(pid: u32) -> bool {
    imp::kill(pid, true)
}

#[cfg(unix)]
mod imp {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    pub fn detach(cmd: &mut Command) {
        // like `setsid qemu ...`: a terminal Ctrl-C (or hangup) reaches only us; we stop
        // QEMU over QMP
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    pub fn alive(pid: u32) -> bool {
        let Ok(p) = libc::pid_t::try_from(pid) else {
            return false;
        };
        if unsafe { libc::kill(p, 0) } != 0 {
            // EPERM: it exists, it is just not ours
            return std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        }
        // a zombie still "exists"; Linux tells us its state
        if let Ok(st) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) {
            if let Some((_, rest)) = st.rsplit_once(')') {
                if let Some(state) = rest.split_whitespace().next() {
                    return state != "Z" && state != "X";
                }
            }
        }
        true
    }

    pub fn kill(pid: u32, hard: bool) -> bool {
        let Ok(p) = libc::pid_t::try_from(pid) else {
            return false;
        };
        unsafe { libc::kill(p, if hard { libc::SIGKILL } else { libc::SIGTERM }) == 0 }
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, TerminateProcess, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_TERMINATE,
    };

    pub fn detach(cmd: &mut Command) {
        // a console Ctrl+C is not delivered to a new process group
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    pub fn alive(pid: u32) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE as u32
        }
    }

    pub fn kill(pid: u32, _hard: bool) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if h.is_null() {
                return false;
            }
            let ok = TerminateProcess(h, 1) != 0;
            CloseHandle(h);
            ok
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::process::Command;
    pub fn detach(_: &mut Command) {}
    pub fn alive(_: u32) -> bool {
        false
    }
    pub fn kill(_: u32, _: bool) -> bool {
        false
    }
}
