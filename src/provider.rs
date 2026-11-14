use crate::{Result, error};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::OnceLock;
#[derive(Debug, Default)]
pub struct Decoded {
    pub text: String,
    pub native_id: Option<String>,
    pub activity: Option<String>,
    pub error: Option<Value>,
    pub complete: bool,
    pub final_text: Option<String>,
}
fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_owned()
}
pub fn invocation(
    id: &str,
    native: Option<&str>,
    config: &Value,
    write: bool,
) -> Result<(String, Vec<String>)> {
    let p = &config["providers"][id];
    let adapter = p["adapter"].as_str().unwrap_or(id);
    let mut a: Vec<String> = vec![];
    let mut add = |items: &[&str]| a.extend(items.iter().map(|s| s.to_string()));
    match adapter {
        "claude" => {
            add(&[
                "-p",
                "--verbose",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
            ]);
            if let Some(id) = native {
                add(&["--resume", id]);
            }
            if write {
                add(&["--permission-mode", "acceptEdits"]);
                if let Some(t) = p["allowedTools"].as_str() {
                    add(&["--allowedTools", t]);
                }
            } else {
                add(&[
                    "--permission-mode",
                    "plan",
                    "--allowedTools",
                    "Read,Glob,Grep",
                    "--disallowedTools",
                    "Bash,Write,Edit,NotebookEdit",
                ]);
            }
        }
        "codex" => {
            add(&[
                "exec",
                "-c",
                if write {
                    "sandbox_mode=\"workspace-write\""
                } else {
                    "sandbox_mode=\"read-only\""
                },
                "-c",
                "approval_policy=\"never\"",
            ]);
            if let Some(id) = native {
                add(&["resume", id]);
            }
            add(&["--json"]);
        }
        "gemini" => {
            add(&[
                "--output-format",
                "stream-json",
                "--approval-mode",
                if write { "auto_edit" } else { "plan" },
            ]);
            if let Some(id) = native {
                add(&["--resume", id]);
            }
            add(&["-p", "Continue the task supplied on standard input."]);
        }
        _ => return Err(error(format!("Unknown adapter: {adapter}"))),
    }
    if let Some(model) = p["model"].as_str() {
        a.extend(["--model".into(), model.into()]);
    }
    if adapter == "codex" {
        a.push("-".into());
    }
    Ok((p["command"].as_str().unwrap_or(adapter).into(), a))
}
pub fn decode(adapter: &str, e: &Value) -> Decoded {
    let mut d = Decoded::default();
    let t = e["type"].as_str().unwrap_or("");
    match adapter {
        "claude" => {
            if t == "system" && e["subtype"] == "init" {
                d.native_id = e["session_id"].as_str().map(str::to_owned);
            }
            if t == "stream_event" {
                if e["event"]["type"] == "content_block_delta"
                    && e["event"]["delta"]["type"] == "text_delta"
                {
                    d.text = s(&e["event"]["delta"]["text"]);
                }
                if e["event"]["type"] == "content_block_start"
                    && e["event"]["content_block"]["type"] == "tool_use"
                {
                    d.activity = e["event"]["content_block"]["name"]
                        .as_str()
                        .map(str::to_owned);
                }
            }
            if t == "assistant" && e.get("error").is_some_and(|v| !v.is_null() && v != false) {
                d.error =
                    Some(json!({"code":e["error"],"message":e["message"]["content"].to_string()}));
            }
            if t == "result" {
                d.complete = e["subtype"] == "success" && e["is_error"] != true;
                d.final_text = e["result"].as_str().map(str::to_owned);
                if !d.complete {
                    let message = e["errors"]
                        .as_array()
                        .map(|a| a.iter().map(s).collect::<Vec<_>>().join("\n"))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| {
                            e["result"].as_str().unwrap_or("Claude run failed").into()
                        });
                    d.error = Some(json!({"code":e["subtype"],"message":message}));
                }
            }
        }
        "codex" => {
            if t == "thread.started" {
                d.native_id = e["thread_id"].as_str().map(str::to_owned);
            }
            if t == "item.completed" && e["item"]["type"] == "agent_message" {
                d.text = s(&e["item"]["text"]);
            }
            if t == "item.started" && e["item"]["type"] != "reasoning" {
                d.activity = e["item"]["type"].as_str().map(str::to_owned);
            }
            d.complete = t == "turn.completed";
            if t == "turn.failed" || t == "error" {
                d.error=Some(e.get("error").filter(|v|!v.is_null()).cloned().unwrap_or_else(||json!({"message":e["message"].as_str().unwrap_or("Codex run failed")})));
            }
        }
        "gemini" => {
            if t == "init" {
                d.native_id = e["session_id"].as_str().map(str::to_owned);
            }
            if t == "message" && e["role"] == "assistant" {
                d.text = s(&e["content"]);
            }
            if t == "tool_use" {
                d.activity = e["tool_name"].as_str().map(str::to_owned);
            }
            if t == "result" {
                d.complete = e["status"] == "success";
                if !d.complete {
                    d.error=Some(e.get("error").filter(|v|!v.is_null()).cloned().unwrap_or_else(||json!({"message":e["message"].as_str().unwrap_or("Gemini run failed")})));
                }
            }
            if t == "error" && e["severity"] != "warning" {
                d.error = Some(
                    e.get("error")
                        .filter(|v| !v.is_null())
                        .cloned()
                        .unwrap_or_else(|| json!({"code":e["code"],"message":e["message"]})),
                );
            }
        }
        _ => {}
    }
    d
}
pub fn classify(v: &Value) -> &'static str {
    static LIMIT: OnceLock<Regex> = OnceLock::new();
    static TRANSIENT: OnceLock<Regex> = OnceLock::new();
    let text = v
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string());
    if LIMIT.get_or_init(||Regex::new(r"(?i)rate[_ -]?limit|usage[_ -]?limit|quota[_ -]?(?:exceeded|exhausted)|resource_exhausted|insufficient_quota|too many requests|hit your limit|out of extra usage|\b429\b|credit balance is too low|(?:exceeded|exhausted).*(?:quota|usage|capacity)").unwrap()).is_match(&text){"limit"}
    else if TRANSIENT.get_or_init(||Regex::new(r"(?i)overloaded|\b(?:500|502|503|504|529)\b|service unavailable|ECONNRESET|ETIMEDOUT|ENETUNREACH|EAI_AGAIN|fetch failed").unwrap()).is_match(&text){"transient"}else{"fatal"}
}
pub fn portable(adapter: &str, e: &Value) -> Option<Value> {
    let t = e["type"].as_str().unwrap_or("");
    match adapter {
        "claude" => {
            if t == "assistant" || t == "user" {
                let content = &e["message"]["content"];
                if content.is_string() {
                    return Some(json!({"role":t,"content":content}));
                }
                if let Some(a) = content.as_array() {
                    let b: Vec<_> = a
                        .iter()
                        .filter(|b| {
                            ["text", "tool_use", "tool_result"]
                                .contains(&b["type"].as_str().unwrap_or(""))
                        })
                        .collect();
                    if !b.is_empty() {
                        return Some(json!({"role":t,"content":b}));
                    }
                }
            }
            if t == "stream_event" && e["event"]["delta"]["type"] == "text_delta" {
                return Some(
                    json!({"role":"assistant_delta","content":e["event"]["delta"]["text"]}),
                );
            }
        }
        "codex" => {
            if t == "item.completed" && e["item"]["type"] != "reasoning" {
                return Some(json!({"role":"assistant_activity","content":e["item"]}));
            }
            if t == "item.started" {
                return Some(json!({"role":"tool_started","content":e["item"]}));
            }
        }
        "gemini" if ["message", "tool_use", "tool_result"].contains(&t) && e["role"] != "user" => {
            return Some(json!({"role":t,"content":e}));
        }
        _ => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_classification() {
        for s in [
            "429",
            "rate_limit_error",
            "usage_limit_reached",
            "RESOURCE_EXHAUSTED",
            "credit balance is too low",
            "quota exceeded",
            "You have exhausted your capacity",
        ] {
            assert_eq!(classify(&json!(s)), "limit");
        }
        assert_eq!(classify(&json!("503 service unavailable")), "transient");
        assert_eq!(classify(&json!("Invalid API key")), "fatal");
    }
    #[test]
    fn protocol_not_task_text() {
        assert!(decode("claude",&json!({"type":"assistant","message":{"content":[{"type":"text","text":"Explain HTTP 429"}]}})).error.is_none());
        assert!(decode("codex",&json!({"type":"item.completed","item":{"type":"command_execution","aggregated_output":"429"}})).error.is_none());
        assert!(
            decode(
                "gemini",
                &json!({"type":"error","severity":"warning","message":"warning"})
            )
            .error
            .is_none()
        );
    }
    #[test]
    fn private_reasoning_not_portable() {
        assert!(
            portable(
                "codex",
                &json!({"type":"item.completed","item":{"type":"reasoning","text":"private"}})
            )
            .is_none()
        );
        assert!(portable("claude",&json!({"type":"assistant","message":{"content":[{"type":"thinking","thinking":"private"}]}})).is_none());
    }
    #[test]
    fn permissions_and_models() {
        let c = crate::config::normalize(
            json!({"providers":{"fast":{"adapter":"codex","model":"chosen"}}}),
        )
        .unwrap();
        for id in ["claude", "codex", "gemini", "fast"] {
            for write in [false, true] {
                let (_, args) = invocation(id, Some("native"), &c, write).unwrap();
                let joined = args.join(" ");
                assert!(!joined.contains("dangerously"));
                assert!(!joined.contains("yolo"));
                assert!(args.contains(&"native".into()));
            }
        }
        assert!(
            invocation("fast", None, &c, false)
                .unwrap()
                .1
                .contains(&"chosen".into())
        );
    }
}
