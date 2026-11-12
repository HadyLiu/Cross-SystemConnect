//! The LocalSystem service of the elevated mode (see [`crate::elevated`]).
//!
//! Like Deskflow's watchdog: watch the active console session and keep one
//! core running there under a copy of winlogon.exe's token (SYSTEM, in that
//! session, on `winsta0\default`), restarting it when it exits or the
//! session changes.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, SetTokenInformation, TokenPrimary, TokenSessionId,
    TOKEN_ACCESS_MASK, TOKEN_DUPLICATE, TOKEN_QUERY,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSGetActiveConsoleSessionId, WTSQueryUserToken,
};
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW, StartServiceW,
    SC_HANDLE, SC_MANAGER_ALL_ACCESS, SC_MANAGER_CONNECT, SERVICE_ACCEPT_SESSIONCHANGE,
    SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_ALL_ACCESS, SERVICE_AUTO_START,
    SERVICE_CONFIG_DESCRIPTION, SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SESSIONCHANGE,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE,
    SERVICE_STATUS_HANDLE, SERVICE_STOP, SERVICE_STOPPED, SERVICE_STOP_PENDING,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetExitCodeProcess, OpenProcess, OpenProcessToken, TerminateProcess,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION,
    PROCESS_QUERY_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Downloads, FOLDERID_LocalAppData, FOLDERID_Profile,
    FOLDERID_RoamingAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
};

use crate::elevated::{
    child_command, decide, merge_env, service_command, Action, Backoff, SERVICE_DESCRIPTION,
    SERVICE_DISPLAY, SERVICE_NAME,
};
use csc_config::user_dirs;

const DELETE: u32 = 0x0001_0000;
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;
const NO_SESSION: u32 = 0xFFFF_FFFF;
const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
const ERROR_SERVICE_DOES_NOT_EXIST: i32 = 1060;
const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
const ERROR_SERVICE_NOT_ACTIVE: i32 = 1062;

fn io_err(e: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0 & 0xFFFF)
}

fn win32_code(e: &windows::core::Error) -> i32 {
    e.code().0 & 0xFFFF
}

struct Sc(SC_HANDLE);

impl Drop for Sc {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseServiceHandle(self.0);
        }
    }
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

// ---- install / uninstall / state --------------------------------------

/// Installed and running state of the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    NotInstalled,
    Stopped,
    Running,
}

pub fn state() -> ServiceState {
    unsafe {
        let Ok(scm) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) else {
            return ServiceState::NotInstalled;
        };
        let scm = Sc(scm);
        let Ok(svc) = OpenServiceW(scm.0, &HSTRING::from(SERVICE_NAME), SERVICE_QUERY_STATUS)
        else {
            return ServiceState::NotInstalled;
        };
        let svc = Sc(svc);
        let mut st = SERVICE_STATUS::default();
        if QueryServiceStatus(svc.0, &mut st).is_ok() && st.dwCurrentState == SERVICE_RUNNING {
            ServiceState::Running
        } else {
            ServiceState::Stopped
        }
    }
}

/// Install (or update) and start the service. Needs administrator rights.
pub fn install(exe: &Path, config: &Path) -> io::Result<()> {
    let name = HSTRING::from(SERVICE_NAME);
    let display = HSTRING::from(SERVICE_DISPLAY);
    let cmd = HSTRING::from(service_command(exe, config));
    unsafe {
        let scm = Sc(
            OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS)
                .map_err(io_err)?,
        );
        let svc = match OpenServiceW(scm.0, &name, SERVICE_ALL_ACCESS) {
            Ok(svc) => {
                let svc = Sc(svc);
                ChangeServiceConfigW(
                    svc.0,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    &cmd,
                    PCWSTR::null(),
                    None,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    &display,
                )
                .map_err(io_err)?;
                svc
            }
            Err(_) => Sc(CreateServiceW(
                scm.0,
                &name,
                &display,
                SERVICE_ALL_ACCESS,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                &cmd,
                PCWSTR::null(),
                None,
                PCWSTR::null(),
                PCWSTR::null(), // LocalSystem
                PCWSTR::null(),
            )
            .map_err(io_err)?),
        };
        let mut text: Vec<u16> = SERVICE_DESCRIPTION.encode_utf16().chain([0]).collect();
        let desc = SERVICE_DESCRIPTIONW {
            lpDescription: PWSTR(text.as_mut_ptr()),
        };
        let _ = ChangeServiceConfig2W(
            svc.0,
            SERVICE_CONFIG_DESCRIPTION,
            Some((&desc as *const SERVICE_DESCRIPTIONW).cast()),
        );
        match StartServiceW(svc.0, None) {
            Ok(()) => Ok(()),
            Err(e) if win32_code(&e) == ERROR_SERVICE_ALREADY_RUNNING => Ok(()),
            Err(e) => Err(io_err(e)),
        }
    }
}

