use serde_json::{Value, json};
use std::{env, fs::{self, OpenOptions}, io::{self, Read, Write}, path::Path, time::{Instant, SystemTime, UNIX_EPOCH}};
use std::os::unix::fs::OpenOptionsExt;

fn main() {
    let a: Vec<String> = env::args().collect();
    if a.get(1).map(String::as_str) == Some("--help") {
        println!("lennon benchmark probe"); return;
    }
    let executable = Path::new(&a[0]).file_name().unwrap().to_string_lossy();
    if executable.starts_with("provider-") {
        mock_provider(executable.ends_with("claude")); return;
    }
    let mode = a.get(1).expect("mode");
    let start = Instant::now();
    let text = fs::read_to_string(&a[2]).unwrap();
    let mut count = 0u64;
    let mut bytes = 0u64;
    let mut text_bytes = 0u64;
    let mut tools = 0u64;
    let mut limits = 0u64;
    let mut file = if mode == "journal-open" {
        Some(OpenOptions::new().append(true).create(true).mode(0o600).open(&a[3]).unwrap())
    } else { None };
    for line in text.lines() {
        let event: Value = serde_json::from_str(line).unwrap();
        count += 1;
        if let Some(s) = event.pointer("/event/delta/text").and_then(Value::as_str) { text_bytes += s.len() as u64; }
        if event["type"] == "assistant" { tools += 1; }
        if event["type"] == "error" && event["message"].as_str().unwrap_or("").contains("rate_limit") { limits += 1; }
        // Serialize by reference; constructing another Value here needlessly
        // deep-clones the event and biases the comparison against Rust.
        let serialized = format!("{{\"time\":\"2026-11-14T00:00:00.000Z\",\"type\":\"provider_event\",\"provider\":\"claude\",\"event\":{}}}\n", serde_json::to_string(&event).unwrap());
        bytes += serialized.len() as u64;
        if mode == "journal" {
            let mut f = OpenOptions::new().append(true).create(true).mode(0o600).open(&a[3]).unwrap();
            f.write_all(serialized.as_bytes()).unwrap(); f.sync_all().unwrap();
        } else if let Some(ref mut f) = file {
            f.write_all(serialized.as_bytes()).unwrap(); f.sync_all().unwrap();
        }
    }
    println!("{}", json!({"events":count,"bytes":bytes,"textBytes":text_bytes,"tools":tools,"limits":limits,"workMs":start.elapsed().as_secs_f64()*1000.0}));
}

fn stamp(name: &str) {
    let root = env::var("LENNON_BENCH_MARKS").unwrap();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    fs::write(Path::new(&root).join(name), now.to_string()).unwrap();
}

fn mock_provider(claude: bool) {
    stamp(if claude { "claude-start" } else { "codex-start" });
    let mut input = String::new(); io::stdin().read_to_string(&mut input).unwrap();
    let mut out = io::stdout().lock();
    if claude {
        writeln!(out,"{}",json!({"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000001"})).unwrap();
        let n: usize = env::var("LENNON_BENCH_EVENTS").unwrap_or("100".into()).parse().unwrap();
        for _ in 0..n {
            writeln!(out,"{}",json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hello 🐕 "}}})).unwrap();
        }
        stamp("quota-emitted");
        writeln!(out,"{}",json!({"type":"result","subtype":"error_during_execution","is_error":true,"result":"rate_limit_error 429"})).unwrap();
        out.flush().unwrap();
    } else {
        assert!(input.contains("benchmark task"), "user request must reach fallback");
        assert!(input.contains("hello"), "streamed context must reach fallback");
        stamp("fallback-first-event");
        writeln!(out,"{}",json!({"type":"thread.started","thread_id":"00000000-0000-4000-8000-000000000002"})).unwrap();
        writeln!(out,"{}",json!({"type":"item.completed","item":{"type":"agent_message","text":"BENCH_DONE"}})).unwrap();
        writeln!(out,"{}",json!({"type":"turn.completed"})).unwrap();
    }
}
