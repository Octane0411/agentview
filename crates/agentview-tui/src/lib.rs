use agentview_core::codex::attach_codex;
use agentview_core::jobs::{
    DispatchOptions, dispatch_job, pin_job, remove_job, rename_job, reply_to_job, stop_job,
};
use agentview_core::schema::{Job, JobStatus};
use agentview_core::store::{list_jobs, read_job_last};
use agentview_core::util::{relative_time, truncate};
use anyhow::{Result, bail};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Stdout};
use std::time::{Duration, Instant};

type Term = Terminal<CrosstermBackend<Stdout>>;

#[derive(Debug, Clone)]
enum Row {
    Header(String),
    Job(Job),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupBy {
    State,
    Cwd,
}

#[derive(Debug, Default, Clone)]
struct Counts {
    needs_input: usize,
    working: usize,
    completed: usize,
}

#[derive(Debug, Clone)]
struct LastDelete {
    job_id: String,
    at: Instant,
}

#[derive(Debug)]
struct App {
    jobs: Vec<Job>,
    rows: Vec<Row>,
    selected: usize,
    input: String,
    message: String,
    peek: bool,
    help: bool,
    group_by: GroupBy,
    last_delete: Option<LastDelete>,
    last_refresh: Instant,
}

impl Default for App {
    fn default() -> Self {
        Self {
            jobs: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            input: String::new(),
            message: String::new(),
            peek: false,
            help: false,
            group_by: GroupBy::State,
            last_delete: None,
            last_refresh: Instant::now(),
        }
    }
}

pub fn run() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "agentview TUI requires an interactive terminal. Use `agentview list` in non-TTY contexts."
        );
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = App::default().run(&mut terminal);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
    terminal.show_cursor()?;
    result
}

impl App {
    fn run(&mut self, terminal: &mut Term) -> Result<()> {
        self.refresh()?;
        loop {
            self.draw(terminal)?;
            if event::poll(Duration::from_millis(200))? {
                if let Event::Key(key) = event::read()? {
                    if self.handle_key(key, terminal)? {
                        break;
                    }
                }
            }
            if self.last_refresh.elapsed() >= Duration::from_millis(1500) {
                self.refresh()?;
            }
        }
        Ok(())
    }

    fn refresh(&mut self) -> Result<()> {
        self.jobs = list_jobs(false)?;
        self.build_rows();
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
        if matches!(self.rows.get(self.selected), Some(Row::Header(_))) {
            if let Some(index) = self.rows.iter().position(|row| matches!(row, Row::Job(_))) {
                self.selected = index;
            }
        }
        self.last_refresh = Instant::now();
        Ok(())
    }

    fn build_rows(&mut self) {
        let mut rows = Vec::new();
        for (label, jobs) in self.grouped_jobs() {
            if jobs.is_empty() {
                continue;
            }
            rows.push(Row::Header(label));
            rows.extend(jobs.into_iter().map(Row::Job));
        }
        self.rows = rows;
    }

    fn grouped_jobs(&self) -> Vec<(String, Vec<Job>)> {
        match self.group_by {
            GroupBy::State => {
                let pinned: Vec<_> = self.jobs.iter().filter(|job| job.pinned).cloned().collect();
                let rest: Vec<_> = self
                    .jobs
                    .iter()
                    .filter(|job| !job.pinned)
                    .cloned()
                    .collect();
                vec![
                    ("Pinned".to_string(), pinned),
                    (
                        "Needs input".to_string(),
                        rest.iter()
                            .filter(|job| job.status == JobStatus::NeedsInput)
                            .cloned()
                            .collect(),
                    ),
                    (
                        "Working".to_string(),
                        rest.iter()
                            .filter(|job| job.status == JobStatus::Working)
                            .cloned()
                            .collect(),
                    ),
                    (
                        "Completed".to_string(),
                        rest.iter()
                            .filter(|job| job.status == JobStatus::Completed)
                            .cloned()
                            .collect(),
                    ),
                    (
                        "Failed".to_string(),
                        rest.iter()
                            .filter(|job| job.status == JobStatus::Failed)
                            .cloned()
                            .collect(),
                    ),
                    (
                        "Stopped".to_string(),
                        rest.iter()
                            .filter(|job| job.status == JobStatus::Stopped)
                            .cloned()
                            .collect(),
                    ),
                ]
            }
            GroupBy::Cwd => {
                let mut grouped: BTreeMap<String, Vec<Job>> = BTreeMap::new();
                for job in &self.jobs {
                    grouped
                        .entry(short_cwd(if job.dispatch_cwd.is_empty() {
                            &job.cwd
                        } else {
                            &job.dispatch_cwd
                        }))
                        .or_default()
                        .push(job.clone());
                }
                grouped.into_iter().collect()
            }
        }
    }

    fn selected_job(&self) -> Option<Job> {
        match self.rows.get(self.selected) {
            Some(Row::Job(job)) => Some(job.clone()),
            _ => None,
        }
    }