/// Stop and remove the service. Not installed counts as success.
pub fn uninstall() -> io::Result<()> {
    unsafe {
        let scm = Sc(
            OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT).map_err(io_err)?,
        );
        let svc = match OpenServiceW(
            scm.0,
            &HSTRING::from(SERVICE_NAME),
            SERVICE_STOP | SERVICE_QUERY_STATUS | DELETE,
        ) {
            Ok(svc) => Sc(svc),
            Err(e) if win32_code(&e) == ERROR_SERVICE_DOES_NOT_EXIST => return Ok(()),
            Err(e) => return Err(io_err(e)),
        };
        let mut st = SERVICE_STATUS::default();
        match ControlService(svc.0, SERVICE_CONTROL_STOP, &mut st) {
            Ok(()) => {}
            Err(e) if win32_code(&e) == ERROR_SERVICE_NOT_ACTIVE => {}
            Err(e) => tracing::warn!("stop service: {e}"),
        }
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            if QueryServiceStatus(svc.0, &mut st).is_err() || st.dwCurrentState == SERVICE_STOPPED {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        DeleteService(svc.0).map_err(io_err)
    }
}

// ---- service process -------------------------------------------------

static STOP: AtomicBool = AtomicBool::new(false);
static SESSION_EVENT: AtomicBool = AtomicBool::new(false);
static STATUS: AtomicUsize = AtomicUsize::new(0);
static CONFIG: OnceLock<PathBuf> = OnceLock::new();

/// Entry for `--service`: hand the thread to the service manager. Returns
/// once the service has stopped.
pub fn run(config: PathBuf) -> io::Result<()> {
    let _ = CONFIG.set(config);
    let mut name: Vec<u16> = SERVICE_NAME.encode_utf16().chain([0]).collect();
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()).map_err(io_err) }
}

fn report(state: SERVICE_STATUS_CURRENT_STATE) {
    let handle = STATUS.load(Ordering::SeqCst);
    if handle == 0 {
        return;
    }
    let st = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE
        } else {
            0
        },
        dwWaitHint: 5000,
        ..Default::default()
    };
    unsafe {
        let _ = SetServiceStatus(SERVICE_STATUS_HANDLE(handle as *mut _), &st);
    }
}

unsafe extern "system" fn control_handler(
    control: u32,
    _event: u32,
    _data: *mut core::ffi::c_void,
    _ctx: *mut core::ffi::c_void,
) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            STOP.store(true, Ordering::SeqCst);
            report(SERVICE_STOP_PENDING);
            0
        }
        SERVICE_CONTROL_SESSIONCHANGE => {
            SESSION_EVENT.store(true, Ordering::SeqCst);
            0
        }
        SERVICE_CONTROL_INTERROGATE => 0,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let Ok(handle) =
        RegisterServiceCtrlHandlerExW(&HSTRING::from(SERVICE_NAME), Some(control_handler), None)
    else {
        tracing::error!("RegisterServiceCtrlHandlerExW failed");
        return;
    };
    STATUS.store(handle.0 as usize, Ordering::SeqCst);
    report(SERVICE_RUNNING);
    tracing::info!("elevated service running");
    supervise();
    tracing::info!("elevated service stopped");
    report(SERVICE_STOPPED);
}

struct Child {
    process: Handle,
    session: u32,
    started: Instant,
    /// Started while someone was signed in (with that user's folders).
    with_user: bool,
}

impl Child {
    fn exited(&self) -> bool {
        unsafe { WaitForSingleObject(self.process.0, 0).0 == 0 }
    }

