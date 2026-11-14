//! Pure rendering: styles, Lennon's mascot, wrapping, Markdown, and the
//! translation of provider protocol events into Claude Code style items.
use crate::provider::Decoded;
use serde_json::Value;
use std::{
    path::Path,
    sync::atomic::{AtomicU8, Ordering},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const ACCENT: u32 = 0xd77757;
pub const SHIMMER: u32 = 0xeb9f7f;
pub const DIM: u32 = 0x999999;
pub const RULE: u32 = 0x888888;
pub const USER_BG: u32 = 0x373737;
pub const SUCCESS: u32 = 0x4eba65;
pub const ERROR: u32 = 0xff6b80;
pub const WARNING: u32 = 0xffc107;
pub const PLAN: u32 = 0x48968c;
pub const EDITS: u32 = 0xaf87ff;
pub const BASH: u32 = 0xfd5db1;
pub const CODE: u32 = 0xb1b9f9;
pub const ADDED_BG: u32 = 0x225c2b;
pub const REMOVED_BG: u32 = 0x7a2936;

/// 0 = no color, 1 = 256 colors, 2 = truecolor.
static DEPTH: AtomicU8 = AtomicU8::new(2);
pub fn detect_color() {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let depth = if std::env::var_os("NO_COLOR").is_some() || env("TERM") == "dumb" {
        0
    } else if ["truecolor", "24bit"].contains(&env("COLORTERM").as_str())
        || ["iTerm.app", "WezTerm", "ghostty", "vscode"].contains(&env("TERM_PROGRAM").as_str())
        || env("TERM").contains("kitty")
        || env("TERM").contains("ghostty")
    {
        2
    } else {
        1
    };
    DEPTH.store(depth, Ordering::Relaxed);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}
impl Style {
    pub const fn fg(c: u32) -> Self {
        Self {
            fg: Some(c),
            bg: None,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            strike: false,
        }
    }
    pub const fn plain() -> Self {
        Self {
            fg: None,
            bg: None,
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            strike: false,
        }
    }
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
    pub const fn bg(mut self, c: u32) -> Self {
        self.bg = Some(c);
        self
    }
    pub const fn italic(mut self) -> Self {
        self.italic = true;
        self
    }
    pub const fn strike(mut self) -> Self {
        self.strike = true;
        self
    }
    fn sgr(&self) -> String {
        let depth = DEPTH.load(Ordering::Relaxed);
        if depth == 0 {
            return String::new();
        }
        let mut codes: Vec<String> = vec![];
        if self.bold {
            codes.push("1".into());
        }
        if self.dim {
            codes.push("2".into());
        }
        if self.italic {
            codes.push("3".into());
        }
        if self.underline {
            codes.push("4".into());
        }
        if self.strike {
            codes.push("9".into());
        }
        for (base, c) in [(38, self.fg), (48, self.bg)] {
            if let Some(c) = c {
                let (r, g, b) = ((c >> 16) & 255, (c >> 8) & 255, c & 255);
                codes.push(if depth == 2 {
                    format!("{base};2;{r};{g};{b}")
                } else {
                    format!("{base};5;{}", ansi256(r, g, b))
                });
            }
        }
        if codes.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", codes.join(";"))
        }
    }
}
fn ansi256(r: u32, g: u32, b: u32) -> u32 {
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        if r > 248 {
            return 231;
        }
        return 232 + (r - 8) * 24 / 240;
    }
    let q = |v: u32| if v < 48 { 0 } else { ((v - 35) / 40).min(5) };
    16 + 36 * q(r) + 6 * q(g) + q(b)
}

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// One terminal row made of styled spans. Never wider than the terminal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line(pub Vec<(Style, String)>);
impl Line {
    pub fn new() -> Self {
        Self(vec![])
    }
    pub fn of(style: Style, text: impl Into<String>) -> Self {
        Self(vec![(style, text.into())])
    }
    pub fn push(mut self, style: Style, text: impl Into<String>) -> Self {
        self.0.push((style, text.into()));
        self
    }
    pub fn add(&mut self, style: Style, text: impl Into<String>) {
        self.0.push((style, text.into()));
    }
    pub fn width(&self) -> usize {
        self.0.iter().map(|(_, t)| width(t)).sum()
    }
    pub fn text(&self) -> String {
        self.0.iter().map(|(_, t)| t.as_str()).collect()
    }
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (style, text) in &self.0 {
            let sgr = style.sgr();
            out.push_str(&sgr);
            out.push_str(text);
            if !sgr.is_empty() {
                out.push_str("\x1b[0m");
            }
        }
        out
    }
    /// Truncate to `max` columns, appending an ellipsis when cut.
    pub fn truncate(mut self, max: usize) -> Self {
        if self.width() <= max {
            return self;
        }
        let mut out = Line::new();
        let mut used = 0;
        for (style, text) in self.0.drain(..) {
            let mut piece = String::new();
            for c in text.chars() {
                let w = c.width().unwrap_or(0);
                if used + w + 1 > max {
                    out.add(style, piece);
                    out.add(Style::fg(DIM), "…");
                    return out;
                }
                used += w;
                piece.push(c);
            }
            out.add(style, piece);
        }
        out
    }
}

