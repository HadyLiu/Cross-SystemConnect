//! Virtual mouse and keyboard via `/dev/uinput`.
//!
//! On Wayland, XTest is turned into a Remote Desktop portal session. GNOME then
//! shows “远程桌面 / 允许远程交互” and, after that, drops the warp, so the
//! pointer freezes. A kernel uinput device is an ordinary mouse: the compositor
//! does not ask for remote interaction.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Mutex;
use tracing::{info, warn};

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const SYN_REPORT: u16 = 0;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const REL_WHEEL_HI_RES: u16 = 0x0b;
const REL_HWHEEL_HI_RES: u16 = 0x0c;
const BTN_LEFT: u16 = 0x110;
const INPUT_PROP_POINTER: i32 = 0x00;
const BUS_USB: u16 = 0x03;
const KEY_MAX: u16 = 0x2ff;

const fn iow(nr: u32, size: u32) -> libc::c_ulong {
    ((1u32 << 30) | (size << 16) | ((b'U' as u32) << 8) | nr) as libc::c_ulong
}

const fn io(nr: u32) -> libc::c_ulong {
    (((b'U' as u32) << 8) | nr) as libc::c_ulong
}

const UI_DEV_CREATE: libc::c_ulong = io(1);
const UI_DEV_SETUP: libc::c_ulong = iow(3, 92);
const UI_SET_EVBIT: libc::c_ulong = iow(100, 4);
const UI_SET_KEYBIT: libc::c_ulong = iow(101, 4);
const UI_SET_RELBIT: libc::c_ulong = iow(102, 4);
const UI_SET_PROPBIT: libc::c_ulong = iow(110, 4);

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct Ev {
    sec: i64,
    usec: i64,
    type_: u16,
    code: u16,
    value: i32,
}

const RULE: &str = "\
KERNEL==\"uinput\", MODE=\"0666\", OPTIONS+=\"static_node=uinput\"
SUBSYSTEM==\"input\", KERNEL==\"event*\", ATTRS{name}==\"Cross-SystemConnect\", \\
  ENV{ID_INPUT}=\"1\", ENV{ID_INPUT_MOUSE}=\"1\", ENV{ID_INPUT_KEYBOARD}=\"1\", \\
  ENV{LIBINPUT_ATTR_ACCEL_PROFILE}=\"flat\", ENV{LIBINPUT_ATTR_ACCEL_SPEED}=\"0\"
";

struct Dev {
    file: File,
}

struct Slot {
    dev: Option<Dev>,
    warned: bool,
}

static SLOT: Mutex<Slot> = Mutex::new(Slot {
    dev: None,
    warned: false,
});

static ASKING: AtomicBool = AtomicBool::new(false);
static LEFT_DOWN: AtomicBool = AtomicBool::new(false);

pub(crate) fn left_down() -> bool {
    LEFT_DOWN.load(Ordering::Acquire)
}

fn ensure(slot: &mut Slot) -> Option<&mut Dev> {
    if slot.dev.is_none() {
        match open_dev() {
            Some(dev) => {
                slot.dev = Some(dev);
                slot.warned = false;
            }
            None => {
                if !slot.warned && !ASKING.load(Ordering::Relaxed) {
                    warn!(
                        "cannot open /dev/uinput; Wayland pointer control needs it and does not use remote interaction"
                    );
                    slot.warned = true;
                }
            }
        }
    }
    slot.dev.as_mut()
}

/// Open the device now so the first pointer jump on SwitchEnter is not dropped.
pub fn warm() -> bool {
    let Ok(mut slot) = SLOT.lock() else {
        return false;
    };
    ensure(&mut slot).is_some()
}

