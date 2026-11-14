use crate::{
    Result,
    provider::{self, Decoded},
};
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Debug, Clone)]
pub struct Outcome {
    pub status: String,
    pub error: Option<Value>,
    pub code: Option<i32>,
    pub provider: Option<String>,
}
impl Outcome {
    pub fn fail(status: &str, message: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            error: Some(json!(message.into())),
            code: None,
            provider: None,
        }
    }
}
pub struct Spec<'a> {
    pub adapter: &'a str,
    pub command: &'a str,
    pub args: &'a [String],
    pub cwd: &'a Path,
    pub prompt: &'a str,
    pub authentication: &'a str,
    pub timeout_ms: u64,
}
fn nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}
fn signal_group(pid: u32, sig: i32) {
    unsafe {
        libc::kill(-(pid as i32), sig);
    }
}
struct ChildGuard(std::process::Child, bool);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.1 {
            signal_group(self.0.id(), libc::SIGKILL);
            let _ = self.0.wait();
        }
    }
}
fn tail(buffer: &mut String, s: &str) {
    buffer.push_str(s);
    if buffer.len() > 32000 {
        let mut at = buffer.len() - 32000;
        while !buffer.is_char_boundary(at) {
            at += 1;
        }
        buffer.drain(..at);
    }
}
struct StreamState {
    buffer: Vec<u8>,
    stderr: String,
    last_error: Option<Value>,
    forced: bool,
    complete: bool,
    has_text: bool,
    fatal: Option<String>,
}
impl StreamState {
    fn line<F>(&mut self, line: &[u8], adapter: &str, event: &mut F)
    where
        F: FnMut(&Value, &Decoded) -> Result<()>,
    {
        if self.fatal.is_some() || line.iter().all(u8::is_ascii_whitespace) {
            return;
        }
        let e: Value = match serde_json::from_slice(line) {
            Ok(e) => e,
            Err(_) => {
                tail(&mut self.stderr, &String::from_utf8_lossy(line));
                return;
            }
        };
        let mut d = provider::decode(adapter, &e);
        if let Some(ref err) = d.error {
            self.last_error = Some(err.clone());
            if provider::classify(err) == "limit" {
                self.forced = true;
            }
        }
        if d.complete {
            self.complete = true;
            if !self.has_text && d.text.is_empty() {
                d.text = d.final_text.clone().unwrap_or_default();
            }
        }
        self.has_text |= !d.text.is_empty();
        if let Err(err) = event(&e, &d) {
            self.fatal = Some(err.to_string());
        }
    }
    fn bytes<F>(&mut self, chunk: &[u8], adapter: &str, event: &mut F)
    where
        F: FnMut(&Value, &Decoded) -> Result<()>,
    {
        self.buffer.extend_from_slice(chunk);
        while let Some(n) = self.buffer.iter().position(|b| *b == b'\n') {
            if n > 16 * 1024 * 1024 {
                self.fatal = Some("Provider JSON event exceeds 16 MiB".into());
                return;
            }
            let line = self.buffer.drain(..=n).collect::<Vec<_>>();
            self.line(&line[..n], adapter, event);
        }
        if self.buffer.len() > 16 * 1024 * 1024 {
            self.fatal = Some("Provider JSON event exceeds 16 MiB".into());
        }
    }
}
pub fn run<F>(spec: Spec<'_>, cancel: &Arc<AtomicBool>, mut event: F) -> Result<Outcome>
where
    F: FnMut(&Value, &Decoded) -> Result<()>,
{
    if cancel.load(Ordering::Relaxed) {
        return Ok(Outcome::fail("cancelled", "Cancelled"));
    }
    let mut command = Command::new(spec.command);
    command
        .args(spec.args)
        .current_dir(spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if spec.authentication == "subscription" {
        for key in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "OPENAI_API_KEY",
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
        ] {
            command.env_remove(key);
        }
    }
    let mut child = match command.spawn() {
        Ok(c) => ChildGuard(c, true),
        Err(e) => {
            return Ok(Outcome::fail(
                if e.kind() == io::ErrorKind::NotFound {
                    "missing"
                } else {
                    "fatal"
                },
                e.to_string(),
            ));
        }
    };
    let mut input = child.0.stdin.take();
    let mut output = child.0.stdout.take().unwrap();
    let mut errors = child.0.stderr.take().unwrap();
    nonblocking(input.as_ref().unwrap().as_raw_fd())?;
    nonblocking(output.as_raw_fd())?;
    nonblocking(errors.as_raw_fd())?;
    let mut out_open = true;
    let mut err_open = true;
    let mut offset = 0;
    let mut chunk = [0u8; 65536];
    let mut state = StreamState {
        buffer: vec![],
        stderr: String::new(),
        last_error: None,
        forced: false,
        complete: false,
        has_text: false,
        fatal: None,
    };
    let start = Instant::now();
    let mut stopping = None;
    let mut exit_time = None;
    let mut exit = None;
    let mut cancelled = false;
    let mut timed_out = false;
    loop {
        cancelled |= cancel.load(Ordering::Relaxed);
        timed_out |= spec.timeout_ms > 0 && start.elapsed().as_millis() >= spec.timeout_ms as u128;
        if (cancelled || timed_out || state.forced || state.fatal.is_some()) && stopping.is_none() {
            signal_group(child.0.id(), libc::SIGTERM);
            stopping = Some(Instant::now());
            input.take();
        }
        if stopping.is_some_and(|t: Instant| t.elapsed() >= Duration::from_millis(1500)) {
            signal_group(child.0.id(), libc::SIGKILL);
        }
        if exit.is_none() {
            exit = child.0.try_wait()?;
            if exit.is_some() {
                exit_time = Some(Instant::now());
                input.take();
            }
        }
        if exit.is_some() && !out_open && !err_open {
            break;
        }
        if exit_time.is_some_and(|t: Instant| t.elapsed() > Duration::from_millis(1700)) {
            signal_group(child.0.id(), libc::SIGKILL);
            break;
        }
        if input.is_some() && offset >= spec.prompt.len() {
            input.take();
        }
        let mut fds = [
            libc::pollfd {
                fd: if out_open { output.as_raw_fd() } else { -1 },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if err_open { errors.as_raw_fd() } else { -1 },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: input.as_ref().map(AsRawFd::as_raw_fd).unwrap_or(-1),
                events: libc::POLLOUT,
                revents: 0,
            },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 25) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e.into());
        }
        if fds[2].revents != 0 {
            if let Some(ref mut f) = input {
                match f.write(&spec.prompt.as_bytes()[offset..]) {
                    Ok(n) => offset += n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                        input.take();
                    }
                    Err(e) => {
                        state.fatal = Some(e.to_string());
                    }
                }
            }
        }
        for (index, fd) in fds.iter().enumerate().take(2) {
            if fd.revents == 0 {
                continue;
            }
            let mut drained = 0;
            loop {
                let read = if index == 0 {
                    output.read(&mut chunk)
                } else {
                    errors.read(&mut chunk)
                };
                match read {
                    Ok(0) => {
                        if index == 0 {
                            out_open = false;
                        } else {
                            err_open = false;
                        }
                        break;
                    }
                    Ok(n) => {
                        drained += n;
                        if index == 0 {
                            state.bytes(&chunk[..n], spec.adapter, &mut event);
                        } else {
                            tail(&mut state.stderr, &String::from_utf8_lossy(&chunk[..n]));
                        }
                        if state.forced
                            || state.fatal.is_some()
                            || cancel.load(Ordering::Relaxed)
                            || drained >= 1024 * 1024
                        {
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        state.fatal = Some(e.to_string());
                        if index == 0 {
                            out_open = false;
                        } else {
                            err_open = false;
                        }
                        break;
                    }
                }
            }
        }
    }
    if !state.buffer.is_empty() {
        let line = std::mem::take(&mut state.buffer);
        state.line(&line, spec.adapter, &mut event);
    }
    let exit = match exit {
        Some(e) => e,
        None => child.0.wait()?,
    };
    child.1 = false;
    let failure = state
        .fatal
        .clone()
        .map(Value::String)
        .or(state.last_error.clone())
        .unwrap_or_else(|| {
            json!(if state.stderr.is_empty() {
                format!(
                    "Provider exited with code {:?} without a successful result event",
                    exit.code()
                )
            } else {
                state.stderr
            })
        });
    let status = if cancelled {
        "cancelled"
    } else if state.fatal.is_some() {
        "fatal"
    } else if state.forced {
        "limit"
    } else if timed_out {
        "timeout"
    } else if exit.success() && state.complete {
        "success"
    } else {
        provider::classify(&failure)
    };
    Ok(Outcome {
        status: status.into(),
        error: if status == "success" {
            None
        } else {
            Some(failure)
        },
        code: exit.code(),
        provider: None,
    })
}