/// Word-wrap styled spans. The prefixes are repeated on every row.
pub fn wrap(spans: &[(Style, String)], cols: usize, first: &Line, rest: &Line) -> Vec<Line> {
    let mut lines = vec![];
    let mut line = first.clone();
    let mut used = first.width();
    let mut limit = cols.max(first.width() + 4);
    let mut at_start = true;
    let mut words: Vec<(Style, String)> = vec![];
    for (style, text) in spans {
        let mut word = String::new();
        for c in text.chars() {
            if c == ' ' {
                if !word.is_empty() {
                    words.push((*style, std::mem::take(&mut word)));
                }
                words.push((*style, " ".into()));
            } else {
                word.push(c);
            }
        }
        if !word.is_empty() {
            words.push((*style, word));
        }
    }
    for (style, word) in words {
        let w = width(&word);
        if word == " " {
            if !at_start && used < limit {
                line.add(style, " ");
                used += 1;
            }
            continue;
        }
        if used + w > limit && !at_start {
            // Drop trailing space before breaking.
            if let Some((_, t)) = line.0.last_mut() {
                if t == " " {
                    line.0.pop();
                }
            }
            lines.push(std::mem::replace(&mut line, rest.clone()));
            used = rest.width();
            limit = cols.max(rest.width() + 4);
        }
        if used + w > limit {
            // A word longer than a row: hard-break by character.
            let mut piece = String::new();
            for c in word.chars() {
                let cw = c.width().unwrap_or(0);
                if used + cw > limit {
                    line.add(style, std::mem::take(&mut piece));
                    lines.push(std::mem::replace(&mut line, rest.clone()));
                    used = rest.width();
                }
                piece.push(c);
                used += cw;
            }
            line.add(style, piece);
        } else {
            line.add(style, word);
            used += w;
        }
        at_start = false;
    }
    lines.push(line);
    lines
}

/// Lennon: a black podenco with one ear up and one folded, silver eyebrows,
/// and a silver beard. Nine columns by three rows of half blocks, the same
/// footprint as Claude Code's mascot.
pub fn mascot() -> Vec<Line> {
    const GRID: [&str; 6] = [
        ".......B.",
        "BB....BB.",
        "B.BBBBBB.",
        ".BSSBSSB.",
        ".BEBBBEB.",
        "..SSNSS..",
    ];
    let color = |c: u8| match c {
        b'B' => Some(0x5c6058),
        b'S' => Some(0xcfcab6),
        b'E' | b'N' => Some(0x121b19),
        _ => None,
    };
    (0..3)
        .map(|row| {
            let mut line = Line::new();
            for x in 0..9 {
                let top = color(GRID[row * 2].as_bytes()[x]);
                let bottom = color(GRID[row * 2 + 1].as_bytes()[x]);
                let (glyph, style) = match (top, bottom) {
                    (None, None) => (" ", Style::plain()),
                    (Some(t), None) => ("▀", Style::fg(t)),
                    (None, Some(b)) => ("▄", Style::fg(b)),
                    (Some(t), Some(b)) if t == b => ("█", Style::fg(t)),
                    (Some(t), Some(b)) => ("▀", Style::fg(t).bg(b)),
                };
                line.add(style, glyph);
            }
            line
        })
        .collect()
}