/// Make `/dev/uinput` writable before the pointer arrives.
/// A password dialog is shown once, at startup, while this computer still has the mouse.
pub fn grant_access() {
    if device_writable() {
        let _ = warm();
        return;
    }
    install_rule();
    if device_writable() {
        let _ = warm();
        info!("Wayland pointer control can use /dev/uinput");
        return;
    }
    info!("asking once for permission to use /dev/uinput");
    ASKING.store(true, Ordering::Relaxed);
    let ok = prompt_install();
    ASKING.store(false, Ordering::Relaxed);
    if ok && device_writable() {
        let _ = warm();
        info!("Wayland pointer control can use /dev/uinput; remote interaction is not requested");
    } else {
        warn!(
            "cannot open /dev/uinput; install /etc/udev/rules.d/71-cross-systemconnect.rules and reload udev, then start the program again"
        );
    }
}

fn device_writable() -> bool {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/uinput")
        .is_ok()
}

fn prompt_install() -> bool {
    let script = format!(
        "cat > /etc/udev/rules.d/71-cross-systemconnect.rules << 'END'\n{RULE}END\nudevadm control --reload-rules\nudevadm trigger --sysname-match=uinput --action=change\n"
    );
    let status = Command::new("pkexec")
        .arg("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .status();
    match status {
        Ok(status) => status.success(),
        Err(e) => {
            warn!("could not ask for /dev/uinput permission ({e})");
            false
        }
    }
}

fn open_dev() -> Option<Dev> {
    install_rule();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/uinput")
        .or_else(|_| {
            let _ = std::process::Command::new("udevadm").arg("settle").status();
            OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/uinput")
        });
    let file = match file {
        Ok(file) => file,
        Err(_) => return None,
    };
    unsafe {
        let fd = file.as_raw_fd();
        if ioctl_int(fd, UI_SET_EVBIT, EV_KEY as i32) < 0
            || ioctl_int(fd, UI_SET_EVBIT, EV_REL as i32) < 0
            || ioctl_int(fd, UI_SET_EVBIT, EV_SYN as i32) < 0
            || ioctl_int(fd, UI_SET_PROPBIT, INPUT_PROP_POINTER) < 0
        {
            warn!("uinput setup failed");
            return None;
        }
        for bit in [
            REL_X,
            REL_Y,
            REL_WHEEL,
            REL_HWHEEL,
            REL_WHEEL_HI_RES,
            REL_HWHEEL_HI_RES,
        ] {
            if ioctl_int(fd, UI_SET_RELBIT, bit as i32) < 0 {
                warn!("uinput relative axis {bit} failed");
                return None;
            }
        }
        for code in 1..=KEY_MAX {
            if ioctl_int(fd, UI_SET_KEYBIT, code as i32) < 0 {
                warn!("uinput key bit failed");
                return None;
            }
        }
        let mut setup = UinputSetup {
            id: InputId {
                bustype: BUS_USB,
                vendor: 0x1209,
                product: 0x0001,
                version: 1,
            },
            name: [0; 80],
            ff_effects_max: 0,
        };
        let name = b"Cross-SystemConnect";
        setup.name[..name.len()].copy_from_slice(name);
        if libc::ioctl(fd, UI_DEV_SETUP, &setup) < 0 || libc::ioctl(fd, UI_DEV_CREATE) < 0 {
            warn!("uinput device create failed");
            return None;
        }
    }
    let _ = std::process::Command::new("udevadm").arg("settle").status();
    info!("Wayland pointer control uses /dev/uinput; remote interaction is not requested");
    Some(Dev { file })
}

fn install_rule() {
    let path = "/etc/udev/rules.d/71-cross-systemconnect.rules";
    if std::fs::read_to_string(path).ok().as_deref() == Some(RULE) {
        return;
    }
    if std::fs::write(path, RULE).is_err() {
        return;
    }
    info!("installed {path} so /dev/uinput is usable and pointer motion stays 1:1");
    let _ = std::process::Command::new("udevadm")
        .args(["control", "--reload-rules"])
        .status();
    let _ = std::process::Command::new("udevadm")
        .args(["trigger", "--sysname-match=uinput", "--action=change"])
        .status();
}

unsafe fn ioctl_int(fd: i32, req: libc::c_ulong, value: i32) -> i32 {
    libc::ioctl(fd, req, value)
}

fn emit(pairs: &[(u16, u16, i32)]) {
    if pairs.is_empty() {
        return;
    }
    let Ok(mut slot) = SLOT.lock() else {
        return;
    };
    let Some(dev) = ensure(&mut slot) else {
        return;
    };
    let mut buf = Vec::with_capacity((pairs.len() + 1) * std::mem::size_of::<Ev>());
    for (type_, code, value) in pairs {
        buf.extend_from_slice(&event_bytes(*type_, *code, *value));
    }
    buf.extend_from_slice(&event_bytes(EV_SYN, SYN_REPORT, 0));
    if dev.file.write_all(&buf).is_err() {
        warn!("uinput write failed");
    }
}

fn event_bytes(type_: u16, code: u16, value: i32) -> [u8; std::mem::size_of::<Ev>()] {
    let ev = Ev {
        sec: 0,
        usec: 0,
        type_,
        code,
        value,
    };
    unsafe {
        let mut out = [0u8; std::mem::size_of::<Ev>()];
        std::ptr::copy_nonoverlapping(
            std::ptr::from_ref(&ev).cast::<u8>(),
            out.as_mut_ptr(),
            std::mem::size_of::<Ev>(),
        );
        out
    }
}

pub fn move_rel(dx: i32, dy: i32) {
    // One report. Splitting a jump into many small steps paints the pointer
    // sliding across the desktop. The kernel accepts a full relative delta.
    if dx == 0 && dy == 0 {
        return;
    }
    emit(&[(EV_REL, REL_X, dx), (EV_REL, REL_Y, dy)]);
}

pub fn button(button: u32, down: bool) {
    if button == 0 {
        LEFT_DOWN.store(down, Ordering::Release);
    }
    let code = match button {
        0 => BTN_LEFT,
        1 => BTN_LEFT + 1,
        2 => BTN_LEFT + 2,
        3 => BTN_LEFT + 3,
        4 => BTN_LEFT + 4,
        _ => return,
    };
    emit(&[(EV_KEY, code, if down { 1 } else { 0 })]);
}

/// `v` and `h` in 1/120 notch units. Hi-res axes get every unit; the legacy
/// axes get whole notches once enough units add up.
pub fn wheel(v: i32, h: i32) {
    let mut ev = Vec::new();
    if v != 0 {
        ev.push((EV_REL, REL_WHEEL_HI_RES, v));
        let notches = super::wheel_notches(&WHEEL_V, v);
        if notches != 0 {
            ev.push((EV_REL, REL_WHEEL, notches));
        }
    }
    if h != 0 {
        ev.push((EV_REL, REL_HWHEEL_HI_RES, h));
        let notches = super::wheel_notches(&WHEEL_H, h);
        if notches != 0 {
            ev.push((EV_REL, REL_HWHEEL, notches));
        }
    }
    emit(&ev);
}

static WHEEL_V: AtomicI32 = AtomicI32::new(0);
static WHEEL_H: AtomicI32 = AtomicI32::new(0);

pub fn key(code: u16, down: bool) {
    emit(&[(EV_KEY, code, if down { 1 } else { 0 })]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_uinput_h() {
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
        assert_eq!(std::mem::size_of::<Ev>(), 24);
        assert_eq!(UI_DEV_CREATE, 0x5501);
        assert_eq!(UI_DEV_SETUP, 0x405C_5503);
        assert_eq!(UI_SET_EVBIT, 0x4004_5564);
        assert_eq!(UI_SET_KEYBIT, 0x4004_5565);
        assert_eq!(UI_SET_RELBIT, 0x4004_5566);
        assert_eq!(UI_SET_PROPBIT, 0x4004_556E);
    }
}
