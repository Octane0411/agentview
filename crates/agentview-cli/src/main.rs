use agentview_codex_app_server::{AppServerClient, ThreadStartOptions};
use agentview_core::codex::attach_codex;
use agentview_core::jobs::{
    DispatchOptions, RemoveOptions, archive_job, dispatch_job, doctor, pin_job, remove_job,
    rename_job, reply_to_job, respawn_job, stop_job,
};
use agentview_core::schema::{Job, JobStatus};
use agentview_core::store::{list_jobs, read_job_last, require_job, tail_job_events};
use agentview_core::util::{relative_time, truncate};
use agentview_core::worker::worker_main;
use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "agentview",
    version,
    about = "Local Agent View-style controller for Codex sessions"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(about = "Dispatch a Codex job")]
    Run {
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long)]
        sandbox: Option<String>,
        #[arg(long)]
        attach: bool,
        #[arg(required = true)]
        task: Vec<String>,
    },
    #[command(alias = "ls", about = "List jobs")]
    List {
        #[arg(long)]
        all: bool,
    },
    #[command(about = "Show latest output for a job")]
    Peek { job_id: String },
    #[command(about = "Show normalized event log")]
    Logs {
        job_id: String,
        limit: Option<usize>,
    },
    #[command(about = "Resume full Codex conversation")]
    Attach { job_id: String },
    #[command(about = "Send a follow-up turn")]
    Reply {
        job_id: String,
        #[arg(required = true)]
        message: Vec<String>,
    },
    #[command(alias = "accept", about = "Send an approval reply")]
    Approve { job_id: String },
    #[command(alias = "deny", about = "Send a decline reply")]
    Decline { job_id: String },
    #[command(alias = "interrupt", about = "Stop a running job")]
    Stop { job_id: String },
    #[command(alias = "remove", about = "Remove a job")]
    Rm {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        purge: bool,
        job_id: String,
    },
    #[command(about = "Hide a job")]
    Archive { job_id: String },
    #[command(about = "Unhide a job")]
    Unarchive { job_id: String },
    #[command(about = "Rename a job")]
    Rename {
        job_id: String,
        #[arg(required = true)]
        title: Vec<String>,
    },
    #[command(about = "Pin or unpin a job")]
    Pin { job_id: String },
    #[command(about = "Resume a Codex thread in the background")]
    Respawn { job_id: String, prompt: Vec<String> },
    #[command(about = "Check local dependencies")]
    Doctor,
    #[command(hide = true, name = "__app-server-smoke")]
    AppServerSmoke {
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    #[command(hide = true, name = "__worker")]
    Worker {
        job_id: String,
        mode: String,
        prompt: Vec<String>,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => agentview_tui::run(),
        Some(Commands::Run {
            cwd,
            model,
            profile,
            sandbox,
            attach,
            task,
        }) => cmd_run(cwd, model, profile, sandbox, attach, task),
        Some(Commands::List { all }) => cmd_list(all),
        Some(Commands::Peek { job_id }) => cmd_peek(&job_id),
        Some(Commands::Logs { job_id, limit }) => cmd_logs(&job_id, limit.unwrap_or(80)),
        Some(Commands::Attach { job_id }) => cmd_attach(&job_id),
        Some(Commands::Reply { job_id, message }) => cmd_reply(&job_id, &message.join(" ")),
        Some(Commands::Approve { job_id }) => cmd_decision(&job_id, "approved"),
        Some(Commands::Decline { job_id }) => cmd_decision(&job_id, "declined"),
        Some(Commands::Stop { job_id }) => cmd_stop(&job_id),
        Some(Commands::Rm {
            force,
            purge,
            job_id,
        }) => cmd_remove(&job_id, force, purge),
        Some(Commands::Archive { job_id }) => cmd_archive(&job_id, true),
        Some(Commands::Unarchive { job_id }) => cmd_archive(&job_id, false),
        Some(Commands::Rename { job_id, title }) => cmd_rename(&job_id, &title.join(" ")),
        Some(Commands::Pin { job_id }) => cmd_pin(&job_id),
        Some(Commands::Respawn { job_id, prompt }) => cmd_respawn(&job_id, &prompt.join(" ")),
        Some(Commands::Doctor) => cmd_doctor(),
        Some(Commands::AppServerSmoke { cwd }) => cmd_app_server_smoke(cwd),
        Some(Commands::Worker {
            job_id,
            mode,
            prompt,
        }) => worker_main(&job_id, &mode, optional_join(&prompt).as_deref()),
    }
}

fn cmd_run(
    cwd: Option<PathBuf>,
    model: Option<String>,
    profile: Option<String>,
    sandbox: Option<String>,
    attach: bool,
    task: Vec<String>,
) -> Result<()> {
    let prompt = task.join(" ").trim().to_string();
    if prompt.is_empty() {
        bail!("Usage: agentview run [--cwd DIR] [--model MODEL] [--attach] \"task\"");
    }
    let job = dispatch_job(
        &prompt,
        DispatchOptions {
            cwd,
            model,
            profile,
            sandbox,
            ..Default::default()
        },
    )?;
    println!("backgrounded  {}", job.id);
    println!("  agentview                  list sessions");
    println!("  agentview attach {}  open in this terminal", job.id);
    println!("  agentview logs {}    show recent output", job.id);
    println!("  agentview stop {}    stop this session", job.id);
    if attach {
        cmd_attach(&job.id)?;
    }
    Ok(())
}