/// Inline Markdown for one source line: bold, italics, code, and links.
fn inline(text: &str, base: Style) -> Vec<(Style, String)> {
    let mut out: Vec<(Style, String)> = vec![];
    let mut cur = String::new();
    let (mut bold, mut italic, mut code) = (false, false, false);
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let style = |b: bool, it: bool, c: bool| {
        let mut s = base;
        if c {
            s.fg = Some(CODE);
        }
        s.bold |= b;
        s.italic |= it;
        s
    };
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        let flip = |out: &mut Vec<(Style, String)>, cur: &mut String, s: Style| {
            if !cur.is_empty() {
                out.push((s, std::mem::take(cur)));
            }
        };
        if c == '`' {
            flip(&mut out, &mut cur, style(bold, italic, code));
            code = !code;
            i += 1;
            continue;
        }
        if !code && c == '*' && next == Some('*') {
            flip(&mut out, &mut cur, style(bold, italic, code));
            bold = !bold;
            i += 2;
            continue;
        }
        if !code
            && (c == '*' || c == '_')
            && (italic
                || next.is_some_and(|n| !n.is_whitespace())
                    && (i == 0 || !chars[i - 1].is_alphanumeric()))
            && (!italic || i == 0 || !chars[i - 1].is_whitespace())
            && text[..].matches(c).count() >= 2
        {
            flip(&mut out, &mut cur, style(bold, italic, code));
            italic = !italic;
            i += 1;
            continue;
        }
        if !code && c == '[' {
            let rest: String = chars[i..].iter().collect();
            if let Some(close) = rest.find("](") {
                if let Some(end) = rest[close..].find(')') {
                    flip(&mut out, &mut cur, style(bold, italic, code));
                    let label = &rest[1..close];
                    let url = &rest[close + 2..close + end];
                    out.push((style(bold, italic, false).underline(), label.into()));
                    if label != url {
                        out.push((Style::fg(DIM), format!(" ({url})")));
                    }
                    i += rest[..close + end + 1].chars().count();
                    continue;
                }
            }
        }
        cur.push(c);
        i += 1;
    }
    if !cur.is_empty() {
        out.push((style(bold, italic, code), cur));
    }
    out
}
impl Style {
    const fn underline(mut self) -> Self {
        self.underline = true;
        self
    }
}

/// Render an assistant message: "⏺ " on the first row, two-space indent after.
pub fn assistant(text: &str, cols: usize, first: bool) -> Vec<Line> {
    let mut lines = vec![];
    let mut in_code = false;
    let mut lead = first;
    let bullet = Line::of(Style::plain(), "⏺ ");
    let indent = Line::of(Style::plain(), "  ");
    for raw in text.trim_matches('\n').lines() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        let pre = if lead { &bullet } else { &indent };
        lead = false;
        if in_code {
            let l = pre.clone().push(Style::plain(), raw.to_string());
            lines.extend(wrap(&l.0[1..], cols, pre, &indent));
            continue;
        }
        if trimmed.is_empty() {
            lines.push(Line::new());
            continue;
        }
        if trimmed.chars().all(|c| c == '-' || c == '*' || c == '_') && trimmed.len() >= 3 {
            lines.push(
                pre.clone()
                    .push(Style::fg(DIM), "─".repeat(cols.saturating_sub(4).min(60))),
            );
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        let spans = if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            let mut s = Style::plain().bold();
            if hashes == 1 {
                s = s.italic().underline();
            }
            inline(trimmed[hashes + 1..].trim(), s)
        } else if let Some(q) = trimmed.strip_prefix("> ") {
            inline(q, Style::fg(DIM).italic())
        } else {
            let indent_len = raw.len() - trimmed.len();
            let mut s = vec![(Style::plain(), " ".repeat(indent_len))];
            if let Some(item) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
            {
                s.push((Style::plain(), "- ".into()));
                s.extend(inline(item, Style::plain()));
            } else {
                s.extend(inline(trimmed, Style::plain()));
            }
            s
        };
        lines.extend(wrap(&spans, cols, pre, &indent));
    }
    lines
}

/// A user message: grey band with a dim "❯".
pub fn user(text: &str, cols: usize) -> Vec<Line> {
    let bg = Style::fg(0xffffff).bg(USER_BG);
    let mut out = vec![];
    for (i, row) in text.trim_end().lines().enumerate() {
        let first = Line::of(Style::fg(DIM).bg(USER_BG), if i == 0 { "❯ " } else { "  " });
        let rest = Line::of(bg, "  ");
        for mut l in wrap(&[(bg, row.to_string())], cols, &first, &rest) {
            let pad = cols.saturating_sub(l.width());
            l.add(bg, " ".repeat(pad));
            out.push(l);
        }
    }
    out
}

