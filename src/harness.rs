use crate::{
    Result, config, context, error, provider,
    runner::{self, Outcome, Spec},
    store::{Session, millis},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
pub trait Display {
    fn status(&mut self, text: &str) -> Result<()>;
    fn text(&mut self, text: &str) -> Result<()>;
}
pub fn turn(
    session: &mut Session,
    text: Option<&str>,
    config: &Value,
    write: bool,
    timeout: u64,
    cancel: &Arc<AtomicBool>,
    display: &mut dyn Display,
) -> Result<Outcome> {
    if let Some(text) = text.filter(|t| !t.is_empty()) {
        session.append(json!({"type":"user","text":text}))?;
        session.meta["pending"] = true.into();
        session.save()?;
    }
    if session.meta["pending"] != true {
        return Err(error("No unfinished task. Supply a prompt."));
    }
    let mut reasons = vec![];
    for item in config["order"].as_array().unwrap() {
        let id = item.as_str().unwrap();
        let p = &config["providers"][id];
        if p["enabled"] != true {
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok(Outcome::fail("cancelled", "Cancelled"));
        }
        let adapter = p["adapter"].as_str().unwrap();
        let route = config::identity(id, config);
        if let Some(native) = session.meta["providers"].get(id) {
            let old = native
                .get("route")
                .cloned()
                .unwrap_or_else(|| json!({"adapter":id,"command":id,"model":null}));
            if old != route {
                session.append(json!({"type":"route_reset","provider":id,"route":route}))?;
                session.meta["providers"]
                    .as_object_mut()
                    .unwrap()
                    .remove(id);
                session.meta["cooldowns"]
                    .as_object_mut()
                    .unwrap()
                    .remove(id);
                session.save()?;
            }
        }
        let until = session.meta["cooldowns"][id].as_u64().unwrap_or(0);
        if until > millis() {
            reasons.push(format!("{id}: cooling down until {until}"));
            continue;
        }
        let native = session.meta["providers"][id]["id"]
            .as_str()
            .map(str::to_owned);
        let prompt = context::prompt(session, id, config)?;
        let (command, args) = provider::invocation(id, native.as_deref(), config, write)?;
        session.append(json!({"type":"attempt_start","provider":id,"adapter":adapter,"model":route["model"],"route":route,"nativeId":native,"permission":if write{"write"}else{"read-only"}}))?;
        session.meta["providers"]
            .as_object_mut()
            .unwrap()
            .entry(id)
            .or_insert_with(|| json!({}))["route"] = route.clone();
        let label = format!(
            "{id}{}{}",
            p["model"]
                .as_str()
                .map(|m| format!(" · {m}"))
                .unwrap_or_default(),
            if native.is_some() { " · resuming" } else { "" }
        );
        display.status(&label)?;
        let cwd = session.meta["cwd"]
            .as_str()
            .ok_or_else(|| error("Invalid session working directory"))?
            .to_owned();
        let mut result = runner::run(
            Spec {
                adapter,
                command: &command,
                args: &args,
                cwd: Path::new(&cwd),
                prompt: &prompt,
                authentication: config["authentication"].as_str().unwrap_or("subscription"),
                timeout_ms: timeout,
            },
            cancel,
            |event, decoded| {
                session.append(
                    json!({"type":"provider_event","provider":id,"adapter":adapter,"event":event}),
                )?;
                if let Some(native) = decoded.native_id.as_deref().filter(|id| !id.is_empty()) {
                    if session.meta["providers"][id]["id"] != native {
                        session.append(json!({"type":"native_session","provider":id,"id":native,"route":route}))?;
                        session.meta["providers"][id]["id"] = json!(native);
                        session.meta["providers"][id]["route"] = route.clone();
                        session.save()?;
                    }
                }
                if let Some(activity) = &decoded.activity {
                    display.status(&format!("{id} · {activity}"))?;
                }
                if !decoded.text.is_empty() {
                    display.text(&decoded.text)?;
                }
                Ok(())
            },
        )?;
        result.provider = Some(id.into());
        session.append(
            json!({"type":"attempt_end","provider":id,"status":result.status,"error":result.error}),
        )?;
        if result.status == "success" {
            session.append(json!({"type":"complete","provider":id}))?;
            session.meta["providers"][id]["cursor"] = json!(session.events.len());
            session.meta["pending"] = false.into();
            session.meta["cooldowns"]
                .as_object_mut()
                .unwrap()
                .remove(id);
            session.save()?;
            return Ok(result);
        }
        if result.status == "cancelled" {
            session.save()?;
            return Ok(result);
        }
        if ["limit", "transient", "missing"].contains(&result.status.as_str()) {
            reasons.push(format!(
                "{id}: {} — {}",
                result.status,
                result
                    .error
                    .as_ref()
                    .map(|e| e
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| e.to_string()))
                    .unwrap_or_default()
            ));
            if result.status != "missing" {
                session.meta["cooldowns"][id] =
                    json!(millis().saturating_add(config["cooldownMs"].as_u64().unwrap()));
            }
            session.save()?;
            display.status(&format!("{id}: {}; trying next provider", result.status))?;
            continue;
        }
        session.save()?;
        return Ok(result);
    }
    session.save()?;
    Ok(Outcome::fail("exhausted", reasons.join("\n")))
}
