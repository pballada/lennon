use crate::{Result, error};
use chrono::{SecondsFormat, Utc};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::fd::AsRawFd,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use uuid::Uuid;
pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
pub fn millis() -> u64 {
    Utc::now().timestamp_millis().max(0) as u64
}
pub fn private_options() -> OpenOptions {
    let mut o = OpenOptions::new();
    o.mode(0o600);
    o
}
pub fn private_dir(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    Ok(())
}
pub fn atomic_json(path: &Path, value: &Value) -> Result<()> {
    let temp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        Uuid::new_v4()
    ));
    let outcome = (|| -> Result<()> {
        let mut f = private_options().create_new(true).write(true).open(&temp)?;
        serde_json::to_writer_pretty(&mut f, value)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&temp);
    }
    outcome
}
pub fn valid_session(id: &str) -> bool {
    id.len() == 36
        && Uuid::parse_str(id).is_ok()
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
}
pub struct Session {
    pub dir: PathBuf,
    pub journal: PathBuf,
    pub meta: Value,
    pub events: Vec<Value>,
    lock: Option<File>,
    guard: Option<File>,
    writer: Option<File>,
}
impl Session {
    pub fn open(root: &Path, id: &str) -> Result<Self> {
        if !valid_session(id) {
            return Err(error("Invalid session ID"));
        }
        let dir = root.join("sessions").join(id);
        let meta = serde_json::from_slice(&fs::read(dir.join("session.json"))?)?;
        Ok(Self {
            journal: dir.join("events.jsonl"),
            dir,
            meta,
            events: vec![],
            lock: None,
            guard: None,
            writer: None,
        })
    }
    pub fn create(root: &Path, cwd: &Path) -> Result<Self> {
        let id = Uuid::new_v4().to_string();
        let dir = root.join("sessions").join(&id);
        private_dir(&dir)?;
        atomic_json(
            &dir.join("session.json"),
            &json!({"version":1,"id":id,"cwd":fs::canonicalize(cwd)?,"created":now(),"updated":now(),"providers":{},"cooldowns":{},"pending":false}),
        )?;
        Self::open(root, &id)
    }
    pub fn acquire(&mut self) -> Result<()> {
        let guard = private_options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.dir.join("lock.guard"))?;
        // All native writers serialize stale-lock recovery using this stable inode.
        if unsafe { libc::flock(guard.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(error("Session is already running"));
        }
        let path = self.dir.join("lock");
        let mut lock = match private_options().write(true).create_new(true).open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let pid = fs::read_to_string(&path)?
                    .trim()
                    .parse::<i32>()
                    .map_err(|_| error("Session lock is invalid"))?;
                if pid <= 0 {
                    return Err(error("Session lock is invalid"));
                }
                let live = unsafe { libc::kill(pid, 0) };
                if live == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                {
                    return Err(error(format!("Session is already running (PID {pid})")));
                }
                fs::remove_file(&path)?;
                private_options().write(true).create_new(true).open(&path)?
            }
            Err(e) => return Err(e.into()),
        };
        writeln!(lock, "{}", std::process::id())?;
        lock.sync_all()?;
        self.lock = Some(lock);
        self.guard = Some(guard);
        if let Err(e) = self.recover() {
            self.release();
            return Err(e);
        }
        Ok(())
    }
    fn recover(&mut self) -> Result<()> {
        self.meta = serde_json::from_slice(&fs::read(self.dir.join("session.json"))?)?;
        if !self.meta["providers"].is_object() || !self.meta["cooldowns"].is_object() {
            return Err(error("Invalid session metadata"));
        }
        let mut bytes = match fs::read(&self.journal) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
            let end = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |n| n + 1);
            bytes.truncate(end);
            let f = private_options().write(true).open(&self.journal)?;
            f.set_len(end as u64)?;
            f.sync_all()?;
        }
        self.events = bytes
            .split(|b| *b == b'\n')
            .filter(|b| !b.is_empty())
            .map(serde_json::from_slice)
            .collect::<std::result::Result<Vec<Value>, _>>()?;
        let mut user = None;
        let mut done = None;
        for (i, e) in self.events.iter().enumerate() {
            let id = e["provider"].as_str().unwrap_or("");
            match e["type"].as_str().unwrap_or("") {
                "user" => user = Some(i),
                "route_reset" => {
                    self.meta["providers"].as_object_mut().unwrap().remove(id);
                }
                "attempt_start" if e.get("route").is_some() => {
                    let p = self.meta["providers"]
                        .as_object_mut()
                        .unwrap()
                        .entry(id)
                        .or_insert_with(|| json!({}));
                    p["route"] = e["route"].clone();
                }
                "native_session" => {
                    let p = self.meta["providers"]
                        .as_object_mut()
                        .unwrap()
                        .entry(id)
                        .or_insert_with(|| json!({}));
                    p["id"] = e["id"].clone();
                    if let Some(route) = e.get("route") {
                        p["route"] = route.clone();
                    }
                }
                "complete" => {
                    done = Some(i);
                    let p = self.meta["providers"]
                        .as_object_mut()
                        .unwrap()
                        .entry(id)
                        .or_insert_with(|| json!({}));
                    p["cursor"] = json!(i + 1);
                }
                _ => {}
            }
        }
        self.meta["pending"] = json!(user.is_some() && (done.is_none() || user > done));
        Ok(())
    }
    pub fn append(&mut self, mut event: Value) -> Result<()> {
        event
            .as_object_mut()
            .ok_or_else(|| error("Invalid journal event"))?
            .insert("time".into(), json!(now()));
        if self.writer.is_none() {
            self.writer = Some(
                private_options()
                    .append(true)
                    .create(true)
                    .open(&self.journal)?,
            );
        }
        let f = self.writer.as_mut().unwrap();
        serde_json::to_writer(&mut *f, &event)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        self.events.push(event);
        Ok(())
    }
    pub fn save(&mut self) -> Result<()> {
        self.meta["updated"] = json!(now());
        atomic_json(&self.dir.join("session.json"), &self.meta)
    }
    pub fn release(&mut self) {
        self.writer.take();
        if self.lock.take().is_some() {
            let _ = fs::remove_file(self.dir.join("lock"));
        }
        if let Some(guard) = self.guard.take() {
            // A fork can temporarily retain a duplicate of this open file
            // description even with CLOEXEC. Unlock explicitly so those copies
            // cannot prolong the session lock after the owner releases it.
            unsafe { libc::flock(guard.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.release();
    }
}
/// The first user request of a session, for pickers.
pub fn title(root: &Path, id: &str) -> Option<String> {
    if !valid_session(id) {
        return None;
    }
    let text = fs::read_to_string(root.join("sessions").join(id).join("events.jsonl")).ok()?;
    text.lines()
        .filter(|l| l.contains("\"type\":\"user\""))
        .find_map(|l| serde_json::from_str::<Value>(l).ok())
        .and_then(|v| v["text"].as_str().map(str::to_owned))
}
pub fn list(root: &Path) -> Result<Vec<Value>> {
    let entries = match fs::read_dir(root.join("sessions")) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut out = vec![];
    for e in entries {
        let id = e?.file_name().to_string_lossy().into_owned();
        if valid_session(&id) {
            out.push(Session::open(root, &id)?.meta.clone());
        }
    }
    out.sort_by(|a, b| b["updated"].as_str().cmp(&a["updated"].as_str()));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;

    #[test]
    fn release_unlocks_even_when_an_inherited_descriptor_remains_open() {
        let root = std::env::temp_dir().join(format!("lennon-lock-{}", Uuid::new_v4()));
        private_dir(&root).unwrap();
        let mut owner = Session::create(&root, &root).unwrap();
        let id = owner.meta["id"].as_str().unwrap().to_owned();
        owner.acquire().unwrap();
        let fd = unsafe { libc::dup(owner.guard.as_ref().unwrap().as_raw_fd()) };
        assert!(fd >= 0);
        let retained = unsafe { File::from_raw_fd(fd) };
        owner.release();
        let mut successor = Session::open(&root, &id).unwrap();
        successor.acquire().unwrap();
        drop(retained);
        let mut competitor = Session::open(&root, &id).unwrap();
        assert!(competitor.acquire().is_err());
        successor.release();
        fs::remove_dir_all(root).unwrap();
    }
}
