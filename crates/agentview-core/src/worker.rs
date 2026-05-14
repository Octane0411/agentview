use crate::codex::{run_codex_app_server_turn, run_codex_turn};
use crate::schema::{JobStatus, ProcessState};
use crate::store::{append_job_event, require_job, update_job};
use crate::util::now_iso;
use anyhow::{Result, bail};
use serde_json::json;

pub fn worker_main(job_id: &str, mode: &str, prompt: Option<&str>) -> Result<()> {
    let job = require_job(job_id)?;
    let (turn_prompt, resume) = match mode {
        "run" => (job.initial_prompt.clone(), false),
        "app-server-run" => {
            run_codex_app_server_turn(job_id, &job.initial_prompt)?;
            return Ok(());
        }
        "reply" | "resume" => (
            prompt
                .map(str::to_string)
                .unwrap_or_else(|| "Continue the previous task.".to_string()),
            true,
        ),
        other => bail!("Unknown worker mode: {other}"),
    };

    if let Err(error) = run_codex_turn(job_id, &turn_prompt, resume) {
        let message = error.to_string();
        let _ = append_job_event(
            job_id,
            &json!({
                "type": "worker_error",
                "error": message,
                "timestamp": now_iso()
            }),
        );
        let _ = update_job(job_id, |job| {
            job.status = JobStatus::Failed;
            job.process_state = ProcessState::Exited;
            job.pid = None;
            job.active_worker_pid = None;
            job.completed_at = Some(now_iso());
            job.last_summary = Some(format!("failed: {message}"));
            job.error = Some(message.clone());
            Ok(())
        });
        return Err(error);
    }

    Ok(())
}
