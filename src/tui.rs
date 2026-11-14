//! The interactive interface, modelled on Claude Code's terminal UI.
//!
//! Output is rendered inline: finished transcript blocks scroll into the
//! terminal's scrollback, and a live region at the bottom (spinner, running
//! tools, prompt box, footer) is redrawn in place.
use crate::{
    Result, cli, config, error,
    harness::{self, Display},
    provider::{Decoded, Mode, Options},
    runner::Outcome,
    store::{self, Session, private_options},
    view::{
        self, ACCENT, BASH, DIM, EDITS, ERROR, Item, Line, PLAN, RULE, SUCCESS, Style, Tool,
        WARNING,
    },
};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    env, fs,
    io::{self, Read, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub struct Ctx {
    pub root: PathBuf,
    pub config_path: PathBuf,
    pub config: Value,
    pub opts: Options,
    pub timeout: u64,
    pub cancel: Arc<AtomicBool>,
    pub verbose: bool,
}

const BUILTINS: [(&str, &str); 13] = [
    ("clear", "Clear conversation history and free up context"),
    ("config", "Show Lennon configuration"),
    (
        "cost",
        "Show the total cost and duration of the current session",
    ),
    ("doctor", "Check the health of your provider installations"),
    ("exit", "Exit the REPL"),
    ("help", "Show help and available commands"),
    (
        "init",
        "Initialize a new CLAUDE.md file with codebase documentation",
    ),
    ("memory", "Edit the project CLAUDE.md memory file"),
    ("model", "Set the AI model for Lennon"),
    ("providers", "Show the provider fallback chain"),
    ("resume", "Resume a conversation"),
    (
        "retry",
        "Retry unfinished work now and clear provider cooldowns",
    ),
    (
        "status",
        "Show Lennon status including version, providers, and session",
    ),
];
const ALIASES: [(&str, &str); 4] = [
    ("quit", "exit"),
    ("reset", "clear"),
    ("new", "clear"),
    ("continue", "resume"),
];
const PLACEHOLDERS: [&str; 6] = [
    "Try \"explain this codebase\"",
    "Try \"fix lint errors\"",
    "Try \"write a test for <filepath>\"",
    "Try \"how does <filepath> work?\"",
    "Try \"refactor <filepath>\"",
    "Try \"edit <filepath> to...\"",
];
const INIT_PROMPT: &str = "Analyze this codebase and create a CLAUDE.md file in the repository root for future coding agents. Include the commands used to build, lint, and test (including how to run a single test) and a concise overview of the architecture and conventions that require reading several files to understand. Incorporate important rules from files such as AGENTS.md, .cursorrules, .github/copilot-instructions.md, and README.md. If CLAUDE.md already exists, suggest improvements to it instead. Do not list obvious or generic advice.";

// ───────────────────────── transcript blocks ─────────────────────────

#[derive(Clone, Debug)]
enum Block {
    Header,
    User(String),
    Text {
        text: String,
        first: bool,
    },
    Tool {
        tool: Tool,
        result: Option<(String, bool)>,
    },
    Quiet([usize; 3]),
    /// "  ⎿  " rows, e.g. slash-command output.
    Out(Vec<(Style, String)>),
    /// A "⏺" note in a colour, e.g. a provider handoff.
    Note(u32, String),
    BashIn(String),
    Done(String),
}

struct Pending {
    tool: Tool,
    result: Option<(String, bool)>,
}

/// Accumulates one turn's provider items into committed blocks.
#[derive(Default)]
struct Live {
    text: String,
    text_started: bool,
    tools: Vec<Pending>,
    seen: HashSet<String>,
    quiet: [usize; 3],
    quiet_ids: Vec<String>,
    quiet_open: bool,
    provider: Option<String>,
    failover: Option<(String, String)>,
    output_tokens: u64,
}
impl Live {
    fn feed(&mut self, item: Item, verbose: bool, out: &mut Vec<Block>) {
        match item {
            Item::Text(s) => {
                self.close_quiet(out);
                self.commit_ready(out);
                self.text.push_str(&s);
                self.commit_paragraphs(out);
            }
            Item::Block(s) => {
                self.close_quiet(out);
                self.commit_ready(out);
                self.flush_text(out);
                if !s.trim().is_empty() {
                    out.push(Block::Text {
                        text: s,
                        first: true,
                    });
                }
            }
            Item::ToolUse(t) => {
                if !t.id.is_empty() && !self.seen.insert(t.id.clone()) {
                    return;
                }
                self.flush_text(out);
                if let (Some(q), false) = (t.quiet, verbose) {
                    let i = match q {
                        view::Quiet::Read => 0,
                        view::Quiet::Search => 1,
                        view::Quiet::List => 2,
                    };
                    self.quiet[i] += 1;
                    self.quiet_ids.push(t.id);
                    self.quiet_open = true;
                } else {
                    self.close_quiet(out);
                    self.tools.push(Pending {
                        tool: t,
                        result: None,
                    });
                }
            }
            Item::ToolResult { id, output, error } => {
                if let Some(id) = id.as_ref().filter(|i| !i.is_empty()) {
                    if let Some(at) = self.quiet_ids.iter().position(|q| q == id) {
                        self.quiet_ids.remove(at);
                        return;
                    }
                }
                let slot = self
                    .tools
                    .iter()
                    .position(|p| {
                        id.as_deref()
                            .is_some_and(|i| !i.is_empty() && p.tool.id == i)
                    })
                    .or_else(|| self.tools.iter().position(|p| p.result.is_none()));
                match slot {
                    Some(i) => self.tools[i].result = Some((output, error)),
                    None if !self.quiet_ids.is_empty() => {
                        self.quiet_ids.remove(0);
                    }
                    None => {}
                }
                self.commit_ready(out);
            }
            Item::Usage { output, .. } => self.output_tokens += output,
        }
    }
    fn commit_paragraphs(&mut self, out: &mut Vec<Block>) {
        let mut fence = false;
        let mut split = None;
        let mut pos = 0;
        for line in self.text.split_inclusive('\n') {
            let start = pos;
            pos += line.len();
            if line.trim_start().starts_with("```") {
                fence = !fence;
            }
            if !fence && line.trim().is_empty() && line.ends_with('\n') && start > 0 {
                split = Some(start);
            }
        }
        if let Some(s) = split {
            let chunk = self.text[..s].to_string();
            if !chunk.trim().is_empty() {
                out.push(Block::Text {
                    text: chunk,
                    first: !self.text_started,
                });
                self.text_started = true;
            }
            self.text = self.text[s..].trim_start_matches('\n').to_string();
        }
    }
    fn flush_text(&mut self, out: &mut Vec<Block>) {
        if !self.text.trim().is_empty() {
            out.push(Block::Text {
                text: std::mem::take(&mut self.text),
                first: !self.text_started,
            });
        }
        self.text.clear();
        self.text_started = false;
    }
    fn close_quiet(&mut self, out: &mut Vec<Block>) {
        if self.quiet_open {
            out.push(Block::Quiet(self.quiet));
            self.quiet = [0; 3];
            self.quiet_open = false;
            self.quiet_ids.clear();
        }
    }
    fn commit_ready(&mut self, out: &mut Vec<Block>) {
        while self.tools.first().is_some_and(|p| p.result.is_some()) {
            let p = self.tools.remove(0);
            out.push(Block::Tool {
                tool: p.tool,
                result: p.result,
            });
        }
    }
    fn finish(&mut self, interrupted: bool, out: &mut Vec<Block>) {
        self.flush_text(out);
        self.close_quiet(out);
        for p in self.tools.drain(..) {
            out.push(Block::Tool {
                tool: p.tool,
                result: p
                    .result
                    .or_else(|| interrupted.then(|| ("Interrupted".to_string(), true))),
            });
        }
    }
}

// ───────────────────────── prompt editor ─────────────────────────

#[derive(Default)]
struct Editor {
    buf: String,
    cur: usize,
    bash: bool,
    kill: String,
    pastes: Vec<String>,
}
impl Editor {
    fn insert(&mut self, s: &str) {
        self.buf.insert_str(self.cur, s);
        self.cur += s.len();
    }
    fn prev(&self) -> usize {
        self.buf[..self.cur]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
    fn next(&self) -> usize {
        self.buf[self.cur..]
            .chars()
            .next()
            .map(|c| self.cur + c.len_utf8())
            .unwrap_or(self.cur)
    }
    fn backspace(&mut self) {
        if self.cur > 0 {
            let p = self.prev();
            self.buf.replace_range(p..self.cur, "");
            self.cur = p;
        }
    }
    fn delete(&mut self) {
        let n = self.next();
        self.buf.replace_range(self.cur..n, "");
    }
    fn word_left(&self) -> usize {
        let b = &self.buf[..self.cur];
        let t = b.trim_end_matches(|c: char| !c.is_alphanumeric());
        t.rfind(|c: char| !c.is_alphanumeric())
            .map(|i| i + t[i..].chars().next().unwrap().len_utf8())
            .unwrap_or(0)
    }
    fn word_right(&self) -> usize {
        let a = &self.buf[self.cur..];
        let skip = a.len() - a.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
        let rest = &a[skip..];
        self.cur
            + skip
            + rest
                .find(|c: char| !c.is_alphanumeric())
                .unwrap_or(rest.len())
    }
    fn line_start(&self) -> usize {
        self.buf[..self.cur].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }
    fn line_end(&self) -> usize {
        self.buf[self.cur..]
            .find('\n')
            .map(|i| self.cur + i)
            .unwrap_or(self.buf.len())
    }
    fn cut(&mut self, from: usize, to: usize) {
        if from < to {
            self.kill = self.buf[from..to].to_string();
            self.buf.replace_range(from..to, "");
            self.cur = from;
        }
    }
    /// Move vertically within a multi-line prompt; false at the edge.
    fn vertical(&mut self, up: bool) -> bool {
        let start = self.line_start();
        let col = self.buf[start..self.cur].chars().count();
        let target = if up {
            if start == 0 {
                return false;
            }
            self.buf[..start - 1]
                .rfind('\n')
                .map(|i| i + 1)
                .unwrap_or(0)
        } else {
            let end = self.line_end();
            if end == self.buf.len() {
                return false;
            }
            end + 1
        };
        let line_end = self.buf[target..]
            .find('\n')
            .map(|i| target + i)
            .unwrap_or(self.buf.len());
        self.cur = self.buf[target..line_end]
            .char_indices()
            .nth(col)
            .map(|(i, _)| target + i)
            .unwrap_or(line_end);
        true
    }
    fn set(&mut self, text: &str) {
        self.buf = text.to_string();
        self.cur = self.buf.len();
    }
    fn clear(&mut self) {
        self.buf.clear();
        self.cur = 0;
        self.bash = false;
        self.pastes.clear();
    }
    fn take(&mut self) -> String {
        let mut text = std::mem::take(&mut self.buf);
        for (i, p) in self.pastes.iter().enumerate() {
            let tag = paste_tag(i, p);
            text = text.replace(&tag, p);
        }
        self.clear();
        text
    }
}
fn paste_tag(i: usize, text: &str) -> String {
    format!("[Pasted text #{} +{} lines]", i + 1, text.lines().count())
}

// ───────────────────────── menus and pickers ─────────────────────────

struct MenuItem {
    insert: String,
    label: String,
    detail: String,
}
#[derive(Clone, Copy, PartialEq)]
enum PickKind {
    Resume,
    Model,
}
struct Picker {
    kind: PickKind,
    title: String,
    hint: String,
    items: Vec<(String, String, String)>,
    sel: usize,
}

enum Ev {
    Attempt(String, Option<String>),
    Failover(String, String),
    Items(Vec<Item>),
}
struct Chan {
    tx: mpsc::Sender<Ev>,
    cwd: PathBuf,
}
impl Display for Chan {
    fn status(&mut self, _text: &str) -> Result<()> {
        Ok(())
    }
    fn text(&mut self, text: &str) -> Result<()> {
        let _ = self.tx.send(Ev::Items(vec![Item::Text(text.into())]));
        Ok(())
    }
    fn attempt(&mut self, id: &str, model: Option<&str>, _resuming: bool) -> Result<()> {
        let _ = self
            .tx
            .send(Ev::Attempt(id.into(), model.map(str::to_owned)));
        Ok(())
    }
    fn failover(&mut self, id: &str, status: &str, _reason: &str) -> Result<()> {
        let _ = self.tx.send(Ev::Failover(id.into(), status.into()));
        Ok(())
    }
    fn event(&mut self, _id: &str, adapter: &str, raw: &Value, decoded: &Decoded) -> Result<()> {
        let items = view::interpret(adapter, raw, decoded, &self.cwd);
        if !items.is_empty() {
            let _ = self.tx.send(Ev::Items(items));
        }
        Ok(())
    }
}

struct RawMode {
    enhanced: bool,
}
impl RawMode {
    fn on() -> Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        // Only probe terminals known to speak the kitty keyboard protocol
        // (for shift+enter); others may never answer and would stall startup.
        let term = env::var("TERM").unwrap_or_default();
        let program = env::var("TERM_PROGRAM").unwrap_or_default();
        let capable = ["kitty", "ghostty", "foot", "alacritty"]
            .iter()
            .any(|t| term.contains(t))
            || ["ghostty", "WezTerm", "iTerm.app", "kitty"].contains(&program.as_str());
        let enhanced = capable
            && matches!(
                crossterm::terminal::supports_keyboard_enhancement(),
                Ok(true)
            );
        let mut o = io::stdout();
        if enhanced {
            crossterm::execute!(
                o,
                event::PushKeyboardEnhancementFlags(
                    event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                )
            )?;
        }
        write!(o, "\x1b[?2004h")?;
        o.flush()?;
        Ok(Self { enhanced })
    }
    fn off(&self) {
        let mut o = io::stdout();
        if self.enhanced {
            let _ = crossterm::execute!(o, event::PopKeyboardEnhancementFlags);
        }
        let _ = write!(o, "\x1b[?2004l\x1b[?25h");
        let _ = o.flush();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
impl Drop for RawMode {
    fn drop(&mut self) {
        self.off();
    }
}

// ───────────────────────── the application ─────────────────────────

struct App {
    ctx: Ctx,
    session: Option<Session>,
    blocks: Vec<Block>,
    printed: usize,
    cols: usize,
    rows: usize,
    cursor_row: usize,
    input: Editor,
    menu: Vec<MenuItem>,
    menu_sel: usize,
    menu_hidden: bool,
    picker: Option<Picker>,
    shortcuts: bool,
    live: Option<Live>,
    queue: Vec<String>,
    tick: usize,
    verb: usize,
    turn_start: Instant,
    exit_armed: Option<Instant>,
    esc_armed: Option<Instant>,
    notice: Option<(String, Instant)>,
    history: Vec<String>,
    hist_pos: Option<usize>,
    hist_draft: String,
    files: Option<Vec<String>>,
    custom: Vec<(String, String)>,
    cost: f64,
    tokens: (u64, u64),
    started: Instant,
    pending_bash: Vec<String>,
    placeholder: &'static str,
    quit: bool,
    raw: Option<RawMode>,
}

fn now_seed() -> usize {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
}

pub fn run(ctx: Ctx, session: Session, initial: Option<String>, picker: bool) -> Result<i32> {
    view::detect_color();
    let (cols, rows) = match crossterm::terminal::size() {
        Ok((c, r)) if c >= 20 && r >= 8 => (c, r),
        _ => (80, 24),
    };
    let cwd = PathBuf::from(session.meta["cwd"].as_str().unwrap_or("."));
    let seed = now_seed();
    let mut app = App {
        history: load_history(&ctx.root, &cwd),
        custom: custom_commands(&cwd),
        ctx,
        session: Some(session),
        blocks: vec![Block::Header],
        printed: 0,
        cols: cols as usize,
        rows: rows as usize,
        cursor_row: 0,
        input: Editor::default(),
        menu: vec![],
        menu_sel: 0,
        menu_hidden: false,
        picker: None,
        shortcuts: false,
        live: None,
        queue: vec![],
        tick: 0,
        verb: seed % view::VERBS.len(),
        turn_start: Instant::now(),
        exit_armed: None,
        esc_armed: None,
        notice: None,
        hist_pos: None,
        hist_draft: String::new(),
        files: None,
        cost: 0.0,
        tokens: (0, 0),
        started: Instant::now(),
        pending_bash: vec![],
        placeholder: PLACEHOLDERS[seed % PLACEHOLDERS.len()],
        quit: false,
        raw: None,
    };
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        print!("\x1b[?2004l\x1b[?25h\r\n");
        default_hook(info);
    }));
    app.raw = Some(RawMode::on()?);
    app.replay();
    if picker {
        app.resume_picker()?;
    }
    app.draw()?;
    if let Some(text) = initial.filter(|t| !t.trim().is_empty()) {
        app.submit(text)?;
    }
    let result = app.event_loop();
    app.finish_screen();
    app.raw.take();
    result?;
    Ok(if app.ctx.cancel.load(Ordering::Relaxed) {
        130
    } else {
        0
    })
}

