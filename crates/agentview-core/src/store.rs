use crate::schema::{Job, Store};
use crate::util::{home_dir, now_iso, path_exists};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

const STORE_VERSION: u32 = 1;

pub fn agentview_home() -> PathBuf {
    std::env::var_os("AGENTVIEW_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".agentview"))
}

pub fn store_path() -> PathBuf {
    agentview_home().join("agentview.json")
}

pub fn jobs_dir() -> PathBuf {
    agentview_home().join("jobs")
}

pub fn job_dir(job_id: &str) -> PathBuf {
    jobs_dir().join(job_id)
}

pub fn job_events_path(job_id: &str) -> PathBuf {
    job_dir(job_id).join("events.jsonl")
}

pub fn job_last_path(job_id: &str) -> PathBuf {
    job_dir(job_id).join("last.txt")
}

pub fn init_store() -> Result<()> {
    fs::create_dir_all(jobs_dir())?;
    if !path_exists(store_path()) {
        save_store(&Store::default())?;
    }
    Ok(())
}

pub fn load_store() -> Result<Store> {
    init_store()?;
    let content = fs::read_to_string(store_path()).context("failed to read agentview store")?;
    let mut store: Store =
        serde_json::from_str(&content).context("failed to parse agentview store")?;
    store.version = STORE_VERSION;
    Ok(store)
}

pub fn save_store(store: &Store) -> Result<()> {
    fs::create_dir_all(agentview_home())?;
    let mut normalized = store.clone();
    normalized.version = STORE_VERSION;

    let target = store_path();
    let temp = target.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        chrono::Utc::now().timestamp_millis()
    ));
    fs::write(
        &temp,
        format!("{}\n", serde_json::to_string_pretty(&normalized)?),
    )?;
    fs::rename(&temp, &target).with_context(|| {
        format!(
            "failed to move temporary store {} to {}",
            temp.display(),
            target.display()
        )
    })?;
    Ok(())
}

