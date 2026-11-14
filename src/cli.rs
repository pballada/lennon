use crate::{
    Result, config, error,
    harness::{self, Display},
    provider::{Mode, Options},
    store::{self, Session},
    tui,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    env, fs,
    io::{self, IsTerminal, Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
const HELP: &str = "Usage: lennon [options] [command] [prompt]

Lennon - Claude Code that keeps working when you hit a usage limit. Starts an
interactive session by default; use -p/--print for non-interactive output.
When a provider reports a limit, the task continues on the next configured
provider (Claude Code, then Codex, then Gemini by default).

Arguments:
  prompt                                Your prompt

Options:
  --add-dir <directory>                 Additional directory to allow tool
                                        access to (repeatable)
  --allowedTools, --allowed-tools <tools>
      Comma-separated list of tool names to allow (e.g. \"Bash(git *),Edit\")
  -c, --continue                        Continue the most recent conversation in
                                        the current directory
  --config <path>                       Lennon configuration file to use
  --cwd <path>                          Project directory
  --disallowedTools, --disallowed-tools <tools>
      Comma-separated list of tool names to deny (e.g. \"Bash(git *),Edit\")
  -h, --help                            Display help for command
  --model <model>                       Model for the current session's primary
                                        provider (e.g. 'opus' or 'sonnet'), or
                                        PROVIDER=MODEL for any provider
                                        (repeatable)
  --order <providers>                   Comma-separated provider fallback order
  --output-format <format>              Output format (only works with --print):
                                        \"text\" (default), \"json\" (single
                                        result), or \"stream-json\" (realtime
                                        streaming) (choices: \"text\", \"json\",
                                        \"stream-json\")
  --permission-mode <mode>              Permission mode to use for the session
                                        (choices: \"manual\", \"acceptEdits\",
                                        \"plan\", \"auto\")
  -p, --print                           Print response and exit (useful for
                                        pipes)
  --provider <id>                       Use only one configured provider
  -r, --resume [sessionId]              Resume a conversation by session ID, or
                                        open interactive picker
  --retry                               Clear provider cooldowns before running
  --timeout <ms>                        Stop a stalled provider after ms
  --verbose                             Show provider handoffs and tool activity
  -v, --version                         Output the version number

Commands:
  config [show|init|set|unset]          Show or edit providers, models, and
                                        fallback order
  doctor                                Check the health of your provider
                                        installations
  providers                             List configured providers in order
  sessions                              List saved sessions

Lennon never bypasses permission checks. Log in with each provider's own CLI
first (claude, codex login, gemini). Handoffs transfer observable context, not
private model state.
";
#[derive(Default, Debug)]
pub struct Args {
    pub values: HashMap<String, String>,
    pub flags: HashMap<String, bool>,
    pub models: Vec<String>,
    pub positional: Vec<String>,
}
impl Args {
    fn value(&self, k: &str) -> Option<&str> {
        self.values.get(k).map(String::as_str)
    }
    fn flag(&self, k: &str) -> bool {
        self.flags.get(k) == Some(&true)
    }
}
const VALUES: [&str; 13] = [
    "prompt",
    "cwd",
    "order",
    "provider",
    "model",
    "config",
    "timeout",
    "output-format",
    "permission-mode",
    "add-dir",
    "allowedTools",
    "disallowedTools",
    "resume",
];
const FLAGS: [&str; 8] = [
    "print", "continue", "help", "version", "write", "retry", "json", "verbose",
];
const REPEATABLE: [&str; 4] = ["model", "add-dir", "allowedTools", "disallowedTools"];
pub fn parse(args: Vec<String>) -> Result<Args> {
    let mut out = Args::default();
    let mut it = args.into_iter().peekable();
    let mut literal = false;
    while let Some(arg) = it.next() {
        if literal || arg == "-" || !arg.starts_with('-') {
            out.positional.push(arg);
            continue;
        }
        if arg == "--" {
            literal = true;
            continue;
        }
        let (name, inline) = if let Some(a) = arg.strip_prefix("--") {
            let (n, v) = a
                .split_once('=')
                .map(|(a, b)| (a.to_owned(), Some(b.to_owned())))
                .unwrap_or_else(|| (a.to_owned(), None));
            let n = match n.as_str() {
                "allowed-tools" => "allowedTools".into(),
                "disallowed-tools" => "disallowedTools".into(),
                "dangerously-skip-permissions" | "allow-dangerously-skip-permissions" => {
                    return Err(error(
                        "Lennon never bypasses permission checks. Use --permission-mode acceptEdits or auto, or allow specific tools with --allowedTools.",
                    ));
                }
                _ => n,
            };
            (n, v)
        } else {
            let mut chars = arg[1..].chars();
            let short = chars.next().unwrap_or(' ');
            let name = match short {
                'p' => "print",
                'r' => "resume",
                'c' => "continue",
                'h' => "help",
                'v' => "version",
                _ => return Err(error(format!("Unknown option: {arg}"))),
            };
            let rest = chars.as_str();
            if rest.is_empty() {
                (name.into(), None)
            } else if name == "print" {
                // Lennon 0.2 accepted -pTEXT.
                ("prompt".into(), Some(rest.to_owned()))
            } else {
                (name.into(), Some(rest.to_owned()))
            }
        };
        if name == "resume" {
            // -r takes an optional session ID, like Claude Code.
            let value = inline.or_else(|| {
                it.next_if(|n| !n.starts_with('-') || n == "-")
                    .filter(|v| v != "-")
            });
            out.flags.insert("resume".into(), true);
            if let Some(v) = value {
                out.values.insert(name, v);
            }
        } else if VALUES.contains(&name.as_str()) {
            let value = inline
                .or_else(|| it.next())
                .ok_or_else(|| error(format!("--{name} requires a value")))?;
            if name == "model" {
                out.models.push(value);
            } else if REPEATABLE.contains(&name.as_str()) {
                let entry = out.values.entry(name).or_default();
                if !entry.is_empty() {
                    entry.push('\n');
                }
                entry.push_str(&value);
            } else {
                out.values.insert(name, value);
            }
        } else if FLAGS.contains(&name.as_str()) {
            if inline.is_some() {
                return Err(error(format!("--{name} does not accept a value")));
            }
            out.flags.insert(name, true);
        } else {
            return Err(error(format!("Unknown option: --{name}")));
        }
    }
    Ok(out)
}
impl Args {
    fn list(&self, k: &str) -> Vec<String> {
        self.value(k)
            .map(|v| v.split('\n').map(str::to_owned).collect())
            .unwrap_or_default()
    }
}
pub fn safe_text(text: &str) -> String {
    let mut out = String::new();
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            match it.peek().copied() {
                Some('[') => {
                    it.next();
                    for x in it.by_ref() {
                        if ('@'..='~').contains(&x) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    it.next();
                    while let Some(x) = it.next() {
                        if x == '\u{7}' {
                            break;
                        }
                        if x == '\u{1b}' && it.peek() == Some(&'\\') {
                            it.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if !c.is_control() || c == '\n' || c == '\t' {
            out.push(c);
        }
    }
    out
}
fn print(value: &str) -> Result<()> {
    writeln!(io::stdout(), "{value}")?;
    Ok(())
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
    StreamJson,
}
pub struct Output {
    pub id: String,
    pub format: Format,
    pub verbose: bool,
    pub result: String,
}
impl Output {
    pub fn emit(&mut self, mut v: Value) -> Result<()> {
        v["session"] = json!(self.id);
        print(&v.to_string())
    }
    fn stream(&self) -> bool {
        self.format == Format::StreamJson
    }
    fn note(&mut self, text: &str) -> Result<()> {
        writeln!(io::stderr(), "lennon: {}", safe_text(text))?;
        Ok(())
    }
}
impl Display for Output {
    fn status(&mut self, text: &str) -> Result<()> {
        match self.format {
            Format::StreamJson => self.emit(json!({"type":"status","text":text})),
            _ if self.verbose => self.note(text),
            _ => Ok(()),
        }
    }
    fn text(&mut self, text: &str) -> Result<()> {
        self.result.push_str(text);
        match self.format {
            Format::StreamJson => self.emit(json!({"type":"text","text":text})),
            Format::Json => Ok(()),
            Format::Text => {
                let mut out = io::stdout().lock();
                out.write_all(safe_text(text).as_bytes())?;
                out.flush()?;
                Ok(())
            }
        }
    }
    fn failover(&mut self, id: &str, status: &str, reason: &str) -> Result<()> {
        if self.stream() {
            return self.emit(
                json!({"type":"status","text":format!("{id}: {status}; trying next provider")}),
            );
        }
        // Separate one provider's partial answer from the next one's.
        if !self.result.is_empty() && !self.result.ends_with('\n') {
            self.text("\n")?;
        }
        let what = match status {
            "limit" => "hit its usage limit",
            "transient" => "is temporarily unavailable",
            "missing" => "is not installed",
            _ => "stopped",
        };
        let detail = if self.verbose && !reason.is_empty() {
            format!(" ({})", reason.lines().next().unwrap_or(""))
        } else {
            String::new()
        };
        self.note(&format!(
            "{id} {what}{detail}; continuing with the next provider"
        ))
    }
}
fn read_input(cancel: &Arc<AtomicBool>, line: bool) -> Result<Option<String>> {
    let mut bytes = vec![];
    let mut chunk = [0u8; 8192];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let mut fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut fd, 1, 25) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e.into());
        }
        if rc == 0 {
            continue;
        }
        // In canonical terminal mode a read ends at a newline. Reading one byte
        // for conversations also prevents swallowing future piped commands.
        let cap = if line { 1 } else { chunk.len() };
        let n = unsafe { libc::read(libc::STDIN_FILENO, chunk.as_mut_ptr().cast(), cap) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e.into());
        }
        if n == 0 {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8(bytes)?))
            };
        }
        bytes.extend_from_slice(&chunk[..n as usize]);
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(error("Prompt exceeds 4 MiB"));
        }
        if line && bytes.last() == Some(&b'\n') {
            return Ok(Some(String::from_utf8(bytes)?));
        }
    }
}
fn run_turn(
    session: &mut Session,
    prompt: Option<&str>,
    c: &Value,
    opts: &Options,
    timeout: u64,
    cancel: &Arc<AtomicBool>,
    output: &mut Output,
) -> Result<i32> {
    let start = Instant::now();
    let result = harness::turn(session, prompt, c, opts, timeout, cancel, output)?;
    let id = session.meta["id"].as_str().unwrap_or("").to_owned();
    let error_text = result.error.as_ref().map(|e| {
        e.as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| e.to_string())
    });
    match output.format {
        Format::StreamJson => {
            let mut v = json!({"type":"result","status":result.status});
            if let Some(p) = &result.provider {
                v["provider"] = json!(p);
            }
            if let Some(e) = &result.error {
                v["error"] = e.clone();
            }
            if let Some(code) = result.code {
                v["code"] = json!(code);
            }
            output.emit(v)?;
        }
        Format::Json => {
            let ok = result.status == "success";
            let v = json!({
                "type":"result",
                "subtype": if ok { "success".to_string() } else { format!("error_{}", result.status) },
                "is_error": !ok,
                "duration_ms": start.elapsed().as_millis() as u64,
                "result": if ok { json!(output.result) } else { json!(error_text) },
                "session_id": id,
                "provider": result.provider,
                "status": result.status,
            });
            print(&v.to_string())?;
        }
        Format::Text => {
            if !output.result.ends_with('\n') {
                print("")?;
            }
        }
    }
    if result.status != "success" {
        if output.format == Format::Text {
            if let Some(e) = &error_text {
                output.note(e)?;
            }
            output.note(&format!(
                "Saved. Continue with: lennon --resume {id}{}",
                if opts.mode == Mode::Manual {
                    String::new()
                } else {
                    format!(" --permission-mode {}", opts.mode.name())
                }
            ))?;
        }
        return Ok(if result.status == "cancelled" { 130 } else { 1 });
    }
    Ok(0)
}
fn doctor(c: &Value, cancel: &Arc<AtomicBool>) -> Result<i32> {
    let (lines, available) = doctor_lines(c, cancel)?;
    for l in lines {
        print(&l)?;
    }
    Ok(if available > 0 { 0 } else { 1 })
}
/// Check each enabled provider executable. Returns report lines and the
/// number of providers that answered.
pub fn doctor_lines(c: &Value, cancel: &Arc<AtomicBool>) -> Result<(Vec<String>, usize)> {
    let mut lines = vec![];
    let mut available = 0;
    for id in c["order"].as_array().unwrap() {
        let id = id.as_str().unwrap();
        let p = &c["providers"][id];
        if p["enabled"] != true {
            continue;
        }
        let cmd = p["command"]
            .as_str()
            .unwrap_or(p["adapter"].as_str().unwrap());
        // Drain output on a reader thread so a verbose version command cannot
        // block the timeout or fill a pipe.
        let mut command = std::process::Command::new(cmd);
        command
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        match command.spawn() {
            Err(e) => lines.push(format!("{id:<7} unavailable: {e}")),
            Ok(mut child) => {
                let mut stream = child.stdout.take().unwrap();
                let reader = std::thread::spawn(move || {
                    let mut b = String::new();
                    let _ = stream.by_ref().take(32768).read_to_string(&mut b);
                    b
                });
                let start = Instant::now();
                let mut killed = false;
                let status = loop {
                    if let Some(s) = child.try_wait()? {
                        break s;
                    }
                    if cancel.load(Ordering::Relaxed) || start.elapsed() > Duration::from_secs(10) {
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        }
                        killed = true;
                        break child.wait()?;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                };
                // Terminate descendants that still hold stdout after parent exit.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGTERM);
                }
                let text = reader.join().unwrap_or_default();
                if status.success() && !killed {
                    available += 1;
                    lines.push(format!("{id:<7} {}", safe_text(text.trim())));
                } else {
                    lines.push(format!(
                        "{id:<7} unavailable: version command failed or timed out"
                    ));
                }
            }
        }
    }
    lines.push(
        "Login using: claude / codex login / gemini. Doctor checks executables, not account quotas."
            .into(),
    );
    Ok((lines, available))
}
pub fn main_entry() -> Result<i32> {
    let args = parse(env::args().skip(1).collect())?;
    if args.flag("help") {
        print(HELP)?;
        return Ok(0);
    }
    let sub = args.positional.first().map(String::as_str).unwrap_or("");
    if args.flag("version") || sub == "version" {
        print(&format!("{} (Lennon)", env!("CARGO_PKG_VERSION")))?;
        return Ok(0);
    }
    let path = config::config_path(args.value("config"));
    let root = config::state_root();
    if sub == "config" && args.positional.len() > 1 && args.positional[1] != "show" {
        config::edit(&path, &args.positional[1..])?;
        print(&path.display().to_string())?;
        return Ok(0);
    }
    // PROVIDER=MODEL assignments go to that entry; a bare model goes to the
    // primary provider, as `claude --model opus` would.
    let (assigned, bare): (Vec<String>, Vec<String>) =
        args.models.iter().cloned().partition(|m| m.contains('='));
    if bare.len() > 1 {
        return Err(error(
            "--model accepts one model for the primary provider; use PROVIDER=MODEL for others",
        ));
    }
    let mut c = config::overrides(
        config::normalize(config::read(&path, args.value("config").is_some())?)?,
        args.value("order"),
        args.value("provider"),
        &assigned,
    )?;
    if let Some(model) = bare.first() {
        let primary = c["order"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .find(|id| c["providers"][*id]["enabled"] == true)
            .ok_or_else(|| error("No enabled provider"))?
            .to_owned();
        if model.trim().is_empty() {
            return Err(error("--model requires a model name"));
        }
        c["providers"][&primary]["model"] = json!(model);
    }
    if sub == "config" {
        if args.positional.len() > 2 {
            return Err(error("Usage: lennon config [show]"));
        }
        let mut v = c;
        v["path"] = json!(path);
        v["state"] = json!(root);
        print(&serde_json::to_string_pretty(&v)?)?;
        return Ok(0);
    }
    let json_lines = args.flag("json");
    if sub == "providers" {
        for (id, p) in c["providers"].as_object().unwrap() {
            let order = c["order"].as_array().unwrap().iter().position(|v| v == id);
            if json_lines {
                let mut v = p.clone();
                v["id"] = json!(id);
                v["order"] = order.map(|n| json!(n + 1)).unwrap_or(Value::Null);
                print(&v.to_string())?;
            } else {
                print(&format!(
                    "{id}  {}  {}  {}",
                    p["adapter"].as_str().unwrap(),
                    p["model"].as_str().unwrap_or("(native default)"),
                    if p["enabled"] != true {
                        "disabled".into()
                    } else {
                        order
                            .map(|n| format!("#{}", n + 1))
                            .unwrap_or("not in order".into())
                    }
                ))?;
            }
        }
        return Ok(0);
    }
    if sub == "sessions" {
        for s in store::list(&root)? {
            if json_lines {
                print(&s.to_string())?;
            } else {
                let id = s["id"].as_str().unwrap_or("");
                print(&format!(
                    "{}  {}  {}  {}  {}",
                    id,
                    s["updated"].as_str().unwrap_or(""),
                    if s["pending"] == true {
                        "unfinished"
                    } else {
                        "complete"
                    },
                    safe_text(s["cwd"].as_str().unwrap_or("")),
                    safe_text(
                        &store::title(&root, id)
                            .unwrap_or_default()
                            .replace('\n', " ")
                            .chars()
                            .take(60)
                            .collect::<String>()
                    )
                ))?;
            }
        }
        return Ok(0);
    }
    let cancel = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(libc::SIGINT, cancel.clone())?;
    signal_hook::flag::register(libc::SIGTERM, cancel.clone())?;
    signal_hook::flag::register(libc::SIGHUP, cancel.clone())?;
    if sub == "doctor" {
        return doctor(&c, &cancel);
    }
    let timeout = args
        .value("timeout")
        .unwrap_or("0")
        .parse::<u64>()
        .map_err(|_| error("--timeout must be a non-negative integer"))?;
    if timeout > 9007199254740991 {
        return Err(error("--timeout must be a non-negative safe integer"));
    }
    let format = match (args.value("output-format"), json_lines) {
        (None, false) | (Some("text"), false) => Format::Text,
        (None, true) | (Some("stream-json"), _) => Format::StreamJson,
        (Some("json"), false) => Format::Json,
        (Some(f), _) => {
            return Err(error(format!(
                "Invalid --output-format {f}. Choose text, json, or stream-json."
            )));
        }
    };
    let mut mode = match args.value("permission-mode") {
        Some(m) => Mode::parse(m)?,
        None => Mode::Manual,
    };
    if args.flag("write") {
        if args.value("permission-mode").is_some() && !mode.writes() {
            return Err(error("--write conflicts with --permission-mode"));
        }
        if !mode.writes() {
            mode = Mode::AcceptEdits;
        }
    }
    let opts = Options {
        mode,
        add_dirs: args
            .list("add-dir")
            .iter()
            .map(|d| fs::canonicalize(d).map(|p| p.display().to_string()))
            .collect::<std::io::Result<_>>()
            .map_err(|e| error(format!("--add-dir: {e}")))?,
        allowed_tools: args.list("allowedTools"),
        disallowed_tools: args.list("disallowedTools"),
    };
    if args.flag("resume") && args.flag("continue") {
        return Err(error("Choose --resume or --continue"));
    }
    let cwd = fs::canonicalize(args.value("cwd").map(Path::new).unwrap_or(Path::new(".")))?;
    let mut prompt = args.value("prompt").map(str::to_owned).or_else(|| {
        if args.positional.is_empty() {
            None
        } else {
            Some(args.positional.join(" "))
        }
    });
    let tty = io::stdin().is_terminal();
    let interactive = !args.flag("print")
        && args.value("prompt").is_none()
        && format == Format::Text
        && tty
        && io::stdout().is_terminal();
    let picker = args.flag("resume") && args.value("resume").is_none();
    if picker && !interactive {
        return Err(error(
            "--resume requires a session ID with --print; run `lennon sessions` to list them",
        ));
    }
    let id = if args.flag("continue") {
        Some(
            store::list(&root)?
                .iter()
                .find(|s| s["cwd"] == json!(cwd))
                .and_then(|s| s["id"].as_str())
                .ok_or_else(|| error("No conversation found to continue"))?
                .to_owned(),
        )
    } else {
        args.value("resume").map(str::to_owned)
    };
    if !tty {
        if let Some(piped) = read_input(&cancel, false)?.filter(|s| !s.trim().is_empty()) {
            prompt = Some(prompt.map(|p| format!("{p}\n\n{piped}")).unwrap_or(piped));
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(130);
    }
    if !interactive && prompt.is_none() && id.is_none() {
        return Err(error(
            "Input must be provided either through stdin or as a prompt argument when using --print",
        ));
    }
    let mut session = if let Some(id) = id {
        let s = Session::open(&root, &id)
            .map_err(|_| error(format!("No conversation found with session ID: {id}")))?;
        if args.value("cwd").is_some() && s.meta["cwd"] != json!(cwd) {
            return Err(error("Session belongs to a different working directory"));
        }
        s
    } else {
        Session::create(&root, &cwd)?
    };
    session.acquire()?;
    if args.flag("retry") {
        session.meta["cooldowns"] = json!({});
        session.save()?;
    }
    if interactive {
        return tui::run(
            tui::Ctx {
                root,
                config_path: path,
                config: c,
                opts,
                timeout,
                cancel,
                verbose: args.flag("verbose"),
            },
            session,
            prompt,
            picker,
        );
    }
    let mut output = Output {
        id: session.meta["id"].as_str().unwrap().into(),
        format,
        verbose: args.flag("verbose"),
        result: String::new(),
    };
    output.status(&format!(
        "session {} · {}",
        output.id,
        session.meta["cwd"].as_str().unwrap()
    ))?;
    run_turn(
        &mut session,
        prompt.as_deref(),
        &c,
        &opts,
        timeout,
        &cancel,
        &mut output,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_compatibility() {
        let a = parse(vec![
            "--model=claude=sonnet".into(),
            "-p".into(),
            "task".into(),
            "-r".into(),
            "--order".into(),
            "claude,codex".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(a.models, ["claude=sonnet"]);
        assert!(a.flag("print"));
        assert_eq!(a.positional, ["task"]);
        assert!(a.flag("resume") && a.value("resume").is_none());
        assert!(a.flag("json"));
        let b = parse(vec![
            "-r".into(),
            "abc".into(),
            "--allowed-tools".into(),
            "Read".into(),
            "--allowedTools=Edit".into(),
        ])
        .unwrap();
        assert_eq!(b.value("resume"), Some("abc"));
        assert_eq!(b.list("allowedTools"), ["Read", "Edit"]);
        assert!(parse(vec!["--dangerously-skip-permissions".into()]).is_err());
        assert!(parse(vec!["--unknown".into()]).is_err());
        assert!(parse(vec!["--model".into()]).is_err());
    }
    #[test]
    fn terminal_sanitization() {
        assert_eq!(
            safe_text("\x1b[31mred\x1b[0m\x1b]0;title\x07\x00\n🐕"),
            "red\n🐕"
        );
        assert_eq!(
            safe_text("\x1b]8;;http://untrusted\x1b\\text\x1b]8;;\x1b\\"),
            "text"
        );
    }
}