/// "  ⎿  " followed by result rows, the classic Claude Code tool-result shape.
pub fn result_rows(rows: &[(Style, String)], cols: usize) -> Vec<Line> {
    let mut out = vec![];
    for (i, (style, text)) in rows.iter().enumerate() {
        let first = Line::of(Style::fg(DIM), if i == 0 { "  ⎿  " } else { "     " });
        let rest = Line::of(Style::plain(), "     ");
        out.extend(wrap(&[(*style, text.clone())], cols, &first, &rest));
    }
    out
}

/// What kind of quiet, groupable tool a call is (Claude Code collapses reads).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quiet {
    Read,
    Search,
    List,
}

#[derive(Clone, Debug)]
pub struct Tool {
    pub id: String,
    pub name: String,
    pub label: String,
    pub arg: String,
    pub input: Value,
    pub quiet: Option<Quiet>,
}

#[derive(Clone, Debug)]
pub enum Item {
    /// Streamed assistant text.
    Text(String),
    /// A complete assistant message.
    Block(String),
    ToolUse(Tool),
    ToolResult {
        id: Option<String>,
        output: String,
        error: bool,
    },
    Usage {
        cost: f64,
        input: u64,
        output: u64,
    },
}

fn rel(path: &str, cwd: &Path) -> String {
    let p = Path::new(path);
    p.strip_prefix(cwd)
        .map(|r| r.display().to_string())
        .unwrap_or_else(|_| path.to_string())
}
fn str_of(v: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| v[*k].as_str())
        .unwrap_or("")
        .to_string()
}
fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("");
    let more = s.lines().nth(1).is_some();
    let mut out: String = line.chars().take(max).collect();
    if more || line.chars().count() > max {
        out.push('…');
    }
    out
}