    fn handle_key(&mut self, key: KeyEvent, terminal: &mut Term) -> Result<bool> {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                if self.input.is_empty() {
                    return Ok(true);
                }
                self.input.clear();
                self.message.clear();
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if self.help || self.peek || !self.input.is_empty() {
                    self.help = false;
                    self.peek = false;
                    self.input.clear();
                } else {
                    return Ok(true);
                }
            }
            KeyEvent {
                code: KeyCode::Up, ..
            } => self.move_selection(-1),
            KeyEvent {
                code: KeyCode::Down,
                ..
            } => self.move_selection(1),
            KeyEvent {
                code: KeyCode::Right,
                ..
            } => self.attach_selected(terminal)?,
            KeyEvent {
                code: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.stop_or_delete()?,
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => self.toggle_pin()?,
            KeyEvent {
                code: KeyCode::Char('s'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.group_by = if self.group_by == GroupBy::State {
                    GroupBy::Cwd
                } else {
                    GroupBy::State
                };
                self.refresh()?;
            }
            KeyEvent {
                code: KeyCode::Char('?'),
                ..
            } => self.help = !self.help,
            KeyEvent {
                code: KeyCode::Char(' '),
                ..
            } => {
                if self.input.is_empty() {
                    self.peek = !self.peek;
                } else {
                    self.input.push(' ');
                }
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => self.submit(terminal)?,
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                self.input.pop();
            }
            KeyEvent {
                code: KeyCode::Char(ch),
                modifiers,
                ..
            } if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT => {
                self.input.push(ch);
            }
            _ => {}
        }
        Ok(false)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let mut next = self.selected as isize;
        loop {
            next = (next + delta).clamp(0, self.rows.len().saturating_sub(1) as isize);
            if matches!(self.rows.get(next as usize), Some(Row::Job(_)))
                || next == 0
                || next == self.rows.len() as isize - 1
            {
                self.selected = next as usize;
                break;
            }
        }
    }

    fn submit(&mut self, terminal: &mut Term) -> Result<()> {
        let text = self.input.trim().to_string();
        self.input.clear();
        let selected = self.selected_job();

        if let Some(rest) = text.strip_prefix("/rename ") {
            if let Some(job) = selected {
                rename_job(&job.id, rest.trim())?;
                self.message = format!("renamed {}", job.id);
                self.refresh()?;
            }
            return Ok(());
        }

        if !text.is_empty() {
            if self.peek {
                if let Some(job) = selected {
                    match reply_to_job(&job.id, &text) {
                        Ok(_) => self.message = format!("reply sent to {}", job.id),
                        Err(error) => self.message = error.to_string(),
                    }
                    self.refresh()?;
                    return Ok(());
                }
            }
            match dispatch_job(&text, DispatchOptions::default()) {
                Ok(job) => self.message = format!("backgrounded {}", job.id),
                Err(error) => self.message = error.to_string(),
            }
            self.refresh()?;
            return Ok(());
        }

        self.attach_selected(terminal)
    }

    fn attach_selected(&mut self, terminal: &mut Term) -> Result<()> {
        let Some(job) = self.selected_job() else {
            return Ok(());
        };

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
        terminal.show_cursor()?;

        let attach_result = attach_codex(&job);
        if let Err(error) = attach_result {
            eprintln!("{error}");
            eprintln!("Press Enter to return to Agent View.");
            let mut input = String::new();
            let _ = io::stdin().read_line(&mut input);
        }

        execute!(terminal.backend_mut(), EnterAlternateScreen, Hide)?;
        enable_raw_mode()?;
        terminal.clear()?;
        self.refresh()?;
        Ok(())
    }

    fn stop_or_delete(&mut self) -> Result<()> {
        let Some(job) = self.selected_job() else {
            return Ok(());
        };
        let same_target = self.last_delete.as_ref().is_some_and(|last| {
            last.job_id == job.id && last.at.elapsed() < Duration::from_secs(2)
        });
        if same_target {
            match remove_job(&job.id, Default::default()) {
                Ok(()) => self.message = format!("removed {}", job.id),
                Err(error) => self.message = error.to_string(),
            }
            self.last_delete = None;
            self.refresh()?;
            return Ok(());
        }
        stop_job(&job.id)?;
        self.last_delete = Some(LastDelete {
            job_id: job.id.clone(),
            at: Instant::now(),
        });
        self.message = format!("stopped {}; press Ctrl+X again to delete", job.id);
        self.refresh()
    }

    fn toggle_pin(&mut self) -> Result<()> {
        if let Some(job) = self.selected_job() {
            pin_job(&job.id, None)?;
            self.refresh()?;
        }
        Ok(())
    }