    fn exit_code(&self) -> u32 {
        let mut code = 0u32;
        unsafe {
            let _ = GetExitCodeProcess(self.process.0, &mut code);
        }
        code
    }

    fn kill(self) {
        if !self.exited() {
            unsafe {
                let _ = TerminateProcess(self.process.0, 0);
                WaitForSingleObject(self.process.0, 3000);
            }
        }
    }
}

fn user_signed_in(session: u32) -> bool {
    let mut token = HANDLE::default();
    unsafe { WTSQueryUserToken(session, &mut token) }
        .ok()
        .map(|_| Handle(token))
        .is_some()
}

fn active_session() -> Option<u32> {
    let s = unsafe { WTSGetActiveConsoleSessionId() };
    (s != NO_SESSION).then_some(s)
}

fn supervise() {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            tracing::error!("current_exe: {e}");
            return;
        }
    };
    let config = CONFIG.get().cloned().unwrap_or_default();
    let mut child: Option<Child> = None;
    let mut backoff = Backoff::default();
    let mut next_start = Instant::now();
    while !STOP.load(Ordering::SeqCst) {
        if SESSION_EVENT.swap(false, Ordering::SeqCst) {
            tracing::info!(
                "session change, active console session {:?}",
                active_session()
            );
        }
        let now = Instant::now();
        let active = active_session();
        // Started on the sign-in screen, and now a user signed in: restart so
        // the core gets that user's folders and config.
        if let Some(c) = child.take_if(|c| !c.with_user && !c.exited() && user_signed_in(c.session))
        {
            tracing::info!(
                session = c.session,
                "user signed in; restarting the core with the user's folders"
            );
            c.kill();
            next_start = now;
        }
        match decide(child.as_ref().map(|c| (c.session, c.exited())), active) {
            Action::Nothing => {}
            Action::Stop => {
                if let Some(c) = child.take() {
                    tracing::info!("no console session; stopping the core");
                    c.kill();
                }
            }
            Action::Restart(session) => {
                if let Some(c) = child.take() {
                    tracing::info!(
                        from = c.session,
                        to = session,
                        "console session changed; restarting the core"
                    );
                    c.kill();
                }
                backoff = Backoff::default();
                next_start = now;
            }
            Action::Start(session) => {
                if let Some(c) = child.take() {
                    let ran = c.started.elapsed();
                    let wait = backoff.after_exit(ran);
                    tracing::warn!(
                        code = c.exit_code(),
                        ran_secs = ran.as_secs(),
                        "core exited; starting again in {}s",
                        wait.as_secs()
                    );
                    next_start = now + wait;
                } else if now >= next_start {
                    match launch(&exe, &config, session) {
                        Ok(c) => {
                            tracing::info!(session, "core started in console session");
                            child = Some(c);
                        }
                        Err(e) => {
                            let wait = backoff.after_exit(Duration::ZERO);
                            tracing::warn!(
                                session,
                                "start core: {e}; retry in {}s",
                                wait.as_secs()
                            );
                            next_start = now + wait;
                        }
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if let Some(c) = child.take() {
        c.kill();
    }
}

fn find_winlogon(session: u32) -> io::Result<u32> {
    unsafe {
        let snap = Handle(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).map_err(io_err)?);
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snap.0, &mut entry).is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if name.eq_ignore_ascii_case("winlogon.exe") {
                let mut s = 0u32;
                if ProcessIdToSessionId(entry.th32ProcessID, &mut s).is_ok() && s == session {
                    return Ok(entry.th32ProcessID);
                }
            }
            more = Process32NextW(snap.0, &mut entry).is_ok();
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("winlogon.exe not found in session {session}"),
    ))
}

fn known_folder(id: &windows::core::GUID, token: HANDLE) -> Option<String> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, token).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s.filter(|s| !s.is_empty())
    }
}