/// Present a tool the way Claude Code names it, for any provider.
pub fn tool(id: String, name: &str, input: Value, cwd: &Path) -> Tool {
    let path = rel(
        &str_of(
            &input,
            &[
                "file_path",
                "path",
                "notebook_path",
                "absolute_path",
                "dir_path",
            ],
        ),
        cwd,
    );
    let (label, arg, quiet): (String, String, Option<Quiet>) = match name {
        "Read" | "read_file" | "read_many_files" => ("Read".into(), path, Some(Quiet::Read)),
        "Write" | "write_file" => ("Write".into(), path, None),
        "Edit" | "MultiEdit" | "replace" | "NotebookEdit" | "edit" => ("Update".into(), path, None),
        "Bash" | "run_shell_command" | "command_execution" => (
            "Bash".into(),
            first_line(&str_of(&input, &["command"]), 160),
            None,
        ),
        "Grep" | "search_file_content" | "grep" => {
            let pat = str_of(&input, &["pattern"]);
            let mut a = format!("pattern: \"{pat}\"");
            if !path.is_empty() {
                a.push_str(&format!(", path: \"{path}\""));
            }
            ("Search".into(), a, Some(Quiet::Search))
        }
        "Glob" | "glob" => (
            "Search".into(),
            format!("pattern: \"{}\"", str_of(&input, &["pattern"])),
            Some(Quiet::Search),
        ),
        "LS" | "list_directory" => ("List".into(), path, Some(Quiet::List)),
        "WebFetch" | "web_fetch" => ("Fetch".into(), str_of(&input, &["url", "prompt"]), None),
        "WebSearch" | "google_web_search" | "web_search" => (
            "Web Search".into(),
            format!("\"{}\"", str_of(&input, &["query"])),
            None,
        ),
        "Task" | "Agent" => (
            str_of(&input, &["subagent_type"])
                .chars()
                .next()
                .map(|_| {
                    let t = str_of(&input, &["subagent_type"]);
                    let mut c = t.chars();
                    c.next().unwrap().to_uppercase().collect::<String>() + c.as_str()
                })
                .unwrap_or_else(|| "Task".into()),
            str_of(&input, &["description", "prompt"]),
            None,
        ),
        "TodoWrite" | "todo_list" | "write_todos" => ("Update Todos".into(), String::new(), None),
        "Skill" => ("Skill".into(), str_of(&input, &["skill", "command"]), None),
        "file_change" => (
            "Update".into(),
            input["changes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|c| rel(c["path"].as_str().unwrap_or(""), cwd))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default(),
            None,
        ),
        "mcp_tool_call" => (
            format!(
                "{} - {} (MCP)",
                str_of(&input, &["server"]),
                str_of(&input, &["tool"])
            ),
            String::new(),
            None,
        ),
        n if n.starts_with("mcp__") => {
            let parts: Vec<_> = n.splitn(3, "__").collect();
            (
                format!(
                    "{} - {} (MCP)",
                    parts.get(1).unwrap_or(&""),
                    parts.get(2).unwrap_or(&"")
                ),
                String::new(),
                None,
            )
        }
        other => {
            let mut c = other.chars();
            let pretty = c
                .next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
                .replace('_', " ");
            (pretty, String::new(), None)
        }
    };
    Tool {
        id,
        name: name.into(),
        label,
        arg,
        input,
        quiet,
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Claude Code's collapsed summary of reads and searches.
pub fn quiet_summary(counts: [usize; 3], done: bool) -> String {
    let [r, s, l] = counts;
    let mut parts = vec![];
    if r > 0 {
        parts.push(format!(
            "{} {}",
            if done { "Read" } else { "Reading" },
            plural(r, "file", "files")
        ));
    }
    if s > 0 {
        parts.push(format!(
            "{} {}",
            if done {
                "searched for"
            } else {
                "searching for"
            },
            plural(s, "pattern", "patterns")
        ));
    }
    if l > 0 {
        parts.push(format!(
            "{} {}",
            if done { "listed" } else { "listing" },
            plural(l, "directory", "directories")
        ));
    }
    let mut text = parts.join(", ");
    if let Some(f) = text.get_mut(0..1) {
        f.make_ascii_uppercase();
    }
    if !done {
        text.push('…');
    }
    text
}

/// The header row of a tool call: "⏺ Label(arg)".
pub fn tool_header(t: &Tool, dot: Style, cols: usize) -> Vec<Line> {
    let mut spans = vec![(Style::plain().bold(), t.label.clone())];
    if !t.arg.is_empty() {
        spans.push((Style::plain(), format!("({})", t.arg)));
    }
    let first = Line::of(dot, "⏺ ");
    let rest = Line::of(Style::plain(), "  ");
    wrap(&spans, cols, &first, &rest)
}

/// The "⎿" body of a finished tool call.
pub fn tool_body(t: &Tool, output: &str, error: bool, verbose: bool, cols: usize) -> Vec<Line> {
    let dim = Style::fg(DIM);
    let lines: Vec<&str> = output.trim_end().lines().collect();
    let n = lines.len();
    let mut rows: Vec<(Style, String)> = vec![];
    if error {
        let text = output.trim();
        let text = text
            .strip_prefix("<tool_use_error>")
            .and_then(|t| t.strip_suffix("</tool_use_error>"))
            .unwrap_or(text);
        let shown: Vec<&str> = text
            .lines()
            .take(if verbose { usize::MAX } else { 3 })
            .collect();
        for (i, l) in shown.iter().enumerate() {
            rows.push((
                Style::fg(ERROR),
                if i == 0 && !l.starts_with("Error") {
                    format!("Error: {l}")
                } else {
                    l.to_string()
                },
            ));
        }
        return result_rows(&rows, cols);
    }
    match t.label.as_str() {
        "Read" => rows.push((dim, format!("Read {}", plural(n, "line", "lines")))),
        "Search" => rows.push((
            dim,
            format!(
                "Found {}",
                if t.name.eq_ignore_ascii_case("glob") {
                    plural(n, "file", "files")
                } else {
                    plural(n, "line", "lines")
                }
            ),
        )),
        "List" => rows.push((dim, format!("Listed {}", plural(n, "path", "paths")))),
        "Write" => {
            let content = t.input["content"].as_str().unwrap_or("");
            let c = content.lines().count();
            rows.push((
                dim,
                format!("Wrote {} to {}", plural(c, "line", "lines"), t.arg),
            ));
        }
        "Update Todos" => {
            for todo in t.input["todos"]
                .as_array()
                .or_else(|| t.input["items"].as_array())
                .into_iter()
                .flatten()
            {
                let text = str_of(todo, &["content", "text", "description"]);
                let status = str_of(todo, &["status"]);
                let done = status == "completed" || todo["completed"] == true;
                rows.push(if done {
                    (dim.strike(), format!("☒ {text}"))
                } else if status == "in_progress" {
                    (Style::plain().bold(), format!("☐ {text}"))
                } else {
                    (Style::plain(), format!("☐ {text}"))
                });
            }
        }
        "Update" => {
            let (old, new) = (
                t.input["old_string"].as_str().unwrap_or(""),
                t.input["new_string"].as_str().unwrap_or(""),
            );
            if old.is_empty() && new.is_empty() {
                rows.push((dim, first_line(output.trim(), 200)));
                if rows[0].1.is_empty() {
                    rows[0].1 = "Updated".into();
                }
                return result_rows(&rows, cols);
            }
            let (removed, added) = diff(old, new);
            rows.push((
                dim,
                format!(
                    "Updated {} with {} and {}",
                    t.arg,
                    plural(added.len(), "addition", "additions"),
                    plural(removed.len(), "removal", "removals")
                ),
            ));
            let mut out = result_rows(&rows, cols);
            let cap = if verbose { usize::MAX } else { 8 };
            for (lines, sign, bg) in [(&removed, "-", REMOVED_BG), (&added, "+", ADDED_BG)] {
                for l in lines.iter().take(cap) {
                    let mut row = Line::of(Style::plain(), "     ")
                        .push(Style::plain().bg(bg), format!("{sign} {l}"))
                        .truncate(cols);
                    let pad = cols.saturating_sub(row.width());
                    row.add(Style::plain().bg(bg), " ".repeat(pad.min(cols)));
                    out.push(row);
                }
                if lines.len() > cap {
                    out.push(Line::of(
                        dim,
                        format!("     … +{} lines", lines.len() - cap),
                    ));
                }
            }
            return out;
        }
        _ => {
            if n == 0 {
                rows.push((dim, "(No content)".into()));
            } else {
                let cap = if verbose { usize::MAX } else { 3 };
                for l in lines.iter().take(cap) {
                    rows.push((Style::plain(), l.to_string()));
                }
                if n > cap {
                    rows.push((dim, format!("… +{} lines (ctrl+o to expand)", n - cap)));
                }
            }
        }
    }
    result_rows(&rows, cols)
}

/// Lines removed and added after trimming the common head and tail.
fn diff(old: &str, new: &str) -> (Vec<String>, Vec<String>) {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let mut head = 0;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < a.len() - head
        && tail < b.len() - head
        && a[a.len() - 1 - tail] == b[b.len() - 1 - tail]
    {
        tail += 1;
    }
    (
        a[head..a.len() - tail]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        b[head..b.len() - tail]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    )
}

fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b["text"].as_str().or_else(|| b.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Translate one provider protocol event into display items.
pub fn interpret(adapter: &str, raw: &Value, decoded: &Decoded, cwd: &Path) -> Vec<Item> {
    let mut items = vec![];
    let t = raw["type"].as_str().unwrap_or("");
    // Subagent internals stay hidden, as in Claude Code.
    if raw.get("parent_tool_use_id").is_some_and(|v| !v.is_null()) {
        return items;
    }
    match adapter {
        "claude" => {
            if !decoded.text.is_empty() {
                items.push(Item::Text(decoded.text.clone()));
            }
            if let Some(blocks) = raw["message"]["content"].as_array() {
                for b in blocks {
                    if t == "assistant" && b["type"] == "tool_use" {
                        items.push(Item::ToolUse(tool(
                            b["id"].as_str().unwrap_or("").into(),
                            b["name"].as_str().unwrap_or("Tool"),
                            b["input"].clone(),
                            cwd,
                        )));
                    }
                    if t == "user" && b["type"] == "tool_result" {
                        items.push(Item::ToolResult {
                            id: b["tool_use_id"].as_str().map(str::to_owned),
                            output: content_text(&b["content"]),
                            error: b["is_error"] == true,
                        });
                    }
                }
            }
            if t == "result" {
                items.push(Item::Usage {
                    cost: raw["total_cost_usd"].as_f64().unwrap_or(0.0),
                    input: raw["usage"]["input_tokens"].as_u64().unwrap_or(0)
                        + raw["usage"]["cache_read_input_tokens"]
                            .as_u64()
                            .unwrap_or(0)
                        + raw["usage"]["cache_creation_input_tokens"]
                            .as_u64()
                            .unwrap_or(0),
                    output: raw["usage"]["output_tokens"].as_u64().unwrap_or(0),
                });
            }
        }
        "codex" => {
            let item = &raw["item"];
            let kind = item["type"].as_str().unwrap_or("");
            let id = item["id"].as_str().unwrap_or("").to_string();
            if t == "item.completed" && kind == "agent_message" {
                items.push(Item::Block(decoded.text.clone()));
            } else if (t == "item.started" || t == "item.completed")
                && !["agent_message", "reasoning", "error"].contains(&kind)
            {
                items.push(Item::ToolUse(tool(id.clone(), kind, item.clone(), cwd)));
                if t == "item.completed" {
                    let failed = item["status"] == "failed"
                        || item["exit_code"].as_i64().is_some_and(|c| c != 0);
                    let output = match kind {
                        "command_execution" => {
                            item["aggregated_output"].as_str().unwrap_or("").to_string()
                        }
                        "mcp_tool_call" => content_text(&item["result"]["content"]),
                        "file_change" => format!(
                            "Changed {}",
                            plural(
                                item["changes"].as_array().map(Vec::len).unwrap_or(0),
                                "file",
                                "files"
                            )
                        ),
                        _ => String::new(),
                    };
                    items.push(Item::ToolResult {
                        id: Some(id),
                        output,
                        error: failed,
                    });
                }
            } else if t == "turn.completed" {
                items.push(Item::Usage {
                    cost: 0.0,
                    input: raw["usage"]["input_tokens"].as_u64().unwrap_or(0),
                    output: raw["usage"]["output_tokens"].as_u64().unwrap_or(0),
                });
            } else if !decoded.text.is_empty() {
                items.push(Item::Text(decoded.text.clone()));
            }
        }
        _ => {
            if !decoded.text.is_empty() {
                items.push(Item::Text(decoded.text.clone()));
            }
            if t == "tool_use" {
                items.push(Item::ToolUse(tool(
                    raw["tool_id"].as_str().unwrap_or("").into(),
                    raw["tool_name"].as_str().unwrap_or("tool"),
                    raw["parameters"].clone(),
                    cwd,
                )));
            }
            if t == "tool_result" {
                items.push(Item::ToolResult {
                    id: raw["tool_id"].as_str().map(str::to_owned),
                    output: content_text(if raw["output"].is_null() {
                        &raw["error"]["message"]
                    } else {
                        &raw["output"]
                    }),
                    error: raw["status"] == "error",
                });
            }
        }
    }
    items
}

pub const SPINNER: [&str; 6] = ["·", "✢", "✳", "✶", "✻", "✽"];
pub fn spinner_frame(tick: usize) -> &'static str {
    let cycle = SPINNER.len() * 2 - 2;
    let i = tick % cycle;
    SPINNER[if i < SPINNER.len() { i } else { cycle - i }]
}
pub const VERBS: [(&str, &str); 24] = [
    ("Thinking", "Thought"),
    ("Pondering", "Pondered"),
    ("Working", "Worked"),
    ("Sniffing", "Sniffed"),
    ("Tracking", "Tracked"),
    ("Fetching", "Fetched"),
    ("Digging", "Dug"),
    ("Wandering", "Wandered"),
    ("Musing", "Mused"),
    ("Mulling", "Mulled"),
    ("Brewing", "Brewed"),
    ("Cooking", "Cooked"),
    ("Crafting", "Crafted"),
    ("Tinkering", "Tinkered"),
    ("Puzzling", "Puzzled"),
    ("Untangling", "Untangled"),
    ("Herding", "Herded"),
    ("Burrowing", "Burrowed"),
    ("Pacing", "Paced"),
    ("Considering", "Considered"),
    ("Noodling", "Noodled"),
    ("Churning", "Churned"),
    ("Scheming", "Schemed"),
    ("Wagging", "Wagged"),
];

/// Spinner verb with a moving shimmer, like Claude Code's status line.
pub fn shimmer(word: &str, tick: usize) -> Line {
    let chars: Vec<char> = word.chars().collect();
    let span = chars.len() + 6;
    let center = (tick % span) as isize - 3;
    let mut line = Line::new();
    for (i, c) in chars.iter().enumerate() {
        let near = (i as isize - center).abs() <= 1;
        line.add(
            Style::fg(if near { SHIMMER } else { ACCENT }),
            c.to_string(),
        );
    }
    line
}

pub fn duration(ms: u128) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

pub fn tokens(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn wraps_with_prefixes_and_never_overflows() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda 🐕🐕🐕";
        let lines = wrap(
            &[(Style::plain(), text.into())],
            20,
            &Line::of(Style::plain(), "⏺ "),
            &Line::of(Style::plain(), "  "),
        );
        assert!(lines.len() > 2);
        assert!(lines.iter().all(|l| l.width() <= 20));
        assert!(lines[0].text().starts_with("⏺ alpha"));
        assert!(lines[1].text().starts_with("  "));
    }
    #[test]
    fn mascot_is_nine_by_three() {
        let m = mascot();
        assert_eq!(m.len(), 3);
        assert!(m.iter().all(|l| l.width() == 9));
    }
    #[test]
    fn markdown_inline_styles() {
        let l = assistant("Use **bold** and `code` here", 80, true);
        assert_eq!(l[0].text(), "⏺ Use bold and code here");
        assert!(l[0].0.iter().any(|(s, t)| s.bold && t == "bold"));
        assert!(
            l[0].0
                .iter()
                .any(|(s, t)| s.fg == Some(CODE) && t == "code")
        );
        let c = assistant("```rust\nlet x = 1;\n```", 80, true);
        assert_eq!(c[0].text(), "⏺ let x = 1;");
    }
    #[test]
    fn tools_render_like_claude_code() {
        let cwd = Path::new("/repo");
        let t = tool(
            "1".into(),
            "Read",
            json!({"file_path":"/repo/src/a.rs"}),
            cwd,
        );
        assert_eq!((t.label.as_str(), t.arg.as_str()), ("Read", "src/a.rs"));
        assert_eq!(t.quiet, Some(Quiet::Read));
        let b = tool(
            "2".into(),
            "run_shell_command",
            json!({"command":"cargo test"}),
            cwd,
        );
        assert_eq!(
            tool_header(&b, Style::plain(), 80)[0].text(),
            "⏺ Bash(cargo test)"
        );
        let out = (1..=10)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let body = tool_body(&b, &out, false, false, 80);
        assert_eq!(body[0].text(), "  ⎿  1");
        assert_eq!(
            body.last().unwrap().text(),
            "     … +7 lines (ctrl+o to expand)"
        );
        let e = tool(
            "3".into(),
            "Edit",
            json!({"file_path":"/repo/a","old_string":"a\nb\nc","new_string":"a\nB\nc"}),
            cwd,
        );
        let body = tool_body(&e, "", false, false, 80);
        assert_eq!(
            body[0].text(),
            "  ⎿  Updated a with 1 addition and 1 removal"
        );
        assert_eq!(
            quiet_summary([2, 1, 0], true),
            "Read 2 files, searched for 1 pattern"
        );
    }
    #[test]
    fn interprets_each_provider() {
        let cwd = Path::new("/");
        let d = Decoded::default();
        let c = interpret(
            "claude",
            &json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}]}}),
            &d,
            cwd,
        );
        assert!(matches!(&c[0], Item::ToolUse(t) if t.label == "Bash"));
        let sub = interpret(
            "claude",
            &json!({"type":"assistant","parent_tool_use_id":"x","message":{"content":[{"type":"tool_use","id":"t","name":"Bash","input":{}}]}}),
            &d,
            cwd,
        );
        assert!(sub.is_empty());
        let x = interpret(
            "codex",
            &json!({"type":"item.completed","item":{"id":"i","type":"command_execution","command":"ls","aggregated_output":"a\n","exit_code":0,"status":"completed"}}),
            &d,
            cwd,
        );
        assert_eq!(x.len(), 2);
        let g = interpret(
            "gemini",
            &json!({"type":"tool_result","tool_id":"g","status":"error","output":"nope"}),
            &d,
            cwd,
        );
        assert!(matches!(&g[0], Item::ToolResult { error: true, .. }));
    }
    #[test]
    fn color_depths() {
        assert_eq!(ansi256(0xd7, 0x77, 0x57), 173);
    }
}
