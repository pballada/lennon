use crate::{
    Result, error,
    store::{atomic_json, private_dir, private_options},
};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};
pub const ADAPTERS: [&str; 3] = ["claude", "codex", "gemini"];

pub fn home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
pub fn state_root() -> PathBuf {
    env::var_os("LENNON_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".local/state"))
                .join("lennon")
        })
}
pub fn config_path(explicit: Option<&str>) -> PathBuf {
    explicit
        .map(PathBuf::from)
        .or_else(|| env::var_os("LENNON_CONFIG").map(PathBuf::from))
        .unwrap_or_else(|| {
            env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home().join(".config"))
                .join("lennon/config.json")
        })
}
pub fn read(path: &Path, required: bool) -> Result<Value> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| error(format!("Invalid config {}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => Ok(json!({})),
        Err(e) => Err(error(format!("Invalid config {}: {e}", path.display()))),
    }
}
pub fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
        && !["constructor", "prototype", "__proto__"].contains(&id)
}
fn nonempty(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| !s.trim().is_empty() && !s.contains('\0'))
}
pub fn normalize(user: Value) -> Result<Value> {
    let obj = user
        .as_object()
        .ok_or_else(|| error("config must be an object"))?;
    for k in obj.keys() {
        if ![
            "order",
            "cooldownMs",
            "handoffChars",
            "authentication",
            "providers",
        ]
        .contains(&k.as_str())
        {
            return Err(error(format!("Unknown config setting: {k}")));
        }
    }
    let mut c = json!({"order":ADAPTERS,"cooldownMs":300000,"handoffChars":120000,"authentication":"subscription","providers":{}});
    for (k, v) in obj {
        c[k] = v.clone();
    }
    if ![Some("subscription"), Some("environment")].contains(&c["authentication"].as_str()) {
        return Err(error("authentication must be subscription or environment"));
    }
    let supplied = c["providers"]
        .as_object()
        .ok_or_else(|| error("providers must be an object"))?;
    let mut providers = serde_json::Map::new();
    for id in ADAPTERS {
        providers.insert(id.into(), json!({"adapter":id,"enabled":true}));
    }
    for (id, p) in supplied {
        if !valid_id(id) {
            return Err(error(format!("Invalid provider ID: {id}")));
        }
        let p = p
            .as_object()
            .ok_or_else(|| error(format!("providers.{id} must be an object")))?;
        let mut entry = providers
            .get(id)
            .cloned()
            .unwrap_or_else(|| json!({"enabled":true}));
        for (k, v) in p {
            if !["adapter", "command", "model", "enabled", "allowedTools"].contains(&k.as_str()) {
                return Err(error(format!("Unknown providers.{id} setting: {k}")));
            }
            entry[k] = v.clone();
        }
        if !entry["adapter"]
            .as_str()
            .is_some_and(|s| ADAPTERS.contains(&s))
        {
            return Err(error(format!(
                "providers.{id}.adapter must be claude, codex, or gemini"
            )));
        }
        if !entry["enabled"].is_boolean() {
            return Err(error(format!("providers.{id}.enabled must be boolean")));
        }
        for key in ["command", "model"] {
            if let Some(v) = entry.get(key) {
                if !nonempty(v)
                    || (key == "model" && v.as_str().is_some_and(|s| s.starts_with('-')))
                {
                    return Err(error(format!("Invalid providers.{id}.{key}")));
                }
            }
        }
        if entry.get("allowedTools").is_some_and(|v| !v.is_string()) {
            return Err(error(format!("Invalid providers.{id}.allowedTools")));
        }
        providers.insert(id.clone(), entry);
    }
    let order = c["order"]
        .as_array()
        .ok_or_else(|| error("order must contain unique configured provider IDs"))?;
    let mut seen = HashSet::new();
    let mut enabled = false;
    if order.is_empty() {
        return Err(error("order must contain unique configured provider IDs"));
    }
    for item in order {
        let id = item
            .as_str()
            .ok_or_else(|| error("order must contain unique configured provider IDs"))?;
        if !seen.insert(id) || !providers.contains_key(id) {
            return Err(error("order must contain unique configured provider IDs"));
        }
        enabled |= providers[id]["enabled"] == true;
    }
    if !enabled {
        return Err(error("order must include at least one enabled provider"));
    }
    c["providers"] = Value::Object(providers);
    for (key, min) in [("cooldownMs", 0), ("handoffChars", 1000)] {
        if !c[key]
            .as_u64()
            .is_some_and(|n| n >= min && n <= 9007199254740991)
        {
            return Err(error(format!("Invalid {key}")));
        }
    }
    Ok(c)
}
pub fn overrides(
    mut c: Value,
    order: Option<&str>,
    provider: Option<&str>,
    models: &[String],
) -> Result<Value> {
    if order.is_some() && provider.is_some() {
        return Err(error("Choose --order or --provider"));
    }
    let mut seen = HashSet::new();
    for assignment in models {
        let (id, model) = assignment
            .split_once('=')
            .ok_or_else(|| error("--model requires PROVIDER=MODEL"))?;
        if c["providers"].get(id).is_none() || model.is_empty() || !seen.insert(id) {
            return Err(error(
                "--model requires one PROVIDER=MODEL assignment per configured provider",
            ));
        }
        c["providers"][id]["model"] = model.into();
    }
    if let Some(id) = provider {
        c["order"] = json!([id]);
    } else if let Some(order) = order {
        c["order"] = json!(order.split(',').map(str::trim).collect::<Vec<_>>());
    }
    normalize(c)
}
pub fn identity(id: &str, c: &Value) -> Value {
    let p = &c["providers"][id];
    let adapter = p["adapter"].as_str().unwrap_or(id);
    json!({"adapter":adapter,"command":p["command"].as_str().unwrap_or(adapter),"model":p.get("model").cloned().unwrap_or(Value::Null)})
}
pub fn edit(path: &Path, args: &[String]) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("");
    if action == "init" && args.len() == 1 {
        private_dir(path.parent().ok_or_else(|| error("Invalid config path"))?)?;
        let mut f = private_options().write(true).create_new(true).open(path)?;
        let template = json!({"order":ADAPTERS,"providers":{"claude":{"adapter":"claude","enabled":true},"codex":{"adapter":"codex","enabled":true},"gemini":{"adapter":"gemini","enabled":true}}});
        f.write_all(format!("{}\n", serde_json::to_string_pretty(&template)?).as_bytes())?;
        f.sync_all()?;
        return Ok(());
    }
    if !((action == "set" && args.len() == 3) || (action == "unset" && args.len() == 2)) {
        return Err(error(
            "Usage: lennon config init | set KEY VALUE | unset KEY",
        ));
    }
    let key = &args[1];
    let parts: Vec<_> = key.split('.').collect();
    if parts.len() > 3
        || parts.iter().any(|s| {
            s.is_empty()
                || !s.as_bytes()[0].is_ascii_alphabetic()
                || !s
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                || ["constructor", "prototype", "__proto__"].contains(s)
        })
    {
        return Err(error("Invalid config key"));
    }
    let mut user = read(path, false)?;
    normalize(user.clone())?;
    let parsed = if action == "unset" {
        Value::Null
    } else if key == "order" {
        json!(args[2].split(',').map(str::trim).collect::<Vec<_>>())
    } else if ["cooldownMs", "handoffChars"].contains(&key.as_str()) {
        json!(
            args[2]
                .parse::<u64>()
                .map_err(|_| error(format!("Invalid {key}")))?
        )
    } else if parts.last() == Some(&"enabled") || (parts[0] == "providers" && parts.len() <= 2) {
        serde_json::from_str(&args[2]).map_err(|_| error(format!("Expected JSON for {key}")))?
    } else {
        Value::String(args[2].clone())
    };
    let mut target = &mut user;
    for part in &parts[..parts.len() - 1] {
        let map = target
            .as_object_mut()
            .ok_or_else(|| error("Invalid config key"))?;
        target = map.entry((*part).to_string()).or_insert_with(|| json!({}));
    }
    let map = target
        .as_object_mut()
        .ok_or_else(|| error("Invalid config key"))?;
    if action == "unset" {
        map.remove(*parts.last().unwrap());
    } else {
        map.insert(parts.last().unwrap().to_string(), parsed);
    }
    normalize(user.clone())?;
    private_dir(path.parent().unwrap_or(Path::new(".")))?;
    atomic_json(path, &user)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_defaults() {
        let c = normalize(
            json!({"order":["codex","claude"],"providers":{"claude":{"model":"sonnet"}}}),
        )
        .unwrap();
        assert_eq!(c["providers"]["claude"]["adapter"], "claude");
        assert_eq!(c["providers"]["gemini"]["enabled"], true);
    }
    #[test]
    fn named_models() {
        let c=normalize(json!({"order":["fast","deep"],"providers":{"fast":{"adapter":"codex","model":"a"},"deep":{"adapter":"codex","model":"b"}}})).unwrap();
        assert_eq!(c["providers"]["fast"]["model"], "a");
        assert_ne!(identity("fast", &c), identity("deep", &c));
    }
    #[test]
    fn invalid_settings() {
        for user in [
            json!(null),
            json!([]),
            json!({"providers":{"claude":null}}),
            json!({"order":[]}),
            json!({"order":["unknown"]}),
            json!({"order":["claude","claude"]}),
            json!({"providers":{"fast":{"model":"a"}}}),
            json!({"providers":{"claude":{"model":"-h"}}}),
            json!({"providers":{"claude":{"enabled":"false"}}}),
            json!({"providers":{"claude":{"modle":"oops"}}}),
            json!({"oder":[]}),
            json!({"order":["claude"],"providers":{"claude":{"enabled":false}}}),
            json!({"providers":{"constructor":{"adapter":"codex"}}}),
        ] {
            assert!(normalize(user).is_err());
        }
    }
    #[test]
    fn command_overrides() {
        let c = normalize(json!({})).unwrap();
        let d = overrides(
            c.clone(),
            Some(" codex,claude "),
            None,
            &["claude=sonnet".into()],
        )
        .unwrap();
        assert_eq!(d["order"], json!(["codex", "claude"]));
        assert_eq!(d["providers"]["claude"]["model"], "sonnet");
        assert!(c["providers"]["claude"].get("model").is_none());
        for models in [
            vec!["invalid".into()],
            vec!["unknown=x".into()],
            vec!["claude=".into()],
            vec!["claude=a".into(), "claude=b".into()],
        ] {
            assert!(overrides(c.clone(), None, None, &models).is_err());
        }
        assert!(overrides(c, Some("claude"), Some("codex"), &[]).is_err());
    }
}
