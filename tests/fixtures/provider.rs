use serde_json::{Value, json};
use std::{
    env, fs,
    io::{self, Read, Write},
    time::Duration,
};
fn emit(v: Value) {
    println!("{v}");
    io::stdout().flush().unwrap();
}
fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    if args == ["--version"] {
        println!("lennon test provider 1");
        return;
    }
    let adapter = if args.iter().any(|a| a == "exec") {
        "codex"
    } else if args.iter().any(|a| a == "--verbose") {
        "claude"
    } else {
        "gemini"
    };
    let scenario = env::var("LENNON_TEST_SCENARIO").unwrap_or("success".into());
    if scenario == "descendant" {
        let exe = env::current_exe().unwrap();
        let mut child = std::process::Command::new(exe)
            .env("LENNON_TEST_SCENARIO", "hang")
            .spawn()
            .unwrap();
        std::thread::spawn(move || child.wait());
    }
    if ["hang", "descendant", "ignore-term"].contains(&scenario.as_str()) {
        if scenario == "ignore-term" {
            unsafe {
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
            }
        }
        if let Ok(path) = env::var("LENNON_TEST_PID") {
            fs::write(path, std::process::id().to_string()).unwrap();
        }
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    if scenario == "quota-hang" {
        emit(json!({"type":"error","message":"rate_limit_error 429"}));
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    if scenario == "oversized" {
        print!("{}", "x".repeat(17 * 1024 * 1024));
        io::stdout().flush().unwrap();
        return;
    }
    let mut prompt = String::new();
    io::stdin().read_to_string(&mut prompt).unwrap();
    let model = args
        .windows(2)
        .find(|a| a[0] == "--model")
        .map(|a| a[1].clone())
        .unwrap_or("default".into());
    if let Ok(path) = env::var("LENNON_TEST_CAPTURE") {
        let row = json!({"adapter":adapter,"args":args,"prompt":prompt,"model":model,"apiKeyPresent":env::var_os("OPENAI_API_KEY").is_some()});
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(f, "{row}").unwrap();
    }
    if scenario == "bad-json" {
        println!("not a protocol event");
        return;
    }
    if scenario == "auth" {
        eprintln!("Invalid API key or login required");
        std::process::exit(1);
    }
    if scenario == "permission" {
        emit(json!({"type":"error","message":"Permission denied"}));
        std::process::exit(1);
    }
    if scenario == "missing-complete" {
        return;
    }
    let limited = scenario == "all-limited"
        || (scenario == "failover" && adapter == "claude")
        || (scenario == "model-fallback" && model == "fast");
    let native = format!("{adapter}-{model}-native");
    match adapter {
        "claude" => {
            emit(json!({"type":"system","subtype":"init","session_id":native}));
            emit(
                json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file":"app.rs"}}]}}),
            );
            emit(
                json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"app.rs already updated"}]}}),
            );
            emit(
                json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Partial 🐕 finding"}}}),
            );
            emit(
                json!({"type":"result","subtype":if limited{"error_during_execution"}else{"success"},"is_error":limited,"result":if limited{"rate_limit_error 429"}else{"OK 🐕"}}),
            );
        }
        "codex" => {
            emit(json!({"type":"thread.started","thread_id":native}));
            if limited {
                emit(json!({"type":"turn.failed","error":{"message":"usage_limit_reached"}}));
            } else {
                if scenario == "recovered-error" {
                    emit(json!({"type":"error","message":"temporary network failure"}));
                }
                if scenario == "utf8" {
                    let text=json!({"type":"item.completed","item":{"type":"agent_message","text":"OK 🐕"}}).to_string()+"\n";
                    for b in text.as_bytes() {
                        io::stdout().write_all(&[*b]).unwrap();
                        io::stdout().flush().unwrap();
                    }
                } else {
                    emit(
                        json!({"type":"item.completed","item":{"type":"agent_message","text":"OK 🐕"}}),
                    );
                }
                emit(json!({"type":"turn.completed"}));
            }
        }
        _ => {
            emit(json!({"type":"init","session_id":native}));
            if limited {
                emit(
                    json!({"type":"result","status":"error","error":{"message":"RESOURCE_EXHAUSTED"}}),
                );
            } else {
                emit(json!({"type":"error","severity":"warning","message":"Nonfatal warning"}));
                emit(json!({"type":"tool_use","tool_name":"read_file"}));
                emit(json!({"type":"tool_result","output":"Constraints retained"}));
                emit(json!({"type":"message","role":"assistant","content":"OK 🐕"}));
                emit(json!({"type":"result","status":"success"}));
            }
        }
    }
}
