#![cfg(feature = "test-fixtures")]
use lennon::{
    config, context,
    store::{self, Session},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
use uuid::Uuid;
struct Fixture {
    dir: PathBuf,
    config: PathBuf,
    capture: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("lennon-rust-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.json");
        let capture = dir.join("capture.jsonl");
        let command = env!("CARGO_BIN_EXE_lennon-provider-fixture");
        fs::write(&config,json!({"order":["claude","codex","gemini"],"providers":{"claude":{"command":command},"codex":{"command":command},"gemini":{"command":command}}}).to_string()).unwrap();
        Self {
            dir,
            config,
            capture,
        }
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_lennon"));
        c.current_dir(&self.dir)
            .env("LENNON_CONFIG", &self.config)
            .env("LENNON_STATE_DIR", self.dir.join("state"))
            .env("LENNON_TEST_CAPTURE", &self.capture)
            .stdin(Stdio::null());
        c
    }
    fn run(&self, args: &[&str], scenario: &str) -> Output {
        self.command()
            .args(args)
            .env("LENNON_TEST_SCENARIO", scenario)
            .output()
            .unwrap()
    }
    fn records(&self) -> Vec<Value> {
        fs::read_to_string(&self.capture)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn events(out: &Output) -> Vec<Value> {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect()
    }
    fn id(out: &Output) -> String {
        Self::events(out)[0]["session"].as_str().unwrap().to_owned()
    }
    fn result(out: &Output) -> Value {
        Self::events(out)
            .into_iter()
            .find(|e| e["type"] == "result")
            .unwrap_or_else(|| panic!("No result: {:?}", out))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn success(o: &Output) {
    assert!(
        o.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}
#[test]
fn cli_help_version_and_config() {
    let f = Fixture::new();
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec!["version"],
        vec!["config"],
        vec!["providers"],
        vec!["doctor"],
    ] {
        success(&f.run(&args, "success"));
    }
    let c: Value = serde_json::from_slice(
        &f.run(
            &[
                "--order",
                "codex,claude",
                "--model",
                "claude=sonnet",
                "config",
            ],
            "success",
        )
        .stdout,
    )
    .unwrap();
    assert_eq!(c["order"], json!(["codex", "claude"]));
    assert_eq!(c["providers"]["claude"]["model"], "sonnet");
}
#[test]
fn quota_failover_preserves_tools_and_constraints() {
    let f = Fixture::new();
    let o = f.run(
        &["--json", "--write", "-p", "Fix tests; never deploy"],
        "failover",
    );
    success(&o);
    assert_eq!(Fixture::result(&o)["provider"], "codex");
    let calls = f.records();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["adapter"], "claude");
    assert_eq!(calls[1]["adapter"], "codex");
    let prompt = calls[1]["prompt"].as_str().unwrap();
    assert!(prompt.contains("never deploy"));
    assert!(prompt.contains("app.rs already updated"));
    assert!(prompt.contains("side effects may already"));
    let meta: Value = serde_json::from_slice(
        &fs::read(
            f.dir
                .join("state/sessions")
                .join(Fixture::id(&o))
                .join("session.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(meta["pending"], false);
    assert!(meta["cooldowns"]["claude"].as_u64().unwrap() > store::millis());
}
#[test]
fn all_providers_limited_then_cooldown_skip() {
    let f = Fixture::new();
    let first = f.run(&["--json", "-p", "Unfinished task"], "all-limited");
    assert!(!first.status.success());
    assert_eq!(Fixture::result(&first)["status"], "exhausted");
    assert_eq!(f.records().len(), 3);
    let id = Fixture::id(&first);
    let again = f.run(&["--json", "--resume", &id], "success");
    assert_eq!(Fixture::result(&again)["status"], "exhausted");
    assert_eq!(f.records().len(), 3);
    success(&f.run(&["--json", "--resume", &id, "--retry"], "success"));
}
#[test]
fn auth_and_permission_errors_do_not_fail_over() {
    for scenario in ["auth", "permission"] {
        let f = Fixture::new();
        let o = f.run(&["--json", "-p", "Task"], scenario);
        assert_eq!(Fixture::result(&o)["status"], "fatal");
        assert_eq!(f.records().len(), 1);
    }
}
#[test]
fn native_resume_uses_only_new_context_and_cross_provider_updates() {
    let f = Fixture::new();
    let first = f.run(
        &["--json", "--provider", "codex", "-p", "First constraint"],
        "success",
    );
    success(&first);
    let id = Fixture::id(&first);
    success(&f.run(
        &[
            "--json",
            "--resume",
            &id,
            "--provider",
            "codex",
            "-p",
            "Second instruction",
        ],
        "success",
    ));
    let calls = f.records();
    assert!(
        calls[1]["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "resume")
    );
    assert!(
        calls[1]["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "codex-default-native")
    );
    success(&f.run(
        &[
            "--json",
            "--resume",
            &id,
            "--provider",
            "gemini",
            "-p",
            "Intervening Gemini work",
        ],
        "success",
    ));
    success(&f.run(
        &[
            "--json",
            "--resume",
            &id,
            "--provider",
            "codex",
            "-p",
            "Return to Codex",
        ],
        "success",
    ));
    assert!(
        f.records()[3]["prompt"]
            .as_str()
            .unwrap()
            .contains("Intervening Gemini work")
    );
}
#[test]
fn named_models_independent_and_disabled_entries_skipped() {
    let f = Fixture::new();
    let exe = env!("CARGO_BIN_EXE_lennon-provider-fixture");
    fs::write(&f.config,json!({"order":["off","fast","deep"],"providers":{"off":{"adapter":"gemini","enabled":false,"command":exe},"fast":{"adapter":"codex","model":"fast","command":exe},"deep":{"adapter":"codex","model":"deep","command":exe}}}).to_string()).unwrap();
    let o = f.run(&["--json", "-p", "Task"], "model-fallback");
    success(&o);
    assert_eq!(Fixture::result(&o)["provider"], "deep");
    let calls = f.records();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["model"], "fast");
    assert_eq!(calls[1]["model"], "deep");
    let meta: Value = serde_json::from_slice(
        &fs::read(
            f.dir
                .join("state/sessions")
                .join(Fixture::id(&o))
                .join("session.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(meta["providers"]["fast"]["id"], "codex-fast-native");
    assert_eq!(meta["providers"]["deep"]["id"], "codex-deep-native");
}
#[test]
fn model_change_replays_complete_context_then_resumes_new_native_session() {
    let f = Fixture::new();
    let o = f.run(
        &[
            "--json",
            "--provider",
            "codex",
            "--model",
            "codex=a",
            "-p",
            "Keep the original constraint",
        ],
        "success",
    );
    success(&o);
    let id = Fixture::id(&o);
    success(&f.run(
        &[
            "--json",
            "--resume",
            &id,
            "--provider",
            "codex",
            "--model",
            "codex=b",
            "-p",
            "Continue with b",
        ],
        "success",
    ));
    let calls = f.records();
    assert!(
        !calls[1]["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "resume")
    );
    assert!(calls[1]["prompt"].as_str().unwrap().contains("OK 🐕"));
    success(&f.run(
        &[
            "--json",
            "--resume",
            &id,
            "--provider",
            "codex",
            "--model",
            "codex=b",
            "-p",
            "Continue again",
        ],
        "success",
    ));
    assert!(
        f.records()[2]["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "codex-b-native")
    );
}
#[test]
fn configured_models_and_api_key_policy_reach_real_child() {
    let f = Fixture::new();
    let o = f
        .command()
        .args([
            "--json",
            "--provider",
            "codex",
            "--model",
            "codex=chosen",
            "-p",
            "Task",
        ])
        .env("OPENAI_API_KEY", "test-sentinel")
        .output()
        .unwrap();
    success(&o);
    assert_eq!(f.records()[0]["model"], "chosen");
    assert_eq!(f.records()[0]["apiKeyPresent"], false);
    let mut c: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    c["authentication"] = "environment".into();
    fs::write(&f.config, c.to_string()).unwrap();
    success(
        &f.command()
            .args(["--json", "--provider", "codex", "-p", "Task"])
            .env("OPENAI_API_KEY", "test-sentinel")
            .output()
            .unwrap(),
    );
    assert_eq!(f.records()[1]["apiKeyPresent"], true);
}
#[test]
fn config_editing_validation_atomicity_and_overrides() {
    let f = Fixture::new();
    for args in [
        vec!["config", "set", "order", "codex,claude"],
        vec!["config", "set", "providers.claude.model", "sonnet"],
        vec!["config", "set", "providers.gemini.enabled", "false"],
        vec![
            "config",
            "set",
            "providers.quick",
            "{\"adapter\":\"claude\",\"model\":\"sonnet\"}",
        ],
    ] {
        success(&f.run(&args, "success"));
    }
    let before = fs::read(&f.config).unwrap();
    for args in [
        vec!["config", "set", "order", "unknown"],
        vec!["config", "set", "providers.__proto__.adapter", "codex"],
        vec!["config", "set", "providers.claude.modle", "bad"],
        vec!["config", "init"],
    ] {
        assert!(!f.run(&args, "success").status.success());
        assert_eq!(fs::read(&f.config).unwrap(), before);
    }
    success(&f.run(&["config", "unset", "providers.claude.model"], "success"));
    let p = f.dir.join("new.json");
    success(&f.run(
        &["--config", p.to_str().unwrap(), "config", "init"],
        "success",
    ));
    let missing = f.dir.join("missing.json");
    assert!(
        !f.run(
            &["--config", missing.to_str().unwrap(), "config"],
            "success"
        )
        .status
        .success()
    );
    assert!(!missing.exists());
}
#[test]
fn invalid_cli_never_launches_provider() {
    let f = Fixture::new();
    for args in [
        vec!["--order", "unknown", "-p", "Task"],
        vec!["--provider", "codex", "--order", "claude"],
        vec!["--model", "codex=", "-p", "Task"],
        vec!["--timeout", "-1", "-p", "Task"],
        vec!["--resume", "../escape", "-p", "Task"],
    ] {
        assert!(!f.run(&args, "success").status.success());
    }
    assert!(f.records().is_empty());
}
#[test]
fn missing_executable_falls_through() {
    let f = Fixture::new();
    let mut c: Value = serde_json::from_slice(&fs::read(&f.config).unwrap()).unwrap();
    c["providers"]["claude"]["command"] = json!(f.dir.join("absent"));
    fs::write(&f.config, c.to_string()).unwrap();
    let o = f.run(&["--json", "-p", "Task"], "success");
    success(&o);
    assert_eq!(Fixture::result(&o)["provider"], "codex");
}
#[test]
fn utf8_framing_and_recovered_errors() {
    for scenario in ["utf8", "recovered-error"] {
        let f = Fixture::new();
        let o = f.run(&["--json", "--provider", "codex", "-p", "Task"], scenario);
        success(&o);
        assert!(String::from_utf8_lossy(&o.stdout).contains("OK 🐕"));
    }
}
#[test]
fn gemini_warning_tool_activity_and_native_resume() {
    let f = Fixture::new();
    let o = f.run(&["--json", "--provider", "gemini", "-p", "Task"], "success");
    success(&o);
    let id = Fixture::id(&o);
    success(&f.run(
        &[
            "--json",
            "--provider",
            "gemini",
            "--resume",
            &id,
            "-p",
            "Next",
        ],
        "success",
    ));
    assert!(
        f.records()[1]["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "gemini-default-native")
    );
    assert!(String::from_utf8_lossy(&o.stdout).contains("read_file"));
}
#[test]
fn no_success_marker_is_fatal_and_oversized_events_are_bounded() {
    for scenario in ["missing-complete", "bad-json", "oversized"] {
        let f = Fixture::new();
        let o = f.run(&["--json", "--provider", "codex", "-p", "Task"], scenario);
        assert_eq!(Fixture::result(&o)["status"], "fatal");
    }
}
#[test]
fn quota_error_stops_native_retry_loop() {
    let f = Fixture::new();
    let start = Instant::now();
    let o = f.run(
        &["--json", "--provider", "codex", "-p", "Task"],
        "quota-hang",
    );
    assert_eq!(Fixture::result(&o)["status"], "exhausted");
    assert!(start.elapsed() < Duration::from_secs(4));
}
#[test]
fn timeout_is_not_a_failover_and_forces_uncooperative_child_exit() {
    for scenario in ["hang", "ignore-term", "descendant"] {
        let f = Fixture::new();
        let start = Instant::now();
        let o = f.run(&["--json", "--timeout", "150", "-p", "Task"], scenario);
        assert_eq!(Fixture::result(&o)["status"], "timeout");
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(f.records().is_empty());
    }
}
#[test]
fn sigint_saves_pending_task_and_kills_child() {
    let f = Fixture::new();
    let pidfile = f.dir.join("pid");
    let child = f
        .command()
        .args(["--json", "-p", "Task"])
        .env("LENNON_TEST_SCENARIO", "hang")
        .env("LENNON_TEST_PID", &pidfile)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !pidfile.exists() {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = fs::read_to_string(&pidfile)
        .unwrap()
        .parse::<i32>()
        .unwrap();
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(130));
    assert_eq!(Fixture::result(&o)["status"], "cancelled");
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    let sessions = store::list(&f.dir.join("state")).unwrap();
    assert_eq!(sessions[0]["pending"], true);
}
#[test]
fn piped_context_is_preserved_and_not_shell_interpreted() {
    let f = Fixture::new();
    let mut child = f
        .command()
        .args(["--json", "--provider", "codex", "-p", "Review this"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"Literal $(touch forbidden) and `commands`\n")
        .unwrap();
    success(&child.wait_with_output().unwrap());
    assert!(
        f.records()[0]["prompt"]
            .as_str()
            .unwrap()
            .contains("Literal $(touch forbidden)")
    );
    assert!(!f.dir.join("forbidden").exists());
}
#[test]
fn journal_recovers_js_format_torn_record_and_native_route() {
    let f = Fixture::new();
    let mut s = Session::create(&f.dir, &f.dir).unwrap();
    let id = s.meta["id"].as_str().unwrap().to_owned();
    let route = json!({"adapter":"codex","command":"codex","model":null});
    let journal = format!(
        "{}\n{}\n{}\n{{torn",
        json!({"time":"2026-11-14T00:00:00.000Z","type":"user","text":"JS unfinished task"}),
        json!({"type":"attempt_start","provider":"codex","route":route}),
        json!({"type":"native_session","provider":"codex","id":"legacy-js-thread","route":route})
    );
    fs::write(&s.journal, journal).unwrap();
    s.acquire().unwrap();
    assert_eq!(s.meta["pending"], true);
    assert_eq!(s.meta["providers"]["codex"]["id"], "legacy-js-thread");
    assert_eq!(s.events.len(), 3);
    s.append(json!({"type":"complete","provider":"codex"}))
        .unwrap();
    s.release();
    let mut recovered = Session::open(&f.dir, &id).unwrap();
    recovered.acquire().unwrap();
    assert_eq!(recovered.meta["pending"], false);
    assert_eq!(recovered.meta["providers"]["codex"]["cursor"], 4);
}
#[test]
fn exclusive_lock_stale_recovery_and_corruption_cleanup() {
    let f = Fixture::new();
    let mut a = Session::create(&f.dir, &f.dir).unwrap();
    let id = a.meta["id"].as_str().unwrap().to_owned();
    a.acquire().unwrap();
    let mut b = Session::open(&f.dir, &id).unwrap();
    assert!(b.acquire().is_err());
    a.release();
    fs::write(a.dir.join("lock"), "2147483647").unwrap();
    b.acquire().unwrap();
    b.release();
    fs::write(&a.journal, b"not-json\n").unwrap();
    assert!(b.acquire().is_err());
    assert!(!a.dir.join("lock").exists());
}
#[test]
fn bounded_handoff_keeps_full_archive_and_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let mut s = Session::create(&f.dir, &f.dir).unwrap();
    s.acquire().unwrap();
    s.append(json!({"type":"user","text":"Never deploy"}))
        .unwrap();
    s.append(json!({"type":"provider_event","provider":"codex","event":{"type":"item.completed","item":{"type":"command_execution","aggregated_output":"x".repeat(20000)}}})).unwrap();
    let c = config::normalize(json!({"handoffChars":1000})).unwrap();
    let p = context::prompt(&s, "gemini", &c).unwrap();
    assert!(p.contains("Never deploy"));
    assert!(p.contains("Read the FULL handoff"));
    let archive = s.dir.join("handoff-gemini.json");
    assert!(fs::metadata(&archive).unwrap().len() > 20000);
    assert_eq!(
        fs::metadata(archive).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
#[test]
fn continue_finds_latest_session_and_completed_retry_is_rejected() {
    let f = Fixture::new();
    let o = f.run(&["--json", "--provider", "codex", "-p", "First"], "success");
    success(&o);
    success(&f.run(
        &[
            "--json",
            "--continue",
            "--provider",
            "codex",
            "-p",
            "Second",
        ],
        "success",
    ));
    let id = Fixture::id(&o);
    let again = f.run(&["--json", "--resume", &id], "success");
    assert!(!again.status.success());
    assert!(String::from_utf8_lossy(&again.stderr).contains("No unfinished task"));
}
