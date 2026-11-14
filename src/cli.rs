use crate::{
    Result, config, error,
    harness::{self, Display},
    store::{self, Session},
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
const HELP: &str = "lennon · the loyal coding harness\n\nUsage: lennon [options] [prompt]\n       lennon doctor | sessions | providers | config | version\n\n  -p, --prompt TEXT      Run one task (also accepts piped stdin)\n  -r, --resume ID        Resume a session; no prompt retries unfinished work\n  -c, --continue         Latest session in the current directory\n      --cwd PATH        Project directory\n      --order LIST      Ordered configured provider IDs\n      --provider ID     Use a single configured provider entry\n      --model ID=MODEL  Override a model (repeatable)\n      --config PATH     Select a configuration file\n      --write           Allow edits using native CLI permission policies\n      --timeout MS      Stop a provider after MS; no automatic retry\n      --retry           Clear provider cooldowns\n      --json            Emit JSONL\n  -h, --help            Help\n  -v, --version         Version\n\nConfiguration:\n  lennon config init\n  lennon config set order codex,claude,gemini\n  lennon config set providers.claude.model sonnet\n  lennon config set providers.gemini.enabled false\n  lennon config unset providers.claude.model\n\nInteractive commands: /status /retry /help /exit\nRead-only tools by default. Login with the native CLIs first.\nHandoffs transfer observable context, not private model state.\n";
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
pub fn parse(args: Vec<String>) -> Result<Args> {
    let mut out = Args::default();
    let mut it = args.into_iter();
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
            a.split_once('=')
                .map(|(a, b)| (a.to_owned(), Some(b.to_owned())))
                .unwrap_or_else(|| (a.to_owned(), None))
        } else {
            let mut chars = arg[1..].chars();
            let short = chars.next().unwrap_or(' ');
            let name = match short {
                'p' => "prompt",
                'r' => "resume",
                'c' => "continue",
                'h' => "help",
                'v' => "version",
                _ => return Err(error(format!("Unknown option: {arg}"))),
            };
            let rest = chars.as_str();
            (
                name.into(),
                if rest.is_empty() {
                    None
                } else {
                    Some(rest.to_owned())
                },
            )
        };
        if [
            "prompt", "resume", "cwd", "order", "provider", "model", "config", "timeout",
        ]
        .contains(&name.as_str())
        {
            let value = inline
                .or_else(|| it.next())
                .ok_or_else(|| error(format!("--{name} requires a value")))?;
            if name == "model" {
                out.models.push(value);
            } else {
                out.values.insert(name, value);
            }
        } else if ["continue", "help", "version", "write", "retry", "json"].contains(&name.as_str())
        {
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
pub struct Output {
    pub id: String,
    pub json: bool,
}
impl Output {
    pub fn emit(&mut self, mut v: Value) -> Result<()> {
        v["session"] = json!(self.id);
        print(&v.to_string())
    }
}
impl Display for Output {
    fn status(&mut self, text: &str) -> Result<()> {
        if self.json {
            self.emit(json!({"type":"status","text":text}))
        } else {
            writeln!(io::stderr(), "\n[lennon] {}", safe_text(text))?;
            Ok(())
        }
    }
    fn text(&mut self, text: &str) -> Result<()> {
        if self.json {
            self.emit(json!({"type":"text","text":text}))
        } else {
            let mut out = io::stdout().lock();
            out.write_all(safe_text(text).as_bytes())?;
            out.flush()?;
            Ok(())
        }
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
    args: &Args,
    timeout: u64,
    cancel: &Arc<AtomicBool>,
    output: &mut Output,
) -> Result<i32> {
    let result = harness::turn(
        session,
        prompt,
        c,
        args.flag("write"),
        timeout,
        cancel,
        output,
    )?;
    if output.json {
        let mut v = json!({"type":"result","status":result.status});
        if let Some(p) = result.provider {
            v["provider"] = json!(p);
        }
        if let Some(e) = &result.error {
            v["error"] = e.clone();
        }
        if let Some(code) = result.code {
            v["code"] = json!(code);
        }
        output.emit(v)?;
    } else {
        print("")?;
    }
    if result.status != "success" {
        if let Some(e) = result.error {
            output.status(
                &e.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| e.to_string()),
            )?;
        }
        output.status(&format!(
            "Saved. Continue with: lennon --resume {}{}",
            session.meta["id"].as_str().unwrap(),
            if args.flag("write") { " --write" } else { "" }
        ))?;
        return Ok(if result.status == "cancelled" { 130 } else { 1 });
    }
    Ok(0)
}
fn doctor(c: &Value, cancel: &Arc<AtomicBool>) -> Result<i32> {
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
            Err(e) => print(&format!("{id:<7} unavailable: {e}"))?,
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
                    print(&format!("{id:<7} {}", safe_text(text.trim())))?;
                } else {
                    print(&format!(
                        "{id:<7} unavailable: version command failed or timed out"
                    ))?;
                }
            }
        }
    }
    print(
        "Login using: claude / codex login / gemini. Doctor checks executables, not account quotas.",
    )?;
    Ok(if available > 0 { 0 } else { 1 })
}
pub fn main_entry() -> Result<i32> {
    let args = parse(env::args().skip(1).collect())?;
    if args.flag("help") {
        print(HELP)?;
        return Ok(0);
    }
    let sub = args.positional.first().map(String::as_str).unwrap_or("");
    if args.flag("version") || sub == "version" {
        print(env!("CARGO_PKG_VERSION"))?;
        return Ok(0);
    }
    let path = config::config_path(args.value("config"));
    let root = config::state_root();
    if sub == "config" && args.positional.len() > 1 && args.positional[1] != "show" {
        config::edit(&path, &args.positional[1..])?;
        print(&path.display().to_string())?;
        return Ok(0);
    }
    let c = config::overrides(
        config::normalize(config::read(&path, args.value("config").is_some())?)?,
        args.value("order"),
        args.value("provider"),
        &args.models,
    )?;
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
    if sub == "providers" {
        for (id, p) in c["providers"].as_object().unwrap() {
            let order = c["order"].as_array().unwrap().iter().position(|v| v == id);
            if args.flag("json") {
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
            if args.flag("json") {
                print(&s.to_string())?;
            } else {
                print(&format!(
                    "{}  {}  {}  {}",
                    s["id"].as_str().unwrap_or(""),
                    s["updated"].as_str().unwrap_or(""),
                    if s["pending"] == true {
                        "unfinished"
                    } else {
                        "complete"
                    },
                    safe_text(s["cwd"].as_str().unwrap_or(""))
                ))?;
            }
        }
        return Ok(0);
    }
    let cancel = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(libc::SIGINT, cancel.clone())?;
    signal_hook::flag::register(libc::SIGTERM, cancel.clone())?;
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
    if args.value("resume").is_some() && args.flag("continue") {
        return Err(error("Choose --resume or --continue"));
    }
    let cwd = fs::canonicalize(args.value("cwd").map(Path::new).unwrap_or(Path::new(".")))?;
    let id = if args.flag("continue") {
        Some(
            store::list(&root)?
                .iter()
                .find(|s| s["cwd"] == json!(cwd))
                .and_then(|s| s["id"].as_str())
                .ok_or_else(|| error("No previous Lennon session in this directory"))?
                .to_owned(),
        )
    } else {
        args.value("resume").map(str::to_owned)
    };
    let mut prompt = args.value("prompt").map(str::to_owned).or_else(|| {
        if args.positional.is_empty() {
            None
        } else {
            Some(args.positional.join(" "))
        }
    });
    let tty = io::stdin().is_terminal();
    let one_shot = prompt.is_some() || !tty || id.is_some();
    if !tty {
        if let Some(piped) = read_input(&cancel, false)?.filter(|s| !s.trim().is_empty()) {
            prompt = Some(prompt.map(|p| format!("{p}\n\n{piped}")).unwrap_or(piped));
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(130);
    }
    if !one_shot && args.flag("json") {
        return Err(error(
            "Interactive mode requires text output; supply --prompt with --json",
        ));
    }
    let mut session = if let Some(id) = id {
        let s = Session::open(&root, &id)?;
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
    let mut output = Output {
        id: session.meta["id"].as_str().unwrap().into(),
        json: args.flag("json"),
    };
    output.status(&format!(
        "session {} · {}",
        output.id,
        session.meta["cwd"].as_str().unwrap()
    ))?;
    if one_shot {
        return run_turn(
            &mut session,
            prompt.as_deref(),
            &c,
            &args,
            timeout,
            &cancel,
            &mut output,
        );
    }
    let order = c["order"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| c["providers"][v.as_str().unwrap()]["enabled"] == true)
        .map(|v| v.as_str().unwrap())
        .collect::<Vec<_>>()
        .join(" → ");
    eprintln!("lennon · /help for commands · {order}");
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        write!(io::stdout(), "\nlennon › ")?;
        io::stdout().flush()?;
        let Some(line) = read_input(&cancel, true)? else {
            break;
        };
        let text = line.trim();
        match text {
            "" => continue,
            "/exit" | "/quit" => break,
            "/help" => {
                print("/status · /retry · /exit\nType a task to continue the same session.")?
            }
            "/status" => print(&serde_json::to_string_pretty(&session.meta)?)?,
            "/retry" => {
                session.meta["cooldowns"] = json!({});
                session.save()?;
                if session.meta["pending"] == true
                    && run_turn(&mut session, None, &c, &args, timeout, &cancel, &mut output)?
                        == 130
                {
                    break;
                }
            }
            _ if text.starts_with('/') => print("Unknown command. Use /help.")?,
            _ => {
                if run_turn(
                    &mut session,
                    Some(text),
                    &c,
                    &args,
                    timeout,
                    &cancel,
                    &mut output,
                )? == 130
                {
                    break;
                }
            }
        }
    }
    Ok(if cancel.load(Ordering::Relaxed) {
        130
    } else {
        0
    })
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
            "--order".into(),
            "claude,codex".into(),
            "--json".into(),
        ])
        .unwrap();
        assert_eq!(a.models, ["claude=sonnet"]);
        assert_eq!(a.value("prompt"), Some("task"));
        assert!(a.flag("json"));
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
