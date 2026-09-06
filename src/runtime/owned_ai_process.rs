//! Bounded ownership of N2 execution children. N1 runners remain separate.
//! Unix groups do not contain children that deliberately create a new session;
//! Linux's parent-death signal protects the direct child, not an arbitrary tree.

use std::fmt;
use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const CAPTURE_LIMIT: usize = 64 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const REAP_TIMEOUT: Duration = Duration::from_secs(5);
const PIPE_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub struct OwnedCommandError {
    pub message: String,
    pub exit_confirmed: bool,
}

impl fmt::Display for OwnedCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OwnedCommandError {}

#[derive(Debug)]
pub struct OwnedCommandOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub forced_stop: bool,
}

#[derive(Debug)]
pub struct OwnedAiChild {
    child: Child,
    status: Option<ExitStatus>,
    exit_confirmed: bool,
    cleanup_failed: bool,
}

impl OwnedAiChild {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            #[cfg(target_os = "linux")]
            let parent_pid = unsafe { libc::getpid() };
            // Only async-signal-safe operations are allowed between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    if libc::setpgid(0, 0) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    #[cfg(target_os = "linux")]
                    {
                        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                            return Err(io::Error::last_os_error());
                        }
                        // The owner may die before the death signal is installed.
                        if libc::getppid() != parent_pid {
                            libc::_exit(127);
                        }
                    }
                    Ok(())
                });
            }
        }
        Ok(Self {
            child: command.spawn()?,
            status: None,
            exit_confirmed: false,
            cleanup_failed: false,
        })
    }

    /// Access pipes only; wait/kill must go through this owner.
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.exit_confirmed {
            return Ok(self.status);
        }
        #[cfg(unix)]
        if self.status.is_none() {
            // Keep the leader unreaped while signalling its group, avoiding PID reuse.
            let mut information = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.id() as libc::id_t,
                    &mut information,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == -1 {
                return Err(io::Error::last_os_error());
            }
            if unsafe { information.si_pid() } == 0 {
                return Ok(None);
            }
            self.signal_group(libc::SIGKILL)?;
            self.status = self.child.try_wait()?;
        }
        #[cfg(not(unix))]
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        if self.status.is_some() && self.group_gone()? {
            self.exit_confirmed = true;
            return Ok(self.status);
        }
        Ok(None)
    }

    pub fn stop(&mut self, grace: Duration) -> Result<(ExitStatus, bool), OwnedCommandError> {
        match self.try_wait() {
            Ok(Some(status)) => return Ok((status, false)),
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::ECHILD) => {
                self.cleanup_failed = true;
                return Err(OwnedCommandError {
                    message: "AI child ownership was lost before exit confirmation".to_string(),
                    exit_confirmed: false,
                });
            }
            _ => {}
        }
        if self.cleanup_failed {
            return Err(OwnedCommandError {
                message: "AI execution cleanup previously ended without exit confirmation"
                    .to_string(),
                exit_confirmed: false,
            });
        }
        self.child.stdin.take();
        #[cfg(unix)]
        if !grace.is_zero() && self.status.is_none() {
            let _ = self.signal_group(libc::SIGTERM);
            if let Some(status) = self.wait_until(Instant::now() + grace.min(REAP_TIMEOUT)) {
                return Ok((status, false));
            }
        }
        #[cfg(not(unix))]
        let _ = grace;
        if self.status.is_none() {
            #[cfg(unix)]
            let _ = self.signal_group(libc::SIGKILL);
            #[cfg(not(unix))]
            let _ = self.child.kill();
        }
        if let Some(status) = self.wait_until(Instant::now() + REAP_TIMEOUT) {
            return Ok((status, true));
        }
        self.cleanup_failed = true;
        Err(OwnedCommandError {
            message: "AI execution exit could not be confirmed after bounded cleanup".to_string(),
            exit_confirmed: false,
        })
    }

    fn wait_until(&mut self, deadline: Instant) -> Option<ExitStatus> {
        loop {
            if let Ok(Some(status)) = self.try_wait() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    #[cfg(unix)]
    fn signal_group(&self, signal: libc::c_int) -> io::Result<()> {
        let result = unsafe { libc::kill(-(self.id() as libc::pid_t), signal) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    fn group_gone(&self) -> io::Result<bool> {
        #[cfg(unix)]
        {
            let result = unsafe { libc::kill(-(self.id() as libc::pid_t), 0) };
            if result == 0 {
                return Ok(false);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(true)
            } else {
                Err(error)
            }
        }
        #[cfg(not(unix))]
        {
            // Windows proves direct-child exit only; it does not provide a Job Object.
            Ok(true)
        }
    }
}

impl Drop for OwnedAiChild {
    fn drop(&mut self) {
        if !self.exit_confirmed && !self.cleanup_failed {
            let _ = self.stop(Duration::ZERO);
        }
    }
}

pub fn run_owned_ai_command(
    command: Command,
    timeout: Duration,
    grace: Duration,
    control: &crate::runtime::ai_execution::ExecutionControl,
) -> Result<OwnedCommandOutput, OwnedCommandError> {
    if control.should_stop() {
        return Err(OwnedCommandError {
            message: if control.is_cancelled() {
                "AI execution cancelled before spawn"
            } else {
                "AI execution deadline expired before spawn"
            }
            .to_string(),
            exit_confirmed: true,
        });
    }
    run_inner(command, timeout, grace, || {
        (control.should_stop(), control.is_cancelled())
    })
}

fn run_inner(
    mut command: Command,
    timeout: Duration,
    grace: Duration,
    stop_requested: impl Fn() -> (bool, bool),
) -> Result<OwnedCommandOutput, OwnedCommandError> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let deadline = Instant::now() + timeout;
    let mut child = OwnedAiChild::spawn(&mut command).map_err(|error| OwnedCommandError {
        message: format!("AI execution spawn failed: {error}"),
        exit_confirmed: true,
    })?;
    let mut pipes_closed = true;
    let result = (|| -> io::Result<_> {
        let mut stdout = PipeCapture::new(child.child_mut().stdout.take().unwrap())?;
        let mut stderr = PipeCapture::new(child.child_mut().stderr.take().unwrap())?;
        let (status, timed_out, cancelled, forced_stop) = loop {
            stdout.pump()?;
            stderr.pump()?;
            if let Some(status) = child.try_wait()? {
                break (status, false, false, false);
            }
            let (stop, cancelled) = stop_requested();
            if stop || Instant::now() >= deadline {
                let (status, forced) = child.stop(grace).map_err(io::Error::other)?;
                break (status, !cancelled, cancelled, forced);
            }
            thread::sleep(POLL_INTERVAL);
        };
        // Nonblocking pipes avoid reader threads that can outlive an escaped descendant.
        let pipe_deadline = Instant::now() + PIPE_TIMEOUT;
        while !stdout.eof || !stderr.eof {
            stdout.pump()?;
            stderr.pump()?;
            if (!stdout.eof || !stderr.eof) && Instant::now() >= pipe_deadline {
                pipes_closed = false;
                return Err(io::Error::other(
                    "AI execution pipes did not close after child exit",
                ));
            }
            if !stdout.eof || !stderr.eof {
                thread::sleep(POLL_INTERVAL);
            }
        }
        Ok(OwnedCommandOutput {
            status,
            stdout: stdout.tail,
            stderr: stderr.tail,
            timed_out,
            cancelled,
            forced_stop,
        })
    })();
    result.map_err(|error| {
        let exit_confirmed = child.stop(Duration::ZERO).is_ok() && pipes_closed;
        OwnedCommandError {
            message: error.to_string(),
            exit_confirmed,
        }
    })
}

#[cfg(unix)]
trait PipeSource: Read + std::os::fd::AsRawFd {}
#[cfg(unix)]
impl<T: Read + std::os::fd::AsRawFd> PipeSource for T {}
#[cfg(windows)]
trait PipeSource: Read + std::os::windows::io::AsRawHandle {}
#[cfg(windows)]
impl<T: Read + std::os::windows::io::AsRawHandle> PipeSource for T {}

struct PipeCapture<P> {
    pipe: P,
    tail: Vec<u8>,
    eof: bool,
}

impl<P: PipeSource> PipeCapture<P> {
    fn new(pipe: P) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
            if flags == -1
                || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                    == -1
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self {
            pipe,
            tail: Vec::with_capacity(CAPTURE_LIMIT),
            eof: false,
        })
    }

    fn pump(&mut self) -> io::Result<()> {
        if self.eof {
            return Ok(());
        }
        let mut buffer = [0u8; 8192];
        for _ in 0..16 {
            let mut limit = buffer.len();
            #[cfg(windows)]
            {
                #[link(name = "kernel32")]
                extern "system" {
                    fn PeekNamedPipe(
                        pipe: *mut std::ffi::c_void,
                        buffer: *mut std::ffi::c_void,
                        size: u32,
                        read: *mut u32,
                        available: *mut u32,
                        left: *mut u32,
                    ) -> i32;
                }
                let mut available = 0;
                let result = unsafe {
                    PeekNamedPipe(
                        self.pipe.as_raw_handle(),
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    )
                };
                if result == 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() == Some(109) {
                        self.eof = true;
                        return Ok(());
                    }
                    return Err(error);
                }
                if available == 0 {
                    return Ok(());
                }
                limit = limit.min(available as usize);
            }
            match self.pipe.read(&mut buffer[..limit]) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(());
                }
                Ok(count) => {
                    let overflow = (self.tail.len() + count).saturating_sub(CAPTURE_LIMIT);
                    self.tail.drain(..overflow);
                    self.tail.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ai_execution::ExecutionRegistry;
    use std::io::Write;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn fixture_command(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["fixture_owned_ai_child", "--nocapture", "--test-threads=1"])
            .env("HARBOR_OWNED_AI_CHILD_FIXTURE", mode)
            .stdin(Stdio::null());
        command
    }

    #[test]
    fn fixture_owned_ai_child() {
        match std::env::var("HARBOR_OWNED_AI_CHILD_FIXTURE").as_deref() {
            Ok("output") => {
                std::io::stdout()
                    .write_all(&vec![b'o'; 256 * 1024])
                    .unwrap();
                std::io::stdout().write_all(b"stdout-tail\n").unwrap();
                std::io::stderr()
                    .write_all(&vec![b'e'; 256 * 1024])
                    .unwrap();
                std::io::stderr().write_all(b"stderr-tail\n").unwrap();
            }
            Ok("sleep") => std::thread::sleep(Duration::from_secs(60)),
            #[cfg(windows)]
            Ok("pipe-holder") => {
                let mut command = fixture_command("sleep");
                command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
                let child = command.spawn().unwrap();
                std::fs::write(
                    std::env::var("HARBOR_OWNED_AI_PID_FILE").unwrap(),
                    child.id().to_string(),
                )
                .unwrap();
                std::process::exit(0);
            }
            #[cfg(target_os = "linux")]
            Ok("parent-exits") => {
                let mut command = fixture_command("sleep");
                let child = OwnedAiChild::spawn(&mut command).unwrap();
                std::fs::write(
                    std::env::var("HARBOR_OWNED_AI_PID_FILE").unwrap(),
                    child.id().to_string(),
                )
                .unwrap();
                // process::exit skips Drop, exercising the kernel parent-death guard.
                std::process::exit(17);
            }
            _ => {}
        }
    }

    #[test]
    fn captures_bounded_tails_without_pipe_deadlock() {
        let output = run_inner(
            fixture_command("output"),
            Duration::from_secs(10),
            Duration::ZERO,
            || (false, false),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), CAPTURE_LIMIT);
        assert_eq!(output.stderr.len(), CAPTURE_LIMIT);
        assert!(String::from_utf8_lossy(&output.stdout).contains("stdout-tail"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("stderr-tail"));
        assert!(!output.timed_out);
        assert!(!output.cancelled);
    }

    #[test]
    fn timeout_kills_and_reaps_a_real_child() {
        let started = Instant::now();
        let output = run_inner(
            fixture_command("sleep"),
            Duration::from_millis(100),
            Duration::ZERO,
            || (false, false),
        )
        .unwrap();
        assert!(output.timed_out);
        assert!(output.forced_stop);
        assert!(!output.cancelled);
        assert!(!output.status.success());
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[test]
    fn cancellation_stops_a_real_child_before_its_deadline() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancelled);
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            trigger.store(true, Ordering::SeqCst);
        });
        let output = run_inner(
            fixture_command("sleep"),
            Duration::from_secs(10),
            Duration::ZERO,
            || {
                let stop = cancelled.load(Ordering::SeqCst);
                (stop, stop)
            },
        )
        .unwrap();
        thread.join().unwrap();
        assert!(output.cancelled);
        assert!(!output.timed_out);
        assert!(!output.status.success());
    }

    #[test]
    fn stop_is_repeatable_after_confirmed_reap() {
        let mut command = fixture_command("sleep");
        let mut child = OwnedAiChild::spawn(&mut command).unwrap();
        let (status, forced) = child.stop(Duration::ZERO).unwrap();
        assert!(forced);
        assert!(!status.success());
        assert_eq!(child.try_wait().unwrap(), Some(status));
        assert_eq!(child.stop(Duration::ZERO).unwrap().0, status);
    }

    #[test]
    fn spawn_failure_is_confirmed_without_an_execution() {
        let output = run_inner(
            Command::new("harbor-owned-ai-no-such-executable"),
            Duration::from_secs(1),
            Duration::ZERO,
            || (false, false),
        );
        assert!(output.unwrap_err().exit_confirmed);
    }

    #[test]
    fn public_control_cancels_actual_execution() {
        let registry = ExecutionRegistry::new(1);
        let ticket = registry
            .register(None, Instant::now() + Duration::from_secs(10))
            .unwrap();
        let control = ticket.control();
        let execution_id = ticket.id().to_string();
        let canceller = registry.clone();
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            canceller.cancel(&execution_id).unwrap();
        });
        let result = run_owned_ai_command(
            fixture_command("sleep"),
            Duration::from_secs(10),
            Duration::ZERO,
            &control,
        )
        .unwrap();
        trigger.join().unwrap();
        assert!(result.cancelled);
        assert!(!result.timed_out);
        assert!(!result.status.success());
        assert_eq!(
            registry.status(ticket.id()).unwrap()["execution_stopped"],
            false
        );
        ticket.finish(true);
    }

    #[test]
    fn expired_public_control_prevents_spawn() {
        let registry = ExecutionRegistry::new(1);
        let ticket = registry.register(None, Instant::now()).unwrap();
        let result = run_owned_ai_command(
            Command::new("harbor-owned-ai-no-such-executable"),
            Duration::from_secs(1),
            Duration::ZERO,
            &ticket.control(),
        )
        .unwrap_err();
        assert!(result.exit_confirmed);
        assert!(result.message.contains("deadline expired before spawn"));
    }

    fn assert_process_stopped(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            #[cfg(unix)]
            let stopped = {
                let gone = unsafe { libc::kill(pid as libc::pid_t, 0) } == -1
                    && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
                #[cfg(target_os = "linux")]
                let gone = gone
                    || std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                        stat.rsplit_once(')')
                            .is_some_and(|(_, fields)| fields.trim_start().starts_with('Z'))
                    });
                gone
            };
            #[cfg(windows)]
            let stopped = unsafe {
                #[link(name = "kernel32")]
                extern "system" {
                    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
                    fn WaitForSingleObject(handle: *mut std::ffi::c_void, timeout: u32) -> u32;
                    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
                }
                let handle = OpenProcess(0x00100000, 0, pid);
                if handle.is_null() {
                    io::Error::last_os_error().raw_os_error() == Some(87)
                } else {
                    let exited = WaitForSingleObject(handle, 0) == 0;
                    CloseHandle(handle);
                    exited
                }
            };
            if stopped {
                return;
            }
            assert!(Instant::now() < deadline, "child {pid} is still executing");
            thread::sleep(POLL_INTERVAL);
        }
    }

    #[test]
    fn drop_kills_and_reaps_an_owned_child() {
        let mut command = fixture_command("sleep");
        let child = OwnedAiChild::spawn(&mut command).unwrap();
        let pid = child.id();
        let started = Instant::now();
        drop(child);
        assert!(started.elapsed() < Duration::from_secs(6));
        assert_process_stopped(pid);
        #[cfg(unix)]
        {
            let result =
                unsafe { libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG) };
            assert_eq!(result, -1);
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn inherited_pipe_timeout_is_bounded_and_never_confirms_tree_exit() {
        struct FixtureCleanup(u32);
        impl Drop for FixtureCleanup {
            fn drop(&mut self) {
                unsafe {
                    #[link(name = "kernel32")]
                    extern "system" {
                        fn OpenProcess(
                            access: u32,
                            inherit: i32,
                            pid: u32,
                        ) -> *mut std::ffi::c_void;
                        fn TerminateProcess(handle: *mut std::ffi::c_void, code: u32) -> i32;
                        fn WaitForSingleObject(handle: *mut std::ffi::c_void, timeout: u32) -> u32;
                        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
                    }
                    let handle = OpenProcess(0x00100001, 0, self.0);
                    if !handle.is_null() {
                        TerminateProcess(handle, 1);
                        WaitForSingleObject(handle, 5000);
                        CloseHandle(handle);
                    }
                }
            }
        }
        let path =
            std::env::temp_dir().join(format!("harbor-owned-ai-pid-{}", uuid::Uuid::new_v4()));
        let mut command = fixture_command("pipe-holder");
        command.env("HARBOR_OWNED_AI_PID_FILE", &path);
        let started = Instant::now();
        let result = run_inner(command, Duration::from_secs(10), Duration::ZERO, || {
            (false, false)
        });
        let pid = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let cleanup = FixtureCleanup(pid);
        std::fs::remove_file(path).unwrap();
        let error = result.unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(!error.exit_confirmed);
        assert!(error.message.contains("pipes did not close"));
        drop(cleanup);
        assert_process_stopped(pid);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_parent_exit_stops_direct_child_without_drop() {
        let path =
            std::env::temp_dir().join(format!("harbor-owned-ai-pid-{}", uuid::Uuid::new_v4()));
        let mut command = fixture_command("parent-exits");
        command.env("HARBOR_OWNED_AI_PID_FILE", &path);
        let result = run_inner(command, Duration::from_secs(10), Duration::ZERO, || {
            (false, false)
        })
        .unwrap();
        assert_eq!(result.status.code(), Some(17));
        let pid = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        std::fs::remove_file(path).unwrap();
        assert_process_stopped(pid);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn timeout_stops_an_inherited_process_group_without_false_confirmation() {
        let path =
            std::env::temp_dir().join(format!("harbor-owned-ai-pid-{}", uuid::Uuid::new_v4()));
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "trap '' TERM; sleep 60 & echo $! > \"$HARBOR_OWNED_AI_PID_FILE\"; wait",
            ])
            .env("HARBOR_OWNED_AI_PID_FILE", &path);
        let result = run_inner(
            command,
            Duration::from_millis(300),
            Duration::from_millis(100),
            || (false, false),
        );
        let pid = std::fs::read_to_string(&path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::fs::remove_file(path).unwrap();
        // A container PID 1 may delay reaping an orphan. That must remain unconfirmed.
        match result {
            Ok(output) => {
                assert!(output.timed_out);
                assert!(output.forced_stop);
                assert!(!output.status.success());
            }
            Err(error) => assert!(!error.exit_confirmed),
        }
        assert_process_stopped(pid);
    }

    #[cfg(unix)]
    #[test]
    fn sigterm_grace_allows_cooperative_exit() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "trap 'exit 0' TERM; while :; do :; done"]);
        let result = run_inner(
            command,
            Duration::from_millis(200),
            Duration::from_secs(1),
            || (false, false),
        )
        .unwrap();
        assert!(result.timed_out);
        assert!(!result.forced_stop);
        assert!(result.status.success());
    }

    #[cfg(unix)]
    #[test]
    fn ignored_sigterm_requires_forced_group_stop() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "trap '' TERM; while :; do :; done"]);
        let output = run_inner(
            command,
            Duration::from_millis(200),
            Duration::from_millis(100),
            || (false, false),
        )
        .unwrap();
        assert!(output.timed_out);
        assert!(output.forced_stop);
        assert!(!output.status.success());
    }
}