fn cmd_list(all: bool) -> Result<()> {
    for job in list_jobs(all)? {
        println!("{}", format_job_line(&job));
    }
    Ok(())
}

fn cmd_peek(job_id: &str) -> Result<()> {
    let job = require_job(job_id)?;
    let last = read_job_last(job_id)?;
    println!("{}  {}  {}", job.id, job.status, job.title);
    println!("cwd: {}", job.cwd);
    if let Some(thread_id) = &job.codex_thread_id {
        println!("thread: {thread_id}");
    }
    if let Some(worktree_path) = &job.worktree_path {
        println!("worktree: {worktree_path}");
    }
    if let Some(blocking_request) = &job.blocking_request {
        println!("needs input: {}", blocking_request.message);
    }
    if !job.pr_refs.is_empty() {
        println!(
            "prs: {}",
            job.pr_refs
                .iter()
                .map(|pr| pr.url.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!();
    println!(
        "{}",
        if last.trim().is_empty() {
            job.last_output
                .or(job.last_summary)
                .unwrap_or_else(|| "(no output yet)".to_string())
        } else {
            last
        }
    );
    Ok(())
}

fn cmd_logs(job_id: &str, limit: usize) -> Result<()> {
    require_job(job_id)?;
    for line in tail_job_events(job_id, limit)? {
        println!("{line}");
    }
    Ok(())
}

fn cmd_attach(job_id: &str) -> Result<()> {
    let job = require_job(job_id)?;
    attach_codex(&job)?;
    Ok(())
}

fn cmd_reply(job_id: &str, prompt: &str) -> Result<()> {
    if prompt.trim().is_empty() {
        bail!("Usage: agentview reply <job_id> \"message\"");
    }
    let pid = reply_to_job(job_id, prompt)?;
    println!("reply sent  {job_id}  pid {}", format_pid(pid));
    Ok(())
}

fn cmd_decision(job_id: &str, decision: &str) -> Result<()> {
    let pid = reply_to_job(job_id, decision)?;
    println!("{decision}  {job_id}  pid {}", format_pid(pid));
    Ok(())
}

fn cmd_stop(job_id: &str) -> Result<()> {
    stop_job(job_id)?;
    println!("stopped {job_id}");
    Ok(())
}

fn cmd_remove(job_id: &str, force: bool, purge: bool) -> Result<()> {
    remove_job(job_id, RemoveOptions { force, purge })?;
    println!("removed {job_id}");
    Ok(())
}

fn cmd_archive(job_id: &str, archived: bool) -> Result<()> {
    archive_job(job_id, archived)?;
    println!(
        "{} {job_id}",
        if archived { "archived" } else { "unarchived" }
    );
    Ok(())
}

fn cmd_rename(job_id: &str, title: &str) -> Result<()> {
    if title.trim().is_empty() {
        bail!("Usage: agentview rename <job_id> \"title\"");
    }
    rename_job(job_id, title)?;
    println!("renamed {job_id}");
    Ok(())
}

fn cmd_pin(job_id: &str) -> Result<()> {
    pin_job(job_id, None)?;
    println!("toggled pin {job_id}");
    Ok(())
}

fn cmd_respawn(job_id: &str, prompt: &str) -> Result<()> {
    let prompt = if prompt.trim().is_empty() {
        "Continue the previous task."
    } else {
        prompt
    };
    let pid = respawn_job(job_id, prompt)?;
    println!("respawned {job_id}  pid {}", format_pid(pid));
    Ok(())
}

fn cmd_doctor() -> Result<()> {
    let report = doctor();
    println!("codex: {}", if report.codex { "ok" } else { "missing" });
    println!(
        "rustc: {}",
        report.rustc.unwrap_or_else(|| "missing".to_string())
    );
    Ok(())
}

fn cmd_app_server_smoke(cwd: Option<PathBuf>) -> Result<()> {
    let mut client = AppServerClient::spawn_stdio()?;
    let initialized = client.initialize()?;
    println!("codex app-server: {}", initialized.user_agent);
    println!("codex home: {}", initialized.codex_home.display());
    println!(
        "platform: {}/{}",
        initialized.platform_family, initialized.platform_os
    );

    if cwd.is_some() {
        let thread = client.start_thread(ThreadStartOptions {
            cwd,
            ..Default::default()
        })?;
        println!(
            "thread: {}  status: {}",
            thread.thread.id,
            thread.thread.status_label()
        );
    }

    client.shutdown()?;
    Ok(())
}

fn format_job_line(job: &Job) -> String {
    let icon = status_icon(job.status);
    let time = relative_time(&job.updated_at);
    let title = truncate(&job.title, 28);
    let summary = truncate(
        job.blocking_request
            .as_ref()
            .map(|request| request.message.as_str())
            .or(job.last_summary.as_deref())
            .unwrap_or(""),
        72,
    );
    format!(
        "{icon} {:<16} {:<12} {:<30} {:<74} {time}",
        job.id, job.status, title, summary
    )
}

fn status_icon(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Working => "*",
        JobStatus::NeedsInput => "?",
        JobStatus::Completed => ".",
        JobStatus::Failed => "x",
        JobStatus::Stopped => "#",
        JobStatus::Idle => "-",
    }
}

fn format_pid(pid: Option<u32>) -> String {
    pid.map(|pid| pid.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn optional_join(parts: &[String]) -> Option<String> {
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}