impl App {
    fn session(&self) -> &Session {
        self.session.as_ref().expect("session")
    }
    fn session_mut(&mut self) -> &mut Session {
        self.session.as_mut().expect("session")
    }
    fn cwd(&self) -> PathBuf {
        PathBuf::from(self.session().meta["cwd"].as_str().unwrap_or("."))
    }
    fn enabled(&self) -> Vec<String> {
        self.ctx.config["order"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| self.ctx.config["providers"][*id]["enabled"] == true)
            .map(str::to_owned)
            .collect()
    }
    fn note(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), Instant::now()));
    }

    // ── rendering ──

    fn block_lines(&self, b: &Block, cols: usize) -> Vec<Line> {
        let dim = Style::fg(DIM);
        match b {
            Block::Header => {
                let m = view::mascot();
                let home = config::home();
                let cwd = self.cwd();
                let path = cwd
                    .strip_prefix(&home)
                    .map(|p| format!("~/{}", p.display()))
                    .unwrap_or_else(|_| cwd.display().to_string());
                let chain = self
                    .enabled()
                    .iter()
                    .map(
                        |id| match self.ctx.config["providers"][id]["model"].as_str() {
                            Some(m) => format!("{id} ({m})"),
                            None => id.clone(),
                        },
                    )
                    .collect::<Vec<_>>()
                    .join(" → ");
                let text = [
                    Line::of(Style::plain().bold(), "Lennon")
                        .push(dim, format!(" v{}", env!("CARGO_PKG_VERSION"))),
                    Line::of(dim, chain),
                    Line::of(dim, path),
                ];
                m.into_iter()
                    .zip(text)
                    .map(|(mut l, t)| {
                        l.add(Style::plain(), "  ");
                        l.0.extend(t.0);
                        l.truncate(cols)
                    })
                    .collect()
            }
            Block::User(t) => view::user(t, cols),
            Block::Text { text, first } => view::assistant(text, cols, *first),
            Block::Tool { tool, result } => {
                let dot = match result {
                    None => dim,
                    Some((_, true)) => Style::fg(ERROR),
                    Some((_, false)) => Style::fg(SUCCESS),
                };
                let mut l = view::tool_header(tool, dot, cols);
                if let Some((out, err)) = result {
                    l.extend(view::tool_body(tool, out, *err, self.ctx.verbose, cols));
                }
                l
            }
            Block::Quiet(c) => vec![Line::of(
                dim,
                format!("  {}", view::quiet_summary(*c, true)),
            )],
            Block::Out(rows) => view::result_rows(rows, cols),
            Block::Note(color, text) => view::wrap(
                &[(Style::fg(*color), text.clone())],
                cols,
                &Line::of(Style::fg(*color), "⏺ "),
                &Line::of(Style::plain(), "  "),
            ),
            Block::BashIn(cmd) => view::wrap(
                &[(Style::plain(), cmd.clone())],
                cols,
                &Line::of(Style::fg(BASH), "! "),
                &Line::of(Style::plain(), "  "),
            ),
            Block::Done(t) => vec![Line::of(dim, format!("✻ {t}"))],
        }
    }

    fn live_lines(&self) -> (Vec<Line>, usize, usize) {
        let cols = self.cols;
        let dim = Style::fg(DIM);
        let mut top: Vec<Line> = vec![];
        if let Some(live) = &self.live {
            if !live.text.trim().is_empty() {
                top.push(Line::new());
                top.extend(view::assistant(&live.text, cols, !live.text_started));
            }
            for p in &live.tools {
                top.push(Line::new());
                let dot = if self.tick / 5 % 2 == 0 {
                    Style::plain()
                } else {
                    dim
                };
                top.extend(view::tool_header(&p.tool, dot, cols));
                match &p.result {
                    Some((o, e)) => {
                        top.extend(view::tool_body(&p.tool, o, *e, self.ctx.verbose, cols))
                    }
                    None => top.push(Line::of(dim, "  ⎿  Running…")),
                }
            }
            if live.quiet_open {
                top.push(Line::new());
                top.push(Line::of(
                    dim,
                    format!("  {}", view::quiet_summary(live.quiet, false)),
                ));
            }
            top.push(Line::new());
            let (verb, _) = view::VERBS[self.verb];
            let mut s = Line::of(
                Style::fg(ACCENT),
                format!("{} ", view::spinner_frame(self.tick)),
            );
            s.0.extend(view::shimmer(&format!("{verb}…"), self.tick).0);
            let secs = self.turn_start.elapsed().as_secs();
            if secs >= 3 {
                let mut extra = format!(
                    " ({}",
                    view::duration(self.turn_start.elapsed().as_millis())
                );
                if live.output_tokens > 0 {
                    extra.push_str(&format!(" · ↓ {} tokens", view::tokens(live.output_tokens)));
                }
                extra.push(')');
                s.add(dim, extra);
            }
            top.push(s.truncate(cols));
            for q in &self.queue {
                for l in view::wrap(
                    &[(dim, q.clone())],
                    cols,
                    &Line::of(dim, "  ❯ "),
                    &Line::of(dim, "    "),
                ) {
                    top.push(l);
                }
            }
        }
        let mut bottom: Vec<Line> = vec![];
        let mut cursor = (0, 0);
        if let Some(p) = &self.picker {
            let accent = Style::fg(view::CODE);
            bottom.push(Line::of(Style::fg(RULE), "─".repeat(cols)));
            bottom.push(Line::of(Style::plain().bold(), format!(" {}", p.title)));
            bottom.push(Line::new());
            let start = p.sel.saturating_sub(7);
            for (i, (label, detail, _)) in p.items.iter().enumerate().skip(start).take(8) {
                let sel = i == p.sel;
                let mut l = Line::of(
                    if sel { accent } else { Style::plain() },
                    format!(" {} {}. {label}", if sel { "❯" } else { " " }, i + 1),
                );
                if !detail.is_empty() {
                    l.add(dim, format!("  {detail}"));
                }
                bottom.push(l.truncate(cols));
            }
            bottom.push(Line::new());
            bottom.push(Line::of(dim, format!(" {}", p.hint)).truncate(cols));
            return (cap(top, bottom, self.rows), usize::MAX, 0);
        }
        // Suggestions menu, shown above the prompt like Claude Code.
        if !self.menu.is_empty() && !self.menu_hidden {
            let start = self.menu_sel.saturating_sub(4);
            let name_w = self
                .menu
                .iter()
                .map(|m| view::width(&m.label))
                .max()
                .unwrap_or(0)
                .clamp(10, 40)
                + 4;
            for (i, m) in self.menu.iter().enumerate().skip(start).take(5) {
                let sel = i == self.menu_sel;
                let style = if sel {
                    Style::fg(view::CODE)
                } else {
                    Style::plain()
                };
                let mut l = Line::of(
                    style,
                    format!("  {} {}", if sel { "❯" } else { " " }, m.label),
                );
                let pad = (name_w + 4).saturating_sub(l.width());
                l.add(Style::plain(), " ".repeat(pad));
                l.add(if sel { style } else { dim }, m.detail.clone());
                bottom.push(l.truncate(cols));
            }
        } else {
            // Right-aligned provider chain, in place of Claude Code's effort hint.
            let active = self
                .live
                .as_ref()
                .and_then(|l| l.provider.clone())
                .or_else(|| self.enabled().into_iter().next());
            let mut chain = Line::new();
            for (i, id) in self.enabled().iter().enumerate() {
                if i > 0 {
                    chain.add(dim, " → ");
                }
                chain.add(
                    if Some(id) == active.as_ref() {
                        Style::plain()
                    } else {
                        dim
                    },
                    id.clone(),
                );
            }
            let w = chain.width();
            if w + 2 < cols {
                let mut l = Line::of(Style::plain(), " ".repeat(cols - w - 1));
                l.0.extend(chain.0);
                bottom.push(l);
            }
        }
        let rule_color = if self.input.bash { BASH } else { RULE };
        bottom.push(Line::of(Style::fg(rule_color), "─".repeat(cols)));
        // Prompt rows with wrapping and cursor tracking.
        let prompt = if self.input.bash {
            Line::of(Style::fg(BASH), "! ")
        } else {
            Line::of(Style::fg(DIM), "❯ ")
        };
        let inner = cols.saturating_sub(3).max(10);
        let first_input_row = bottom.len();
        if self.input.buf.is_empty() {
            let hint = if self.input.bash {
                ""
            } else if self.live.is_some() {
                "Press up to edit queued messages"
            } else {
                self.placeholder
            };
            let mut l = prompt.clone();
            l.add(
                dim,
                if self.live.is_some() && self.queue.is_empty() {
                    ""
                } else {
                    hint
                },
            );
            bottom.push(l.truncate(cols));
            cursor = (first_input_row, 2);
        } else {
            let mut row = 0;
            let mut col = 0;
            let mut current = String::new();
            let mut rows_out: Vec<String> = vec![];
            for (i, c) in self.input.buf.char_indices() {
                if i == self.input.cur {
                    cursor = (row, col);
                }
                if c == '\n' {
                    rows_out.push(std::mem::take(&mut current));
                    row += 1;
                    col = 0;
                    continue;
                }
                let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                if col + w > inner {
                    rows_out.push(std::mem::take(&mut current));
                    row += 1;
                    col = 0;
                }
                current.push(c);
                col += w;
            }
            if self.input.cur >= self.input.buf.len() {
                if col >= inner {
                    rows_out.push(std::mem::take(&mut current));
                    row += 1;
                    col = 0;
                }
                cursor = (row, col);
            }
            rows_out.push(current);
            for (i, r) in rows_out.into_iter().enumerate() {
                let mut l = if i == 0 {
                    prompt.clone()
                } else {
                    Line::of(Style::plain(), "  ")
                };
                l.add(Style::plain(), r);
                bottom.push(l);
            }
            cursor = (first_input_row + cursor.0, 2 + cursor.1);
        }
        bottom.push(Line::of(Style::fg(rule_color), "─".repeat(cols)));
        bottom.extend(self.footer());
        let blen = bottom.len();
        let lines = cap(top, bottom, self.rows);
        let offset = lines.len() - blen;
        (lines, offset + cursor.0, cursor.1)
    }

    fn footer(&self) -> Vec<Line> {
        let dim = Style::fg(DIM);
        if self
            .exit_armed
            .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
        {
            return vec![Line::of(dim, "  Press Ctrl-C again to exit")];
        }
        if self
            .esc_armed
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return vec![Line::of(dim, "  Esc again to clear")];
        }
        if let Some((n, t)) = &self.notice {
            if t.elapsed() < Duration::from_secs(3) {
                return vec![Line::of(dim, format!("  {n}")).truncate(self.cols)];
            }
        }
        if self.shortcuts {
            let cells = [
                [
                    "! for shell mode",
                    "double tap esc to clear input",
                    "ctrl + z to suspend",
                ],
                [
                    "/ for commands",
                    "shift + tab to cycle modes",
                    "ctrl + g to edit in $EDITOR",
                ],
                [
                    "@ for file paths",
                    "ctrl + o for verbose output",
                    "ctrl + l to redraw",
                ],
                ["", "shift + ⏎ for newline", "ctrl + c to interrupt"],
            ];
            return cells
                .iter()
                .map(|r| {
                    Line::of(dim, format!("  {:<24}{:<35}{}", r[0], r[1], r[2])).truncate(self.cols)
                })
                .collect();
        }
        if self.input.bash {
            return vec![Line::of(Style::fg(BASH), "  ! for shell mode")];
        }
        let running = self.live.is_some();
        let (glyph, color) = match self.ctx.opts.mode {
            Mode::Manual => ("⏸ manual mode on", DIM),
            Mode::AcceptEdits => ("⏵⏵ accept edits on", EDITS),
            Mode::Plan => ("⏸ plan mode on", PLAN),
            Mode::Auto => ("⏵⏵ auto mode on", WARNING),
        };
        let mut l = Line::of(Style::fg(color), format!("  {glyph}"));
        if self.ctx.opts.mode != Mode::Manual {
            l.add(dim, " (shift+tab to cycle)");
        }
        l.add(
            dim,
            if running {
                " · esc to interrupt"
            } else if self.ctx.opts.mode == Mode::Manual {
                " · ? for shortcuts"
            } else {
                ""
            },
        );
        if self.ctx.verbose {
            l.add(dim, " · verbose");
        }
        vec![l.truncate(self.cols)]
    }

    fn draw(&mut self) -> Result<()> {
        let mut out = String::from("\x1b[?2026h\x1b[?25l");
        if self.cursor_row > 0 {
            out.push_str(&format!("\x1b[{}A", self.cursor_row));
        }
        out.push_str("\r\x1b[J");
        let cols = self.cols;
        while self.printed < self.blocks.len() {
            let b = self.blocks[self.printed].clone();
            // Results attach directly to the command or prompt above them.
            if self.printed > 0 && !matches!(b, Block::Out(_)) {
                out.push_str("\r\n");
            }
            for l in self.block_lines(&b, cols) {
                out.push_str(&l.render());
                out.push_str("\r\n");
            }
            self.printed += 1;
        }
        let (lines, crow, ccol) = self.live_lines();
        for (i, l) in lines.iter().enumerate() {
            out.push_str(&l.render());
            if i + 1 < lines.len() {
                out.push_str("\r\n");
            }
        }
        let last = lines.len().saturating_sub(1);
        let (crow, show) = if crow == usize::MAX {
            (last, false)
        } else {
            (crow.min(last), true)
        };
        if last > crow {
            out.push_str(&format!("\x1b[{}A", last - crow));
        }
        out.push('\r');
        if ccol > 0 {
            out.push_str(&format!("\x1b[{ccol}C"));
        }
        if show {
            out.push_str("\x1b[?25h");
        }
        out.push_str("\x1b[?2026l");
        self.cursor_row = crow;
        let mut o = io::stdout().lock();
        o.write_all(out.as_bytes())?;
        o.flush()?;
        Ok(())
    }

    /// Clear the screen and scrollback, then print the transcript again.
    fn redraw_all(&mut self) -> Result<()> {
        let mut o = io::stdout();
        write!(o, "\x1b[2J\x1b[3J\x1b[H")?;
        o.flush()?;
        self.printed = 0;
        self.cursor_row = 0;
        self.draw()
    }

    fn finish_screen(&mut self) {
        let mut o = io::stdout();
        if self.cursor_row > 0 {
            let _ = write!(o, "\x1b[{}A", self.cursor_row);
        }
        let _ = write!(o, "\r\x1b[J");
        let _ = o.flush();
    }

    // ── transcript replay ──

    fn replay(&mut self) {
        let events = self.session().events.clone();
        let cwd = self.cwd();
        let mut live = Live::default();
        let mut out = vec![];
        for e in &events {
            match e["type"].as_str().unwrap_or("") {
                "user" => {
                    live.finish(false, &mut out);
                    out.push(Block::User(e["text"].as_str().unwrap_or("").into()));
                }
                "provider_event" => {
                    let adapter = e["adapter"]
                        .as_str()
                        .or_else(|| e["provider"].as_str())
                        .unwrap_or("");
                    let decoded = crate::provider::decode(adapter, &e["event"]);
                    for item in view::interpret(adapter, &e["event"], &decoded, &cwd) {
                        live.feed(item, false, &mut out);
                    }
                }
                "attempt_end" if e["status"] != "success" => {
                    live.finish(true, &mut out);
                    out.push(Block::Note(
                        WARNING,
                        format!(
                            "{} stopped: {}",
                            e["provider"].as_str().unwrap_or("provider"),
                            e["status"].as_str().unwrap_or("error")
                        ),
                    ));
                }
                "complete" => live.finish(false, &mut out),
                _ => {}
            }
        }
        live.finish(false, &mut out);
        self.blocks.extend(out);
        if self.session().meta["pending"] == true {
            self.blocks.push(Block::Note(
                WARNING,
                "This session has unfinished work · /retry to continue".into(),
            ));
        }
    }

    // ── main loop ──

    fn event_loop(&mut self) -> Result<()> {
        while !self.quit {
            if self.ctx.cancel.load(Ordering::Relaxed) {
                break;
            }
            if event::poll(Duration::from_millis(100))? {
                let ev = event::read()?;
                self.handle(ev, None)?;
                while !self.quit && event::poll(Duration::ZERO)? {
                    let ev = event::read()?;
                    self.handle(ev, None)?;
                }
            }
            self.draw()?;
        }
        Ok(())
    }

    fn handle(&mut self, ev: Event, stop: Option<&AtomicBool>) -> Result<()> {
        match ev {
            Event::Resize(c, r) => {
                self.cols = (c as usize).max(20);
                self.rows = (r as usize).max(8);
                self.redraw_all()?;
            }
            Event::Paste(text) => {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                if text.lines().count() > 10 || text.len() > 2000 {
                    let tag = paste_tag(self.input.pastes.len(), &text);
                    self.input.pastes.push(text);
                    self.input.insert(&tag);
                } else {
                    self.input.insert(&text);
                }
                self.refresh_menu();
            }
            Event::Key(k) if k.kind != KeyEventKind::Release => self.key(k, stop)?,
            _ => {}
        }
        Ok(())
    }

    fn key(&mut self, k: KeyEvent, stop: Option<&AtomicBool>) -> Result<()> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        if let Some(p) = &mut self.picker {
            match k.code {
                KeyCode::Up => p.sel = p.sel.saturating_sub(1),
                KeyCode::Down => p.sel = (p.sel + 1).min(p.items.len().saturating_sub(1)),
                KeyCode::Esc => self.picker = None,
                KeyCode::Char('c') if ctrl => self.picker = None,
                KeyCode::Enter => {
                    let p = self.picker.take().unwrap();
                    if let Some(item) = p.items.get(p.sel) {
                        self.picked(p.kind, item.2.clone())?;
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    let i = c as usize - '1' as usize;
                    if i < p.items.len() {
                        let p = self.picker.take().unwrap();
                        self.picked(p.kind, p.items[i].2.clone())?;
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        if self.exit_armed.is_some() && !matches!(k.code, KeyCode::Char('c') | KeyCode::Char('d')) {
            self.exit_armed = None;
        }
        let menu_open = !self.menu.is_empty() && !self.menu_hidden;
        match k.code {
            KeyCode::Char('c') if ctrl => {
                if let Some(stop) = stop {
                    stop.store(true, Ordering::Relaxed);
                } else if !self.input.buf.is_empty() || self.input.bash {
                    self.input.clear();
                } else if self
                    .exit_armed
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
                {
                    self.quit = true;
                } else {
                    self.exit_armed = Some(Instant::now());
                }
            }
            KeyCode::Char('d') if ctrl => {
                if !self.input.buf.is_empty() {
                    self.input.delete();
                } else if stop.is_none() {
                    if self
                        .exit_armed
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(2))
                    {
                        self.quit = true;
                    } else {
                        self.exit_armed = Some(Instant::now());
                    }
                }
            }
            KeyCode::Esc => {
                if menu_open {
                    self.menu_hidden = true;
                } else if self.shortcuts {
                    self.shortcuts = false;
                } else if let Some(stop) = stop {
                    stop.store(true, Ordering::Relaxed);
                } else if !self.input.buf.is_empty() {
                    if self
                        .esc_armed
                        .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
                    {
                        self.input.clear();
                        self.esc_armed = None;
                    } else {
                        self.esc_armed = Some(Instant::now());
                    }
                } else if self.input.bash {
                    self.input.bash = false;
                }
            }
            KeyCode::BackTab => {
                self.ctx.opts.mode = self.ctx.opts.mode.next();
            }
            KeyCode::Tab if shift => {
                self.ctx.opts.mode = self.ctx.opts.mode.next();
            }
            KeyCode::Tab => {
                if menu_open {
                    self.accept_menu(false)?;
                }
            }
            KeyCode::Enter if shift || alt => self.input.insert("\n"),
            KeyCode::Char('j') if ctrl => self.input.insert("\n"),
            KeyCode::Enter => {
                if menu_open {
                    return self.accept_menu(true);
                }
                if self.input.buf[..self.input.cur].ends_with('\\') {
                    self.input.backspace();
                    self.input.insert("\n");
                } else {
                    let bash = self.input.bash;
                    let text = self.input.take();
                    self.menu.clear();
                    self.hist_pos = None;
                    if bash {
                        if stop.is_some() {
                            self.note("Shell commands can run when Lennon is idle");
                        } else if !text.trim().is_empty() {
                            self.run_bash(text)?;
                        }
                    } else if !text.trim().is_empty() {
                        if stop.is_some() {
                            self.queue.push(text);
                        } else {
                            self.submit(text)?;
                        }
                    }
                }
            }
            KeyCode::Up => {
                if menu_open {
                    self.menu_sel = self.menu_sel.checked_sub(1).unwrap_or(self.menu.len() - 1);
                } else if stop.is_some() && self.input.buf.is_empty() && !self.queue.is_empty() {
                    let q = self.queue.join("\n");
                    self.queue.clear();
                    self.input.set(&q);
                } else if !self.input.vertical(true) {
                    self.history_step(true);
                }
            }
            KeyCode::Down => {
                if menu_open {
                    self.menu_sel = (self.menu_sel + 1) % self.menu.len();
                } else if !self.input.vertical(false) {
                    self.history_step(false);
                }
            }
            KeyCode::Left if ctrl || alt => self.input.cur = self.input.word_left(),
            KeyCode::Right if ctrl || alt => self.input.cur = self.input.word_right(),
            KeyCode::Left => self.input.cur = self.input.prev(),
            KeyCode::Right => self.input.cur = self.input.next(),
            KeyCode::Home => self.input.cur = self.input.line_start(),
            KeyCode::End => self.input.cur = self.input.line_end(),
            KeyCode::Backspace if alt || ctrl => {
                let w = self.input.word_left();
                let cur = self.input.cur;
                self.input.cut(w, cur);
            }
            KeyCode::Backspace => {
                if self.input.buf.is_empty() && self.input.bash {
                    self.input.bash = false;
                } else {
                    self.input.backspace();
                }
            }
            KeyCode::Delete => self.input.delete(),
            KeyCode::Char(c) if ctrl => match c {
                'a' => self.input.cur = self.input.line_start(),
                'e' => self.input.cur = self.input.line_end(),
                'b' => self.input.cur = self.input.prev(),
                'f' => self.input.cur = self.input.next(),
                'k' => {
                    let (a, b) = (self.input.cur, self.input.line_end());
                    self.input.cut(a, b);
                }
                'u' => {
                    let (a, b) = (self.input.line_start(), self.input.cur);
                    self.input.cut(a, b);
                }
                'w' => {
                    let (a, b) = (self.input.word_left(), self.input.cur);
                    self.input.cut(a, b);
                }
                'y' => {
                    let k = self.input.kill.clone();
                    self.input.insert(&k);
                }
                'p' => {
                    if !self.input.vertical(true) {
                        self.history_step(true);
                    }
                }
                'n' => {
                    if !self.input.vertical(false) {
                        self.history_step(false);
                    }
                }
                'l' => self.redraw_all()?,
                'o' => {
                    self.ctx.verbose = !self.ctx.verbose;
                    let state = if self.ctx.verbose { "on" } else { "off" };
                    self.note(format!("Verbose output {state}"));
                }
                'g' => self.external_editor()?,
                'z' => self.suspend()?,
                _ => {}
            },
            KeyCode::Char(c) if alt => match c {
                'b' => self.input.cur = self.input.word_left(),
                'f' => self.input.cur = self.input.word_right(),
                'd' => {
                    let (a, b) = (self.input.cur, self.input.word_right());
                    self.input.cut(a, b);
                }
                _ => {}
            },
            KeyCode::Char('?') if self.input.buf.is_empty() && !self.input.bash => {
                self.shortcuts = !self.shortcuts;
                return Ok(());
            }
            KeyCode::Char('!') if self.input.buf.is_empty() && !self.input.bash => {
                self.input.bash = true;
            }
            KeyCode::Char(c) => {
                self.input.insert(c.encode_utf8(&mut [0; 4]));
                self.menu_hidden = false;
            }
            _ => {}
        }
        if !matches!(k.code, KeyCode::Char('?')) {
            self.shortcuts = false;
        }
        if !matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::Esc) {
            self.refresh_menu();
        }
        Ok(())
    }

    fn history_step(&mut self, back: bool) {
        if self.history.is_empty() {
            return;
        }
        let pos = match (self.hist_pos, back) {
            (None, true) => {
                self.hist_draft = self.input.buf.clone();
                Some(self.history.len() - 1)
            }
            (None, false) => None,
            (Some(p), true) => Some(p.saturating_sub(1)),
            (Some(p), false) if p + 1 < self.history.len() => Some(p + 1),
            (Some(_), false) => {
                let d = std::mem::take(&mut self.hist_draft);
                self.input.set(&d);
                self.hist_pos = None;
                return;
            }
        };
        if let Some(p) = pos {
            let text = self.history[p].clone();
            self.input.set(&text);
        }
        self.hist_pos = pos;
    }

    fn refresh_menu(&mut self) {
        self.menu.clear();
        let before = &self.input.buf[..self.input.cur];
        if self.input.bash {
            return;
        }
        if before.starts_with('/') && !before.contains(char::is_whitespace) {
            let q = before[1..].to_lowercase();
            let mut all: Vec<(String, String)> = BUILTINS
                .iter()
                .map(|(n, d)| (n.to_string(), d.to_string()))
                .collect();
            all.extend(self.custom.iter().cloned());
            let mut hits: Vec<(usize, MenuItem)> = all
                .into_iter()
                .filter_map(|(name, desc)| {
                    let n = name.to_lowercase();
                    let rank = if n.starts_with(&q) {
                        0
                    } else if n.contains(&q) || desc.to_lowercase().contains(&q) && q.len() > 2 {
                        1
                    } else {
                        return None;
                    };
                    Some((
                        rank,
                        MenuItem {
                            insert: format!("/{name}"),
                            label: format!("/{name}"),
                            detail: desc,
                        },
                    ))
                })
                .collect();
            hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.label.len().cmp(&b.1.label.len())));
            self.menu = hits.into_iter().map(|h| h.1).collect();
        } else if let Some(at) = before.rfind('@') {
            let token = &before[at + 1..];
            let boundary = at == 0 || before[..at].ends_with(char::is_whitespace);
            if boundary && !token.contains(char::is_whitespace) {
                let cwd = self.cwd();
                let files = self.files.get_or_insert_with(|| list_files(&cwd));
                let q = token.to_lowercase();
                let mut hits: Vec<(usize, &String)> = files
                    .iter()
                    .filter_map(|f| {
                        let l = f.to_lowercase();
                        let base = l.trim_end_matches('/').rsplit('/').next().unwrap_or(&l);
                        if base.starts_with(&q) {
                            Some((0, f))
                        } else if l.contains(&q) {
                            Some((1, f))
                        } else {
                            None
                        }
                    })
                    .collect();
                hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len())));
                self.menu = hits
                    .into_iter()
                    .take(50)
                    .map(|(_, f)| MenuItem {
                        insert: f.clone(),
                        label: format!("+ {f}"),
                        detail: String::new(),
                    })
                    .collect();
            }
        }
        if self.menu_sel >= self.menu.len() {
            self.menu_sel = 0;
        }
    }

    fn accept_menu(&mut self, run: bool) -> Result<()> {
        let Some(item) = self.menu.get(self.menu_sel) else {
            return Ok(());
        };
        let insert = item.insert.clone();
        if insert.starts_with('/') {
            self.input.set(&insert);
            self.menu.clear();
            if run {
                let text = self.input.take();
                return self.submit(text);
            }
            self.input.insert(" ");
        } else {
            let before = &self.input.buf[..self.input.cur];
            let at = before.rfind('@').unwrap_or(0);
            let cur = self.input.cur;
            self.input
                .buf
                .replace_range(at..cur, &format!("@{insert} "));
            self.input.cur = at + insert.len() + 2;
            self.menu.clear();
        }
        Ok(())
    }

    // ── submitting ──

    fn submit(&mut self, text: String) -> Result<()> {
        let trimmed = text.trim().to_string();
        save_history(&self.ctx.root, &self.cwd(), &trimmed);
        self.history.push(trimmed.clone());
        if let Some(cmd) = trimmed.strip_prefix('/') {
            let (name, args) = cmd.split_once(char::is_whitespace).unwrap_or((cmd, ""));
            let name = ALIASES
                .iter()
                .find(|(a, _)| *a == name)
                .map(|(_, n)| *n)
                .unwrap_or(name);
            if BUILTINS.iter().any(|(n, _)| *n == name) {
                return self.command(name, args.trim(), &trimmed);
            }
            if !self.custom.iter().any(|(n, _)| n == name) {
                self.blocks.push(Block::User(trimmed.clone()));
                self.blocks.push(Block::Out(vec![(
                    Style::fg(DIM),
                    format!("Unknown slash command: {name}"),
                )]));
                return Ok(());
            }
        }
        let mut prompt = trimmed.clone();
        if !self.pending_bash.is_empty() {
            prompt = format!("{}\n\n{prompt}", self.pending_bash.join("\n"));
            self.pending_bash.clear();
        }
        self.blocks.push(Block::User(trimmed));
        self.turn(Some(prompt))
    }

    fn turn(&mut self, prompt: Option<String>) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let mut session = self.session.take().expect("session");
        let cwd = PathBuf::from(session.meta["cwd"].as_str().unwrap_or("."));
        let config = self.ctx.config.clone();
        let opts = self.ctx.opts.clone();
        let timeout = self.ctx.timeout;
        let flag = stop.clone();
        let handle = thread::spawn(move || {
            let mut chan = Chan { tx, cwd };
            let r = harness::turn(
                &mut session,
                prompt.as_deref(),
                &config,
                &opts,
                timeout,
                &flag,
                &mut chan,
            );
            (session, r)
        });
        self.live = Some(Live::default());
        self.turn_start = Instant::now();
        self.verb = (self.verb + 1 + now_seed() % 7) % view::VERBS.len();
        let mut last_tick = Instant::now();
        loop {
            if self.ctx.cancel.load(Ordering::Relaxed) {
                stop.store(true, Ordering::Relaxed);
            }
            let mut changed = false;
            while let Ok(ev) = rx.try_recv() {
                self.apply(ev);
                changed = true;
            }
            if handle.is_finished() {
                while let Ok(ev) = rx.try_recv() {
                    self.apply(ev);
                }
                break;
            }
            if event::poll(Duration::from_millis(40))? {
                let ev = event::read()?;
                self.handle(ev, Some(&stop))?;
                changed = true;
            }
            if last_tick.elapsed() >= Duration::from_millis(120) {
                self.tick += 1;
                last_tick = Instant::now();
                changed = true;
            }
            if changed {
                self.draw()?;
            }
        }
        let (session, result) = handle
            .join()
            .map_err(|_| error("Provider thread panicked"))?;
        self.session = Some(session);
        let mut live = self.live.take().unwrap_or_default();
        let elapsed = self.turn_start.elapsed().as_millis();
        let mut out = vec![];
        match result {
            Ok(outcome) => {
                let interrupted = outcome.status == "cancelled";
                live.finish(interrupted, &mut out);
                if let Some((id, status)) = live.failover.take() {
                    out.push(Block::Note(WARNING, format!("{id} {}", describe(&status))));
                }
                self.blocks.extend(out);
                self.outcome(&outcome, elapsed);
            }
            Err(e) => {
                live.finish(true, &mut out);
                self.blocks.extend(out);
                self.blocks.push(Block::Note(ERROR, format!("Error: {e}")));
            }
        }
        self.draw()?;
        if self.ctx.cancel.load(Ordering::Relaxed) {
            self.quit = true;
            return Ok(());
        }
        if !self.queue.is_empty() {
            let next = std::mem::take(&mut self.queue).join("\n");
            return self.submit(next);
        }
        Ok(())
    }

    fn apply(&mut self, ev: Ev) {
        let verbose = self.ctx.verbose;
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let mut out = vec![];
        match ev {
            Ev::Attempt(id, model) => {
                if let Some((old, status)) = live.failover.take() {
                    live.finish(true, &mut out);
                    out.push(Block::Note(
                        WARNING,
                        format!(
                            "{old} {} · continuing with {id}{}",
                            describe(&status),
                            model.map(|m| format!(" ({m})")).unwrap_or_default()
                        ),
                    ));
                }
                live.provider = Some(id);
            }
            Ev::Failover(id, status) => live.failover = Some((id, status)),
            Ev::Items(items) => {
                for item in items {
                    if let Item::Usage {
                        cost,
                        input,
                        output,
                    } = item
                    {
                        self.cost += cost;
                        self.tokens.0 += input;
                        self.tokens.1 += output;
                    }
                    live.feed(item, verbose, &mut out);
                }
            }
        }
        self.blocks.extend(out);
    }

    fn outcome(&mut self, o: &Outcome, elapsed: u128) {
        let (_, past) = view::VERBS[self.verb];
        let msg = |e: &Option<Value>| {
            e.as_ref()
                .map(|e| {
                    e.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| e.to_string())
                })
                .unwrap_or_default()
        };
        match o.status.as_str() {
            "success" => self.blocks.push(Block::Done(format!(
                "{past} for {}",
                view::duration(elapsed)
            ))),
            "cancelled" => self.blocks.push(Block::Out(vec![(
                Style::fg(ERROR),
                "Interrupted · What should Lennon do instead?".into(),
            )])),
            "exhausted" => {
                self.blocks.push(Block::Note(
                    ERROR,
                    "Every provider is unavailable right now. Your work is saved · /retry when a limit resets".into(),
                ));
                let rows = msg(&o.error)
                    .lines()
                    .map(|l| (Style::fg(DIM), l.to_string()))
                    .collect::<Vec<_>>();
                if !rows.is_empty() {
                    self.blocks.push(Block::Out(rows));
                }
            }
            status => {
                let text = msg(&o.error);
                self.blocks.push(Block::Note(
                    ERROR,
                    format!(
                        "{} error: {}",
                        o.provider.clone().unwrap_or_else(|| status.into()),
                        cli::safe_text(text.lines().next().unwrap_or(status))
                    ),
                ));
            }
        }
    }

    // ── slash commands ──

    fn out(&mut self, input: &str, rows: Vec<String>) {
        self.blocks.push(Block::User(input.into()));
        self.blocks.push(Block::Out(
            rows.into_iter().map(|r| (Style::plain(), r)).collect(),
        ));
    }

    fn command(&mut self, name: &str, args: &str, input: &str) -> Result<()> {
        match name {
            "exit" => self.quit = true,
            "help" => {
                let mut rows = vec![
                    format!(
                        "Lennon v{} · Claude Code, with automatic provider handoff",
                        env!("CARGO_PKG_VERSION")
                    ),
                    String::new(),
                    "Commands".into(),
                ];
                for (n, d) in BUILTINS {
                    rows.push(format!("  /{n:<11} {d}"));
                }
                for (n, d) in &self.custom {
                    rows.push(format!("  /{n:<11} {d}"));
                }
                rows.extend([
                    String::new(),
                    "Shortcuts".into(),
                    "  ! shell mode · / commands · @ file paths · ? shortcuts".into(),
                    "  shift+tab cycle permission modes · esc interrupt · ctrl+o verbose".into(),
                    "  shift+enter or \\⏎ newline · ctrl+g edit in $EDITOR · ctrl+c twice exit"
                        .into(),
                ]);
                self.out(input, rows);
            }
            "status" => {
                let rows = self.status_rows();
                self.out(input, rows);
            }
            "providers" => {
                let rows = self.provider_rows();
                self.out(input, rows);
            }
            "config" => {
                let mut rows = vec![format!("Config: {}", self.ctx.config_path.display())];
                rows.extend(
                    serde_json::to_string_pretty(&self.ctx.config)?
                        .lines()
                        .map(str::to_owned),
                );
                rows.push("Edit with: lennon config set KEY VALUE".into());
                self.out(input, rows);
            }
            "cost" => {
                let rows = vec![
                    format!(
                        "Total cost:            ${:.4} (reported by Claude; other providers bill by subscription)",
                        self.cost
                    ),
                    format!(
                        "Total duration (wall): {}",
                        view::duration(self.started.elapsed().as_millis())
                    ),
                    format!(
                        "Usage:                 {} input, {} output tokens",
                        view::tokens(self.tokens.0),
                        view::tokens(self.tokens.1)
                    ),
                ];
                self.out(input, rows);
            }
            "doctor" => {
                let (lines, _) = cli::doctor_lines(&self.ctx.config, &self.ctx.cancel)?;
                self.out(input, lines);
            }
            "clear" => {
                let cwd = self.cwd();
                let mut s = Session::create(&self.ctx.root, &cwd)?;
                s.acquire()?;
                self.session = Some(s);
                self.blocks = vec![Block::Header];
                self.cost = 0.0;
                self.tokens = (0, 0);
                self.redraw_all()?;
            }
            "retry" => {
                self.session_mut().meta["cooldowns"] = json!({});
                self.session_mut().save()?;
                if self.session().meta["pending"] == true {
                    self.blocks.push(Block::User("/retry".into()));
                    return self.turn(None);
                }
                self.out(
                    input,
                    vec!["Provider cooldowns cleared. No unfinished task.".into()],
                );
            }
            "init" => {
                self.blocks.push(Block::User("/init".into()));
                return self.turn(Some(INIT_PROMPT.into()));
            }
            "memory" => {
                let path = self.cwd().join("CLAUDE.md");
                self.edit_file(&path)?;
                self.out(
                    input,
                    vec![format!("Opened memory file at {}", path.display())],
                );
            }
            "model" => {
                if args.is_empty() {
                    self.model_picker();
                } else {
                    let msg = self.set_model(args)?;
                    self.out(input, vec![msg]);
                }
            }
            "resume" => {
                if args.is_empty() {
                    self.resume_picker()?;
                } else {
                    self.open_session(args)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn status_rows(&self) -> Vec<String> {
        let s = self.session();
        let mut rows = vec![
            format!("Version:         {}", env!("CARGO_PKG_VERSION")),
            format!("Session ID:      {}", s.meta["id"].as_str().unwrap_or("")),
            format!("cwd:             {}", s.meta["cwd"].as_str().unwrap_or("")),
            format!("Permission mode: {}", self.ctx.opts.mode.name()),
            format!("Config:          {}", self.ctx.config_path.display()),
            format!(
                "Unfinished work: {}",
                if s.meta["pending"] == true {
                    "yes"
                } else {
                    "no"
                }
            ),
            String::new(),
        ];
        rows.extend(self.provider_rows());
        rows
    }
    fn provider_rows(&self) -> Vec<String> {
        let s = self.session();
        let mut rows = vec!["Providers (in fallback order)".to_string()];
        for (i, id) in self.enabled().iter().enumerate() {
            let p = &self.ctx.config["providers"][id];
            let until = s.meta["cooldowns"][id].as_u64().unwrap_or(0);
            let now = store::millis();
            let state = if until > now {
                format!(
                    "cooling down · {} left",
                    view::duration((until - now) as u128)
                )
            } else if s.meta["providers"][id]["id"].is_string() {
                "ready · native session attached".into()
            } else {
                "ready".into()
            };
            rows.push(format!(
                "  {}. {id} · {} · {} · {state}",
                i + 1,
                p["adapter"].as_str().unwrap_or(id),
                p["model"].as_str().unwrap_or("default model")
            ));
        }
        rows
    }

    fn set_model(&mut self, args: &str) -> Result<String> {
        let (id, model) = match args.split_once('=') {
            Some((id, m)) => (id.trim().to_string(), m.trim().to_string()),
            None => (
                self.enabled()
                    .into_iter()
                    .next()
                    .ok_or_else(|| error("No enabled provider"))?,
                args.trim().to_string(),
            ),
        };
        if self.ctx.config["providers"].get(&id).is_none() {
            return Ok(format!("Unknown provider: {id}"));
        }
        let entry = self.ctx.config["providers"][&id].as_object_mut().unwrap();
        if model.is_empty() || model == "default" {
            entry.remove("model");
            Ok(format!("Set model for {id} to its native default"))
        } else {
            entry.insert("model".into(), json!(model));
            Ok(format!("Set model for {id} to {model}"))
        }
    }

    fn model_picker(&mut self) {
        let Some(id) = self.enabled().into_iter().next() else {
            return;
        };
        let adapter = self.ctx.config["providers"][&id]["adapter"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let current = self.ctx.config["providers"][&id]["model"]
            .as_str()
            .map(str::to_owned);
        let mut items = vec![(
            "Default (recommended)".to_string(),
            "Use the provider's own default".to_string(),
            "default".to_string(),
        )];
        if adapter == "claude" {
            for (m, d) in [
                ("fable", "Fable"),
                ("opus", "Opus"),
                ("sonnet", "Sonnet"),
                ("haiku", "Haiku"),
            ] {
                items.push((d.into(), format!("--model {m}"), m.into()));
            }
        }
        if let Some(c) = &current {
            if !items.iter().any(|i| &i.2 == c) {
                items.push((c.clone(), "current".into(), c.clone()));
            }
        }
        let sel = current
            .as_ref()
            .and_then(|c| items.iter().position(|i| &i.2 == c))
            .unwrap_or(0);
        self.picker = Some(Picker {
            kind: PickKind::Model,
            title: format!("Select model for {id}"),
            hint:
                "Enter to confirm · Esc to exit · /model NAME or /model PROVIDER=NAME for any model"
                    .into(),
            items,
            sel,
        });
    }

    fn resume_picker(&mut self) -> Result<()> {
        let current = self.session().meta["id"].as_str().unwrap_or("").to_string();
        let cwd = json!(self.cwd());
        let mut sessions: Vec<Value> = store::list(&self.ctx.root)?
            .into_iter()
            .filter(|s| s["id"] != json!(current))
            .collect();
        sessions.sort_by_key(|s| s["cwd"] != cwd);
        let items: Vec<_> = sessions
            .iter()
            .take(50)
            .map(|s| {
                let id = s["id"].as_str().unwrap_or("").to_string();
                let title =
                    store::title(&self.ctx.root, &id).unwrap_or_else(|| "(no prompt)".into());
                let title = cli::safe_text(&title).replace('\n', " ");
                let mut detail = ago(s["updated"].as_str().unwrap_or(""));
                if s["cwd"] != cwd {
                    detail.push_str(&format!(" · {}", s["cwd"].as_str().unwrap_or("")));
                }
                if s["pending"] == true {
                    detail.push_str(" · unfinished");
                }
                (title.chars().take(70).collect(), detail, id)
            })
            .collect();
        if items.is_empty() {
            self.out("/resume", vec!["No other conversations found".into()]);
            return Ok(());
        }
        self.picker = Some(Picker {
            kind: PickKind::Resume,
            title: "Resume a conversation".into(),
            hint: "Enter to resume · Esc to exit".into(),
            items,
            sel: 0,
        });
        Ok(())
    }

    fn picked(&mut self, kind: PickKind, value: String) -> Result<()> {
        match kind {
            PickKind::Model => {
                let msg = self.set_model(&value)?;
                self.out("/model", vec![msg]);
            }
            PickKind::Resume => self.open_session(&value)?,
        }
        Ok(())
    }

    fn open_session(&mut self, id: &str) -> Result<()> {
        let mut s = match Session::open(&self.ctx.root, id) {
            Ok(s) => s,
            Err(e) => {
                self.out("/resume", vec![format!("Could not open {id}: {e}")]);
                return Ok(());
            }
        };
        if let Err(e) = s.acquire() {
            self.out("/resume", vec![format!("Could not open {id}: {e}")]);
            return Ok(());
        }
        self.session = Some(s);
        self.files = None;
        self.blocks = vec![Block::Header];
        self.replay();
        self.redraw_all()
    }

    // ── shell mode, editor, suspend ──

    fn run_bash(&mut self, cmd: String) -> Result<()> {
        self.blocks.push(Block::BashIn(cmd.clone()));
        self.draw()?;
        let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let child = Command::new(&shell)
            .arg("-c")
            .arg(&cmd)
            .current_dir(self.cwd())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                self.blocks
                    .push(Block::Out(vec![(Style::fg(ERROR), e.to_string())]));
                return Ok(());
            }
        };
        let mut so = child.stdout.take().unwrap();
        let mut se = child.stderr.take().unwrap();
        let a = thread::spawn(move || {
            let mut b = vec![];
            let _ = so.by_ref().take(1 << 20).read_to_end(&mut b);
            b
        });
        let b = thread::spawn(move || {
            let mut b = vec![];
            let _ = se.by_ref().take(1 << 20).read_to_end(&mut b);
            b
        });
        let mut interrupted = false;
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break Some(s);
            }
            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(k) = event::read()? {
                    let ctrl_c =
                        k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
                    if k.code == KeyCode::Esc || ctrl_c {
                        unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                        interrupted = true;
                        let _ = child.wait();
                        break None;
                    }
                }
            }
        };
        unsafe { libc::kill(-(child.id() as i32), libc::SIGTERM) };
        let out = cli::safe_text(&String::from_utf8_lossy(&a.join().unwrap_or_default()));
        let err = cli::safe_text(&String::from_utf8_lossy(&b.join().unwrap_or_default()));
        let failed = interrupted || status.is_some_and(|s| !s.success());
        let text = format!(
            "{}{}{}",
            out,
            if !out.is_empty() && !err.is_empty() {
                "\n"
            } else {
                ""
            },
            err
        );
        let tool = view::tool(String::new(), "Bash", json!({"command":cmd}), &self.cwd());
        let shown = if interrupted {
            "Interrupted".to_string()
        } else {
            text.clone()
        };
        let mut rows = view::tool_body(
            &tool,
            &shown,
            failed && text.trim().is_empty() || interrupted,
            self.ctx.verbose,
            self.cols,
        );
        if rows.is_empty() {
            rows.push(Line::of(Style::fg(DIM), "  ⎿  (No content)"));
        }
        self.blocks.push(Block::Out(
            rows.iter()
                .map(|l| {
                    (
                        Style::plain(),
                        l.text()
                            .trim_start_matches("  ⎿  ")
                            .trim_start()
                            .to_string(),
                    )
                })
                .collect(),
        ));
        self.pending_bash.push(format!(
            "<bash-input>{cmd}</bash-input>\n<bash-stdout>{out}</bash-stdout>\n<bash-stderr>{err}</bash-stderr>"
        ));
        Ok(())
    }

    fn with_terminal<T>(&mut self, f: impl FnOnce() -> T) -> Result<T> {
        self.finish_screen();
        self.cursor_row = 0;
        // Dropping the guard restores the terminal before handing it over.
        self.raw.take();
        let out = f();
        self.raw = Some(RawMode::on()?);
        self.draw()?;
        Ok(out)
    }

    fn edit_file(&mut self, path: &Path) -> Result<()> {
        let editor = env::var("VISUAL")
            .or_else(|_| env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".into());
        let mut parts = editor.split_whitespace();
        let program = parts.next().unwrap_or("vi").to_string();
        let args: Vec<String> = parts.map(str::to_owned).collect();
        let path = path.to_path_buf();
        self.with_terminal(move || {
            let _ = Command::new(program).args(args).arg(&path).status();
        })
    }

    fn external_editor(&mut self) -> Result<()> {
        let path = self
            .ctx
            .root
            .join(format!("prompt-{}.md", std::process::id()));
        {
            let mut f = private_options()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)?;
            f.write_all(self.input.buf.as_bytes())?;
        }
        self.edit_file(&path)?;
        if let Ok(text) = fs::read_to_string(&path) {
            self.input.set(text.trim_end());
        }
        let _ = fs::remove_file(&path);
        Ok(())
    }

    fn suspend(&mut self) -> Result<()> {
        self.with_terminal(|| unsafe {
            libc::kill(0, libc::SIGTSTP);
        })
    }
}