struct StoreLock {
    path: PathBuf,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn acquire_lock() -> Result<StoreLock> {
    fs::create_dir_all(agentview_home())?;
    let lock_path = agentview_home().join("agentview.lock");
    let started = Instant::now();
    loop {
        match fs::create_dir(&lock_path) {
            Ok(()) => {
                return Ok(StoreLock { path: lock_path });
            }
            Err(error) if started.elapsed() < Duration::from_secs(5) => {
                let _ = error;
                thread::sleep(Duration::from_millis(40));
            }
            Err(error) => {
                bail!("Timed out waiting for store lock: {error}");
            }
        }
    }
}

pub fn with_store<T>(mutator: impl FnOnce(&mut Store) -> Result<T>) -> Result<T> {
    let _lock = acquire_lock()?;
    let mut store = load_store()?;
    let result = mutator(&mut store)?;
    save_store(&store)?;
    Ok(result)
}

pub fn get_job(job_id: &str) -> Result<Option<Job>> {
    Ok(load_store()?.jobs.get(job_id).cloned())
}

pub fn require_job(job_id: &str) -> Result<Job> {
    match get_job(job_id)? {
        Some(job) if !job.deleted => Ok(job),
        _ => bail!("Unknown job: {job_id}"),
    }
}

pub fn list_jobs(all: bool) -> Result<Vec<Job>> {
    let mut jobs: Vec<Job> = load_store()?
        .jobs
        .into_values()
        .filter(|job| all || (!job.archived && !job.deleted))
        .collect();
    sort_jobs(&mut jobs);
    Ok(jobs)
}

fn sort_jobs(jobs: &mut [Job]) {
    jobs.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| match (a.manual_order, b.manual_order) {
                (Some(left), Some(right)) => left.cmp(&right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
}

pub fn put_job(job: Job) -> Result<Job> {
    with_store(|store| {
        fs::create_dir_all(job_dir(&job.id))?;
        store.jobs.insert(job.id.clone(), job.clone());
        Ok(job)
    })
}

pub fn update_job(job_id: &str, updater: impl FnOnce(&mut Job) -> Result<()>) -> Result<Job> {
    with_store(|store| {
        let Some(job) = store.jobs.get_mut(job_id) else {
            bail!("Unknown job: {job_id}");
        };
        updater(job)?;
        job.updated_at = now_iso();
        Ok(job.clone())
    })
}

pub fn append_job_event(job_id: &str, event: &Value) -> Result<()> {
    fs::create_dir_all(job_dir(job_id))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(job_events_path(job_id))?;
    writeln!(file, "{}", serde_json::to_string(event)?)?;
    Ok(())
}

pub fn write_job_last(job_id: &str, text: impl AsRef<str>) -> Result<()> {
    fs::create_dir_all(job_dir(job_id))?;
    fs::write(job_last_path(job_id), text.as_ref())?;
    Ok(())
}

pub fn read_job_last(job_id: &str) -> Result<String> {
    let path = job_last_path(job_id);
    if !path_exists(&path) {
        return Ok(String::new());
    }
    Ok(fs::read_to_string(path)?)
}

pub fn tail_job_events(job_id: &str, limit: usize) -> Result<Vec<String>> {
    let path = job_events_path(job_id);
    if !path_exists(&path) {
        return Ok(Vec::new());
    }
    let lines: Vec<String> = BufReader::new(fs::File::open(path)?)
        .lines()
        .collect::<std::io::Result<_>>()?;
    let start = lines.len().saturating_sub(limit);
    Ok(lines[start..].to_vec())
}

pub fn read_job_events(job_id: &str) -> Result<Vec<Value>> {
    let path = job_events_path(job_id);
    if !path_exists(&path) {
        return Ok(Vec::new());
    }
    let mut events = Vec::new();
    for line in BufReader::new(fs::File::open(path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str(&line) {
            events.push(value);
        }
    }
    Ok(events)
}

pub fn get_preference(key: &str) -> Result<Option<Value>> {
    Ok(load_store()?.preferences.get(key).cloned())
}

pub fn set_preference(key: &str, value: Value) -> Result<()> {
    with_store(|store| {
        store.preferences.insert(key.to_string(), value);
        Ok(())
    })
}

pub fn remove_job_files(job_id: &str) -> Result<()> {
    let path = job_dir(job_id);
    if path_exists(&path) {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{JobBackend, JobStatus, ProcessState};

    #[test]
    fn manual_order_sorts_before_updated_at_with_pinned_first() {
        let mut jobs = vec![
            job("newer", None, false, "2026-05-14T10:00:00Z"),
            job("second", Some(1), false, "2026-05-14T08:00:00Z"),
            job("pinned", Some(9), true, "2026-05-14T07:00:00Z"),
            job("first", Some(0), false, "2026-05-14T06:00:00Z"),
        ];

        sort_jobs(&mut jobs);

        assert_eq!(
            jobs.iter().map(|job| job.id.as_str()).collect::<Vec<_>>(),
            vec!["pinned", "first", "second", "newer"]
        );
    }

    fn job(id: &str, manual_order: Option<i64>, pinned: bool, updated_at: &str) -> Job {
        Job {
            id: id.to_string(),
            provider: "codex".to_string(),
            backend: JobBackend::AppServer,
            codex_thread_id: None,
            codex_turn_id: None,
            title: id.to_string(),
            initial_prompt: id.to_string(),
            repo_root: "/repo".to_string(),
            cwd: "/repo".to_string(),
            dispatch_cwd: "/repo".to_string(),
            worktree_path: None,
            worktree_branch: None,
            model: None,
            profile: None,
            approval_policy: "never".to_string(),
            sandbox: "workspace-write".to_string(),
            status: JobStatus::Working,
            process_state: ProcessState::Exited,
            pid: None,
            active_worker_pid: None,
            pinned,
            manual_order,
            archived: false,
            deleted: false,
            last_summary: None,
            last_output: None,
            blocking_request: None,
            pr_refs: Vec::new(),
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            completed_at: None,
            exit_code: None,
            error: None,
        }
    }
}