    fn draw(&mut self, terminal: &mut Term) -> Result<()> {
        let selected = self.selected_job();
        let peek_lines = if self.help {
            render_help()
        } else if self.peek {
            render_peek(selected.as_ref())?
        } else {
            Vec::new()
        };
        let counts = count_jobs(&self.jobs);
        let rows = self.rows.clone();
        let selected_index = self.selected;
        let input = self.input.clone();
        let message = self.message.clone();
        let prompt = if self.peek && selected.is_some() {
            "reply"
        } else {
            "describe a task for a new session"
        };

        terminal.draw(|frame| {
            let panel_height = if peek_lines.is_empty() { 0 } else { 8 };
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(4),
                    Constraint::Length(panel_height),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ])
                .split(frame.area());

            let header = Paragraph::new(vec![
                Line::from(vec![Span::styled(
                    "Codex Agent View v0.1.0",
                    Style::default().add_modifier(Modifier::BOLD),
                )]),
                Line::from(format!(
                    "{} awaiting input . {} working . {} completed",
                    counts.needs_input, counts.working, counts.completed
                )),
            ]);
            frame.render_widget(header, chunks[0]);

            let mut state = ListState::default();
            if !rows.is_empty() {
                state.select(Some(selected_index));
            }
            let items: Vec<_> = rows
                .iter()
                .map(|row| match row {
                    Row::Header(label) => ListItem::new(Line::from(Span::styled(
                        label.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ))),
                    Row::Job(job) => ListItem::new(Line::from(render_job_spans(job))),
                })
                .collect();
            let list = List::new(items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));
            frame.render_stateful_widget(list, chunks[1], &mut state);

            if panel_height > 0 {
                let panel = Paragraph::new(peek_lines)
                    .block(Block::default().borders(Borders::TOP))
                    .wrap(Wrap { trim: true });
                frame.render_widget(panel, chunks[2]);
            }

            let input_line = if input.is_empty() {
                format!("> {prompt}")
            } else {
                format!("> {input}")
            };
            let mut input_lines = Vec::new();
            if !message.is_empty() {
                input_lines.push(Line::from(message));
            }
            input_lines.push(Line::from(input_line));
            let input_widget = Paragraph::new(input_lines).block(Block::default().borders(Borders::TOP));
            frame.render_widget(input_widget, chunks[3]);

            frame.render_widget(
                Paragraph::new("enter to open/send . space to reply . ctrl+x stop/delete . ctrl+s group . ctrl+t pin . ? help"),
                chunks[4],
            );
        })?;
        Ok(())
    }
}

fn render_job_spans(job: &Job) -> Vec<Span<'static>> {
    let icon = status_icon(job.status);
    let title = truncate(&job.title, 32);
    let summary = truncate(
        job.blocking_request
            .as_ref()
            .map(|request| request.message.as_str())
            .or(job.last_summary.as_deref())
            .unwrap_or(""),
        88,
    );
    vec![
        Span::raw(format!("{icon} ")),
        Span::raw(format!("{title:<34}")),
        Span::raw(format!(" {summary:<74}")),
        Span::raw(format!(" {}", relative_time(&job.updated_at))),
    ]
}

fn render_peek(job: Option<&Job>) -> Result<Vec<Line<'static>>> {
    let Some(job) = job else {
        return Ok(vec![Line::from("No session selected.")]);
    };
    let last = read_job_last(&job.id)?;
    let mut lines = vec![
        Line::from(format!("{}  {}  {}", job.id, job.status, job.title)),
        Line::from(format!("cwd: {}", job.cwd)),
    ];
    if let Some(thread_id) = &job.codex_thread_id {
        lines.push(Line::from(format!("thread: {thread_id}")));
    }
    if let Some(turn_id) = &job.codex_turn_id {
        lines.push(Line::from(format!("turn: {turn_id}")));
    }
    if let Some(worktree_path) = &job.worktree_path {
        lines.push(Line::from(format!("worktree: {worktree_path}")));
    }
    if let Some(blocking_request) = &job.blocking_request {
        lines.push(Line::from(format!(
            "needs input: {}",
            blocking_request.message
        )));
    }
    if !job.pr_refs.is_empty() {
        lines.push(Line::from(format!(
            "prs: {}",
            job.pr_refs
                .iter()
                .map(|pr| pr.url.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    lines.push(Line::from(truncate(
        if !last.trim().is_empty() {
            last
        } else {
            job.last_output
                .clone()
                .or_else(|| job.last_summary.clone())
                .unwrap_or_else(|| "(no output yet)".to_string())
        },
        160,
    )));
    Ok(lines)
}

fn render_help() -> Vec<Line<'static>> {
    [
        "Shortcuts",
        "up/down select . enter open or send . space peek/reply . right attach",
        "ctrl+x stop, press again to delete . ctrl+t pin . ctrl+s group by state/directory",
        "type a prompt to dispatch . with peek open, typed text replies to selected session",
        "type /rename <title> while a row is selected to rename it . esc exits",
    ]
    .into_iter()
    .map(Line::from)
    .collect()
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

fn count_jobs(jobs: &[Job]) -> Counts {
    Counts {
        needs_input: jobs
            .iter()
            .filter(|job| job.status == JobStatus::NeedsInput)
            .count(),
        working: jobs
            .iter()
            .filter(|job| job.status == JobStatus::Working)
            .count(),
        completed: jobs
            .iter()
            .filter(|job| job.status == JobStatus::Completed)
            .count(),
    }
}

fn short_cwd(cwd: &str) -> String {
    let Ok(home) = std::env::var("HOME") else {
        return cwd.to_string();
    };
    cwd.strip_prefix(&home)
        .map(|suffix| format!("~{suffix}"))
        .unwrap_or_else(|| cwd.to_string())
}