/// Folder variables for the core (see `csc_config::user_dirs`), and the
/// config path to use: the signed-in user's own when it exists, otherwise
/// the one given at install time.
fn user_env(session: u32, config: &Path) -> (Vec<(&'static str, String)>, PathBuf, Option<Handle>) {
    let mut vars = Vec::new();
    let mut config = config.to_path_buf();
    let mut token = HANDLE::default();
    let user = unsafe { WTSQueryUserToken(session, &mut token) }
        .ok()
        .map(|_| Handle(token));
    if let Some(user) = &user {
        if let Some(roaming) = known_folder(&FOLDERID_RoamingAppData, user.0) {
            let own = Path::new(&roaming)
                .join(csc_config::APP_DIR_NAME)
                .join("config.json");
            if own.exists() {
                config = own;
            }
            vars.push((user_dirs::CONFIG_VAR, roaming));
        }
        for (var, id) in [
            (user_dirs::CACHE_VAR, &FOLDERID_LocalAppData),
            (user_dirs::DESKTOP_VAR, &FOLDERID_Desktop),
            (user_dirs::DOWNLOAD_VAR, &FOLDERID_Downloads),
            (user_dirs::HOME_VAR, &FOLDERID_Profile),
        ] {
            if let Some(dir) = known_folder(id, user.0) {
                vars.push((var, dir));
            }
        }
    }
    if !vars.iter().any(|(k, _)| *k == user_dirs::CONFIG_VAR) {
        // Sign-in screen: nobody is logged on. Keep using the folders of the
        // config the service was installed with (<Roaming>\<app>\config.json).
        if let Some(roaming) = config.parent().and_then(Path::parent) {
            vars.push((
                user_dirs::CONFIG_VAR,
                roaming.to_string_lossy().into_owned(),
            ));
        }
    }
    (vars, config, user)
}

fn environment(token: HANDLE, extra: &[(&str, String)]) -> Vec<u16> {
    let mut base = Vec::new();
    unsafe {
        let mut block: *mut core::ffi::c_void = std::ptr::null_mut();
        if CreateEnvironmentBlock(&mut block, token, false).is_ok() && !block.is_null() {
            let mut len = 0usize;
            let p = block as *const u16;
            // Ends at the first empty string (two NULs in a row).
            while !(*p.add(len) == 0 && (len == 0 || *p.add(len - 1) == 0)) {
                len += 1;
            }
            base = crate::elevated::decode_env_block(std::slice::from_raw_parts(p, len + 1));
            let _ = DestroyEnvironmentBlock(block);
        }
    }
    crate::elevated::encode_env_block(&merge_env(base, extra))
}

fn launch(exe: &Path, config: &Path, session: u32) -> io::Result<Child> {
    let pid = find_winlogon(session)?;
    unsafe {
        let process = Handle(OpenProcess(PROCESS_QUERY_INFORMATION, false, pid).map_err(io_err)?);
        let mut token = HANDLE::default();
        OpenProcessToken(process.0, TOKEN_DUPLICATE | TOKEN_QUERY, &mut token).map_err(io_err)?;
        let token = Handle(token);
        let mut primary = HANDLE::default();
        DuplicateTokenEx(
            token.0,
            TOKEN_ACCESS_MASK(MAXIMUM_ALLOWED),
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
        .map_err(io_err)?;
        let primary = Handle(primary);
        SetTokenInformation(
            primary.0,
            TokenSessionId,
            (&session as *const u32).cast(),
            size_of::<u32>() as u32,
        )
        .map_err(io_err)?;

        let (vars, config, user) = user_env(session, config);
        let with_user = user.is_some();
        let mut extra = vars;
        extra.push(("CSC_SERVICE_SESSION", session.to_string()));
        let env_token = user.as_ref().map(|u| u.0).unwrap_or(primary.0);
        let env = environment(env_token, &extra);

        let mut cmd: Vec<u16> = child_command(exe, &config)
            .encode_utf16()
            .chain([0])
            .collect();
        let mut desktop: Vec<u16> = "winsta0\\default".encode_utf16().chain([0]).collect();
        let si = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        let dir = exe.parent().map(|d| HSTRING::from(d.as_os_str()));
        CreateProcessAsUserW(
            primary.0,
            PCWSTR::null(),
            PWSTR(cmd.as_mut_ptr()),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            Some(env.as_ptr().cast()),
            dir.as_ref()
                .map(|d| PCWSTR(d.as_ptr()))
                .unwrap_or(PCWSTR::null()),
            &si,
            &mut pi,
        )
        .map_err(io_err)?;
        let _ = CloseHandle(pi.hThread);
        Ok(Child {
            process: Handle(pi.hProcess),
            session,
            started: Instant::now(),
            with_user,
        })
    }
}