fn describe(status: &str) -> &'static str {
    match status {
        "limit" => "hit its usage limit",
        "transient" => "is temporarily unavailable",
        "missing" => "is not installed",
        _ => "stopped",
    }
}

fn cap(mut top: Vec<Line>, bottom: Vec<Line>, rows: usize) -> Vec<Line> {
    let room = rows.saturating_sub(1).max(bottom.len());
    if top.len() + bottom.len() > room {
        let drop = top.len() + bottom.len() - room;
        top.drain(..drop.min(top.len()));
    }
    top.extend(bottom);
    top
}

fn ago(stamp: &str) -> String {
    let Ok(t) = chrono::DateTime::parse_from_rfc3339(stamp) else {
        return stamp.to_string();
    };
    let secs = (chrono::Utc::now() - t.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

fn history_path(root: &Path) -> PathBuf {
    root.join("history.jsonl")
}
fn load_history(root: &Path, cwd: &Path) -> Vec<String> {
    let cwd = json!(cwd);
    let mut h: Vec<String> = fs::read_to_string(history_path(root))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["cwd"] == cwd)
        .filter_map(|v| v["text"].as_str().map(str::to_owned))
        .collect();
    let n = h.len();
    h.drain(..n.saturating_sub(500));
    h
}
fn save_history(root: &Path, cwd: &Path, text: &str) {
    if store::private_dir(root).is_err() {
        return;
    }
    if let Ok(mut f) = private_options()
        .create(true)
        .append(true)
        .open(history_path(root))
    {
        let _ = writeln!(f, "{}", json!({"cwd":cwd,"text":text}));
    }
}

fn frontmatter_description(text: &str) -> String {
    let mut lines = text.lines();
    if text.starts_with("---") {
        lines.next();
        for l in lines.by_ref() {
            if l.starts_with("---") {
                break;
            }
            if let Some(d) = l.strip_prefix("description:") {
                return d.trim().trim_matches('"').to_string();
            }
        }
    }
    lines
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .unwrap_or_default()
}

/// Custom slash commands and skills, discovered where Claude Code keeps them.
fn custom_commands(cwd: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    for base in [cwd.join(".claude"), config::home().join(".claude")] {
        let commands = base.join("commands");
        let mut stack = vec![commands.clone()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "md") {
                    let name = p
                        .strip_prefix(&commands)
                        .unwrap_or(&p)
                        .with_extension("")
                        .display()
                        .to_string()
                        .replace('/', ":");
                    let d = fs::read_to_string(&p)
                        .map(|t| frontmatter_description(&t))
                        .unwrap_or_default();
                    out.push((name, d));
                }
            }
        }
        for e in fs::read_dir(base.join("skills"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let skill = e.path().join("SKILL.md");
            if let Ok(t) = fs::read_to_string(&skill) {
                out.push((
                    e.file_name().to_string_lossy().into_owned(),
                    frontmatter_description(&t),
                ));
            }
        }
    }
    out.retain(|(n, _)| !BUILTINS.iter().any(|(b, _)| b == n));
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// Project files for @-mentions, skipping VCS, dependency, and build folders.
fn list_files(cwd: &Path) -> Vec<String> {
    let mut out = vec![];
    let mut stack = vec![cwd.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if (name.starts_with('.') && name != ".github" && name != ".claude")
                || ["node_modules", "target", "dist", "build", "__pycache__"]
                    .contains(&name.as_str())
            {
                continue;
            }
            let p = e.path();
            let rel = p.strip_prefix(cwd).unwrap_or(&p).display().to_string();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                out.push(format!("{rel}/"));
                stack.push(p);
            } else {
                out.push(rel);
            }
            if out.len() >= 20000 {
                return out;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn tool(id: &str, name: &str) -> Item {
        Item::ToolUse(view::tool(
            id.into(),
            name,
            json!({"file_path":"a","command":"ls"}),
            Path::new("/"),
        ))
    }
    #[test]
    fn live_groups_reads_and_orders_tools() {
        let mut live = Live::default();
        let mut out = vec![];
        live.feed(Item::Text("Looking.\n\nSecond".into()), false, &mut out);
        assert!(matches!(&out[0], Block::Text { first: true, .. }));
        live.feed(tool("r1", "Read"), false, &mut out);
        live.feed(
            Item::ToolResult {
                id: Some("r1".into()),
                output: "x".into(),
                error: false,
            },
            false,
            &mut out,
        );
        live.feed(tool("b1", "Bash"), false, &mut out);
        live.feed(
            Item::ToolResult {
                id: Some("b1".into()),
                output: "ok".into(),
                error: false,
            },
            false,
            &mut out,
        );
        live.feed(Item::Text("Done.".into()), false, &mut out);
        live.finish(false, &mut out);
        let kinds: Vec<_> = out
            .iter()
            .map(|b| match b {
                Block::Text { first, .. } => format!("text{first}"),
                Block::Quiet(c) => format!("quiet{}", c[0]),
                Block::Tool { tool, .. } => tool.label.clone(),
                _ => "other".into(),
            })
            .collect();
        assert_eq!(
            kinds,
            ["texttrue", "textfalse", "quiet1", "Bash", "texttrue"]
        );
    }
    #[test]
    fn interrupted_tools_are_marked() {
        let mut live = Live::default();
        let mut out = vec![];
        live.feed(tool("b", "Bash"), false, &mut out);
        live.finish(true, &mut out);
        assert!(matches!(
            &out[0],
            Block::Tool {
                result: Some((_, true)),
                ..
            }
        ));
    }
    #[test]
    fn editor_basics() {
        let mut e = Editor::default();
        e.insert("hello world");
        e.cur = e.word_left();
        assert_eq!(&e.buf[e.cur..], "world");
        e.insert("big ");
        assert_eq!(e.buf, "hello big world");
        e.insert("\nnext");
        assert!(e.vertical(true));
        assert_eq!(e.line_start(), 0);
        let tag = paste_tag(0, "a\nb");
        e.pastes.push("a\nb".into());
        e.set(&format!("see {tag}"));
        assert_eq!(e.take(), "see a\nb");
    }
    #[test]
    fn frontmatter() {
        assert_eq!(
            frontmatter_description("---\ndescription: Ship it\n---\nbody"),
            "Ship it"
        );
        assert_eq!(frontmatter_description("# Title\nbody"), "Title");
    }
}
