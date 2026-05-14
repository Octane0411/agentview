use agentview_core::codex::attach_codex;
use agentview_core::jobs::{
    DispatchOptions, dispatch_job, pin_job, remove_job, rename_job, reorder_jobs, reply_to_job,
    stop_job,
};
use agentview_core::schema::{Job, JobStatus};
use agentview_core::store::{
    append_job_event, get_preference, list_jobs, read_job_last, set_preference,
};
use agentview_core::util::{format_pr_refs, now_iso, pr_status_indicator, relative_time, truncate};
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
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::io::{self, IsTerminal, Stdout};
use std::time::{Duration, Instant};

type Term = Terminal<CrosstermBackend<Stdout>>;
const GROUP_BY_PREFERENCE: &str = "tui.groupBy";

#[derive(Debug, Clone)]
enum Row {
    Header {
        key: String,
        label: String,
        count: usize,
        collapsed: bool,
        dispatch_cwd: Option<std::path::PathBuf>,
    },
    Job(Job),
}

#[derive(Debug, Clone)]
struct Group {
    key: String,
    label: String,
    dispatch_cwd: Option<std::path::PathBuf>,
    jobs: Vec<Job>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupBy {
    State,
    Cwd,
}

impl GroupBy {
    fn as_str(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Cwd => "cwd",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "state" => Some(Self::State),
            "cwd" => Some(Self::Cwd),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Clone)]
struct Counts {
    needs_input: usize,
    working: usize,
    completed: usize,
}

#[derive(Debug, Clone)]
struct LastDelete {
    target: DeleteTarget,
    at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeleteTarget {
    Job(String),
    Group(String),
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
    renaming: Option<String>,
    group_by: GroupBy,
    collapsed_groups: HashSet<String>,
    last_delete: Option<LastDelete>,
    last_ctrl_c: Option<Instant>,
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
            renaming: None,
            group_by: GroupBy::State,
            collapsed_groups: HashSet::new(),
            last_delete: None,
            last_ctrl_c: None,
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
    let mut app = App::default();
    app.load_preferences()?;
    let result = app.run(&mut terminal);
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
        let initial_refresh = self.rows.is_empty();
        self.jobs = list_jobs(false)?;
        self.build_rows();
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
        if initial_refresh && matches!(self.rows.get(self.selected), Some(Row::Header { .. })) {
            if let Some(index) = self.rows.iter().position(|row| matches!(row, Row::Job(_))) {
                self.selected = index;
            }
        }
        self.last_refresh = Instant::now();
        Ok(())
    }

    fn load_preferences(&mut self) -> Result<()> {
        if let Some(value) = get_preference(GROUP_BY_PREFERENCE)?
            .as_ref()
            .and_then(serde_json::Value::as_str)
            .and_then(GroupBy::from_str)
        {
            self.group_by = value;
        }
        Ok(())
    }

    fn build_rows(&mut self) {
        let mut rows = Vec::new();
        for group in self.grouped_jobs() {
            if group.jobs.is_empty() {
                continue;
            }
            let collapsed = self.collapsed_groups.contains(&group.key);
            rows.push(Row::Header {
                key: group.key,
                label: group.label,
                count: group.jobs.len(),
                collapsed,
                dispatch_cwd: group.dispatch_cwd,
            });
            if !collapsed {
                rows.extend(group.jobs.into_iter().map(Row::Job));
            }
        }
        self.rows = rows;
    }

    fn grouped_jobs(&self) -> Vec<Group> {
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
                    state_group("Pinned", pinned),
                    state_group(
                        "Ready for review",
                        rest.iter()
                            .filter(|job| is_ready_for_review(job))
                            .cloned()
                            .collect(),
                    ),
                    state_group(
                        "Needs input",
                        rest.iter()
                            .filter(|job| {
                                !is_ready_for_review(job) && job.status == JobStatus::NeedsInput
                            })
                            .cloned()
                            .collect(),
                    ),
                    state_group(
                        "Working",
                        rest.iter()
                            .filter(|job| {
                                !is_ready_for_review(job) && job.status == JobStatus::Working
                            })
                            .cloned()
                            .collect(),
                    ),
                    state_group(
                        "Completed",
                        rest.iter()
                            .filter(|job| {
                                !is_ready_for_review(job) && is_terminal_status(job.status)
                            })
                            .cloned()
                            .collect(),
                    ),
                ]
            }
            GroupBy::Cwd => {
                let mut grouped: BTreeMap<String, Vec<Job>> = BTreeMap::new();
                for job in &self.jobs {
                    let cwd = if job.dispatch_cwd.is_empty() {
                        &job.cwd
                    } else {
                        &job.dispatch_cwd
                    };
                    grouped.entry(cwd.clone()).or_default().push(job.clone());
                }
                grouped
                    .into_iter()
                    .map(|(cwd, jobs)| Group {
                        key: format!("cwd:{cwd}"),
                        dispatch_cwd: Some(std::path::PathBuf::from(&cwd)),
                        label: short_cwd(&cwd),
                        jobs,
                    })
                    .collect()
            }
        }
    }

    fn selected_job(&self) -> Option<Job> {
        match self.rows.get(self.selected) {
            Some(Row::Job(job)) => Some(job.clone()),
            _ => None,
        }
    }

    fn selected_header_dispatch_cwd(&self) -> Option<std::path::PathBuf> {
        match self.rows.get(self.selected) {
            Some(Row::Header { dispatch_cwd, .. }) => dispatch_cwd.clone(),
            _ => None,
        }
    }

    fn selected_group(&self) -> Option<Group> {
        let Some(Row::Header { key, .. }) = self.rows.get(self.selected) else {
            return None;
        };
        self.grouped_jobs()
            .into_iter()
            .find(|group| group.key == *key)
    }

    fn handle_key(&mut self, key: KeyEvent, terminal: &mut Term) -> Result<bool> {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                if self.handle_ctrl_c() {
                    return Ok(true);
                }
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if self.help || self.peek || self.renaming.is_some() || !self.input.is_empty() {
                    self.help = false;
                    self.peek = false;
                    self.renaming = None;
                    self.input.clear();
                } else {
                    return Ok(true);
                }
            }
            KeyEvent {
                code: KeyCode::Up,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::SHIFT) => self.move_selected_job(-1)?,
            KeyEvent {
                code: KeyCode::Down,
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::SHIFT) => self.move_selected_job(1)?,
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
            } => {
                if self.attach_selected(terminal)? {
                    return Ok(true);
                }
            }
            KeyEvent {
                code: KeyCode::Char('x'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => self.stop_or_delete()?,
            KeyEvent {
                code: KeyCode::Char('t'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => self.toggle_pin()?,
            KeyEvent {
                code: KeyCode::Char('r'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.start_rename();
            }
            KeyEvent {
                code: KeyCode::Char('s'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                self.group_by = if self.group_by == GroupBy::State {
                    GroupBy::Cwd
                } else {
                    GroupBy::State
                };
                set_preference(GROUP_BY_PREFERENCE, json!(self.group_by.as_str()))?;
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
            } => {
                if self.submit(terminal)? {
                    return Ok(true);
                }
            }
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
        self.selected = (self.selected as isize + delta)
            .clamp(0, self.rows.len().saturating_sub(1) as isize) as usize;
    }

    fn move_selected_job(&mut self, delta: isize) -> Result<()> {
        let Some(job) = self.selected_job() else {
            return Ok(());
        };
        let Some(group_order) = self.swap_selected_job_within_group(delta) else {
            return Ok(());
        };
        reorder_jobs(&group_order)?;
        self.refresh()?;
        if let Some(index) = self.rows.iter().position(|row| match row {
            Row::Job(row_job) => row_job.id == job.id,
            Row::Header { .. } => false,
        }) {
            self.selected = index;
        }
        Ok(())
    }

    fn swap_selected_job_within_group(&mut self, delta: isize) -> Option<Vec<String>> {
        if !matches!(self.rows.get(self.selected), Some(Row::Job(_))) {
            return None;
        }
        let (start, end) = self.selected_group_bounds();
        let target = self.selected as isize + delta;
        if target < start as isize || target >= end as isize {
            return None;
        }
        let target = target as usize;
        if !matches!(self.rows.get(target), Some(Row::Job(_))) {
            return None;
        }
        self.rows.swap(self.selected, target);
        self.selected = target;
        Some(self.job_ids_in_range(start, end))
    }

    fn selected_group_bounds(&self) -> (usize, usize) {
        let header = (0..=self.selected)
            .rev()
            .find(|index| matches!(self.rows.get(*index), Some(Row::Header { .. })))
            .unwrap_or(0);
        let start = header + 1;
        let end = (self.selected + 1..self.rows.len())
            .find(|index| matches!(self.rows.get(*index), Some(Row::Header { .. })))
            .unwrap_or(self.rows.len());
        (start, end)
    }

    fn job_ids_in_range(&self, start: usize, end: usize) -> Vec<String> {
        self.rows[start..end]
            .iter()
            .filter_map(|row| match row {
                Row::Job(job) => Some(job.id.clone()),
                Row::Header { .. } => None,
            })
            .collect()
    }

    fn submit(&mut self, terminal: &mut Term) -> Result<bool> {
        let text = self.input.trim().to_string();
        self.input.clear();
        let selected = self.selected_job();

        if let Some(job_id) = self.renaming.take() {
            if text.is_empty() {
                self.message = "rename cancelled".to_string();
                return Ok(false);
            }
            match rename_job(&job_id, &text) {
                Ok(()) => self.message = format!("renamed {job_id}"),
                Err(error) => self.message = error.to_string(),
            }
            self.refresh()?;
            return Ok(false);
        }

        if let Some(rest) = text.strip_prefix("/rename ") {
            if let Some(job) = selected {
                rename_job(&job.id, rest.trim())?;
                self.message = format!("renamed {}", job.id);
                self.refresh()?;
            }
            return Ok(false);
        }

        if !text.is_empty() {
            if self.peek {
                if let Some(job) = selected {
                    match reply_to_job(&job.id, &text) {
                        Ok(_) => self.message = format!("reply sent to {}", job.id),
                        Err(error) => self.message = error.to_string(),
                    }
                    self.refresh()?;
                    return Ok(false);
                }
            }
            match dispatch_job(&text, self.dispatch_options()) {
                Ok(job) => self.message = format!("backgrounded {}", job.id),
                Err(error) => self.message = error.to_string(),
            }
            self.refresh()?;
            return Ok(false);
        }

        if self.toggle_selected_group() {
            return Ok(false);
        }

        self.attach_selected(terminal)
    }

    fn dispatch_options(&self) -> DispatchOptions {
        DispatchOptions {
            cwd: self.dispatch_target_cwd(),
            ..Default::default()
        }
    }

    fn dispatch_target_cwd(&self) -> Option<std::path::PathBuf> {
        if self.group_by != GroupBy::Cwd {
            return None;
        }
        self.selected_job()
            .map(|job| job_dispatch_cwd(&job))
            .or_else(|| self.selected_header_dispatch_cwd())
    }

    fn toggle_selected_group(&mut self) -> bool {
        let Some(Row::Header { key, .. }) = self.rows.get(self.selected) else {
            return false;
        };
        let key = key.clone();
        if !self.collapsed_groups.insert(key.clone()) {
            self.collapsed_groups.remove(&key);
        }
        self.build_rows();
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
        true
    }

    fn attach_selected(&mut self, terminal: &mut Term) -> Result<bool> {
        let Some(job) = self.selected_job() else {
            return Ok(false);
        };

        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
        terminal.show_cursor()?;

        let attach_result = attach_codex(&job);
        let attach_ok = attach_result.is_ok();
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
        let _ = append_job_event(
            &job.id,
            &json!({
                "type": "agentview_list_returned_from_attach",
                "ok": attach_ok,
                "timestamp": now_iso()
            }),
        );
        Ok(std::env::var_os("AGENTVIEW_TUI_EXIT_AFTER_ATTACH").is_some())
    }

    fn stop_or_delete(&mut self) -> Result<()> {
        if let Some(group) = self.selected_group() {
            return self.confirm_or_remove_group(group);
        }

        let Some(job) = self.selected_job() else {
            return Ok(());
        };
        let target = DeleteTarget::Job(job.id.clone());
        let same_target = self.last_delete.as_ref().is_some_and(|last| {
            last.target == target && last.at.elapsed() < Duration::from_secs(2)
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
            target,
            at: Instant::now(),
        });
        self.message = format!("stopped {}; press Ctrl+X again to delete", job.id);
        self.refresh()
    }

    fn confirm_or_remove_group(&mut self, group: Group) -> Result<()> {
        if group.jobs.is_empty() {
            return Ok(());
        }

        let target = DeleteTarget::Group(group.key.clone());
        let same_target = self.last_delete.as_ref().is_some_and(|last| {
            last.target == target && last.at.elapsed() < Duration::from_secs(2)
        });
        if !same_target {
            self.last_delete = Some(LastDelete {
                target,
                at: Instant::now(),
            });
            self.message = format!(
                "press Ctrl+X again to remove {} sessions in {}; dirty worktrees are refused",
                group.jobs.len(),
                group.label
            );
            return Ok(());
        }

        let label = group.label;
        let mut removed = 0usize;
        let mut failures = Vec::new();
        for job in group.jobs {
            match remove_job(&job.id, Default::default()) {
                Ok(()) => removed += 1,
                Err(error) => failures.push(format!("{}: {error}", job.id)),
            }
        }
        self.last_delete = None;
        self.refresh()?;
        if failures.is_empty() {
            self.message = format!("removed {removed} sessions from {label}");
        } else {
            self.message = truncate(
                format!(
                    "removed {removed}; failed {}: {}",
                    failures.len(),
                    failures.join("; ")
                ),
                160,
            );
        }
        Ok(())
    }

    fn handle_ctrl_c(&mut self) -> bool {
        if self.help || self.peek || self.renaming.is_some() || !self.input.is_empty() {
            self.help = false;
            self.peek = false;
            self.renaming = None;
            self.input.clear();
            self.last_ctrl_c = Some(Instant::now());
            self.message = "cleared; press Ctrl+C again to exit".to_string();
            return false;
        }

        if self
            .last_ctrl_c
            .as_ref()
            .is_some_and(|at| at.elapsed() < Duration::from_secs(2))
        {
            return true;
        }
        self.last_ctrl_c = Some(Instant::now());
        self.message = "press Ctrl+C again to exit".to_string();
        false
    }

    fn toggle_pin(&mut self) -> Result<()> {
        if let Some(job) = self.selected_job() {
            pin_job(&job.id, None)?;
            self.refresh()?;
        }
        Ok(())
    }

    fn start_rename(&mut self) -> bool {
        let Some(job) = self.selected_job() else {
            self.message = "select a session to rename".to_string();
            return false;
        };
        self.input = job.title.clone();
        self.renaming = Some(job.id.clone());
        self.peek = false;
        self.help = false;
        self.message = format!("renaming {}", job.id);
        true
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
        let prompt = if self.renaming.is_some() {
            "rename selected session"
        } else if self.peek && selected.is_some() {
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
                    Row::Header {
                        label,
                        count,
                        collapsed,
                        ..
                    } => ListItem::new(Line::from(Span::styled(
                        format!(
                            "{} {} ({count})",
                            if *collapsed { "+" } else { "-" },
                            label
                        ),
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
                Paragraph::new("enter open/send/fold . space reply . ctrl+r rename . ctrl+x stop/delete/group . ctrl+s group . ctrl+t pin . ctrl+c clear/exit . ? help"),
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
        70,
    );
    let pr = pr_status_indicator(&job.pr_refs).unwrap_or_default();
    vec![
        Span::raw(format!("{icon} ")),
        Span::raw(format!("{title:<34}")),
        Span::raw(format!(" {summary:<62}")),
        Span::raw(format!(" {pr:<14}")),
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
        lines.push(Line::from(format!("prs: {}", format_pr_refs(&job.pr_refs))));
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
        "shift+up/down reorder within group . ctrl+x stop/delete row or remove group",
        "ctrl+r rename . ctrl+t pin . ctrl+s group by state/directory",
        "type a prompt to dispatch . with peek open, typed text replies to selected session",
        "ctrl+c clears input/panels; press twice to exit . esc exits or closes panels",
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
            .filter(|job| !is_ready_for_review(job) && is_terminal_status(job.status))
            .count(),
    }
}

fn state_group(label: &str, jobs: Vec<Job>) -> Group {
    Group {
        key: format!("state:{label}"),
        label: label.to_string(),
        dispatch_cwd: None,
        jobs,
    }
}

fn job_dispatch_cwd(job: &Job) -> std::path::PathBuf {
    std::path::PathBuf::from(if job.dispatch_cwd.is_empty() {
        job.cwd.clone()
    } else {
        job.dispatch_cwd.clone()
    })
}

fn is_ready_for_review(job: &Job) -> bool {
    job.status == JobStatus::Completed && !job.pr_refs.is_empty()
}

fn is_terminal_status(status: JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Completed | JobStatus::Failed | JobStatus::Stopped
    )
}

fn short_cwd(cwd: &str) -> String {
    let Ok(home) = std::env::var("HOME") else {
        return cwd.to_string();
    };
    cwd.strip_prefix(&home)
        .map(|suffix| format!("~{suffix}"))
        .unwrap_or_else(|| cwd.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentview_core::schema::{JobBackend, PrRef, ProcessState};
    use std::path::PathBuf;

    #[test]
    fn state_grouping_matches_agent_view_order() {
        let app = App {
            jobs: vec![
                job("pinned", JobStatus::Working, true, false),
                job("ready", JobStatus::Completed, false, true),
                job("needs", JobStatus::NeedsInput, false, false),
                job("working", JobStatus::Working, false, false),
                job("done", JobStatus::Completed, false, false),
                job("failed", JobStatus::Failed, false, false),
                job("stopped", JobStatus::Stopped, false, false),
            ],
            ..Default::default()
        };

        let groups = app.grouped_jobs();
        let labels: Vec<_> = groups.iter().map(|group| group.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Pinned",
                "Ready for review",
                "Needs input",
                "Working",
                "Completed"
            ]
        );

        let completed = groups
            .iter()
            .find(|group| group.label == "Completed")
            .map(|group| &group.jobs)
            .unwrap();
        assert_eq!(
            completed.iter().map(|job| job.status).collect::<Vec<_>>(),
            vec![JobStatus::Completed, JobStatus::Failed, JobStatus::Stopped]
        );
    }

    #[test]
    fn header_completed_count_excludes_ready_for_review() {
        let counts = count_jobs(&[
            job("ready", JobStatus::Completed, false, true),
            job("done", JobStatus::Completed, false, false),
            job("failed", JobStatus::Failed, false, false),
            job("stopped", JobStatus::Stopped, false, false),
        ]);

        assert_eq!(counts.completed, 3);
    }

    #[test]
    fn directory_group_dispatch_uses_selected_row_directory() {
        let mut selected = job("selected", JobStatus::Working, false, false);
        selected.cwd = "/worktree".to_string();
        selected.dispatch_cwd = "/repo".to_string();
        let mut app = App {
            jobs: vec![selected],
            group_by: GroupBy::Cwd,
            ..Default::default()
        };
        app.build_rows();
        app.selected = 1;

        assert_eq!(app.dispatch_options().cwd, Some(PathBuf::from("/repo")));
    }

    #[test]
    fn state_group_dispatch_uses_current_process_directory() {
        let mut selected = job("selected", JobStatus::Working, false, false);
        selected.dispatch_cwd = "/repo".to_string();
        let mut app = App {
            jobs: vec![selected],
            group_by: GroupBy::State,
            ..Default::default()
        };
        app.build_rows();
        app.selected = 1;

        assert_eq!(app.dispatch_options().cwd, None);
    }

    #[test]
    fn enter_on_group_header_collapses_and_expands_group() {
        let mut app = App {
            jobs: vec![job("working", JobStatus::Working, false, false)],
            ..Default::default()
        };
        app.build_rows();
        app.selected = 0;

        assert_eq!(app.rows.len(), 2);
        assert!(app.toggle_selected_group());
        assert_eq!(app.rows.len(), 1);
        assert!(matches!(
            app.rows.first(),
            Some(Row::Header {
                collapsed: true,
                ..
            })
        ));

        assert!(app.toggle_selected_group());
        assert_eq!(app.rows.len(), 2);
        assert!(matches!(
            app.rows.first(),
            Some(Row::Header {
                collapsed: false,
                ..
            })
        ));
    }

    #[test]
    fn collapsed_directory_header_can_be_dispatch_target() {
        let mut selected = job("selected", JobStatus::Working, false, false);
        selected.dispatch_cwd = "/repo".to_string();
        let mut app = App {
            jobs: vec![selected],
            group_by: GroupBy::Cwd,
            ..Default::default()
        };
        app.build_rows();
        app.selected = 0;
        assert!(app.toggle_selected_group());

        assert_eq!(app.dispatch_options().cwd, Some(PathBuf::from("/repo")));
    }

    #[test]
    fn selected_group_includes_collapsed_jobs() {
        let mut app = App {
            jobs: vec![
                job("first", JobStatus::Working, false, false),
                job("second", JobStatus::Working, false, false),
            ],
            ..Default::default()
        };
        app.build_rows();
        app.selected = 0;
        assert!(app.toggle_selected_group());

        let group = app.selected_group().unwrap();
        assert_eq!(group.label, "Working");
        assert_eq!(
            group
                .jobs
                .iter()
                .map(|job| job.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[test]
    fn ctrl_x_on_group_header_arms_bulk_remove() {
        let mut app = App {
            jobs: vec![
                job("first", JobStatus::Working, false, false),
                job("second", JobStatus::Working, false, false),
            ],
            ..Default::default()
        };
        app.build_rows();
        app.selected = 0;

        app.confirm_or_remove_group(app.selected_group().unwrap())
            .unwrap();

        assert!(matches!(
            app.last_delete,
            Some(LastDelete {
                target: DeleteTarget::Group(ref key),
                ..
            }) if key == "state:Working"
        ));
        assert_eq!(
            app.message,
            "press Ctrl+X again to remove 2 sessions in Working; dirty worktrees are refused"
        );
    }

    #[test]
    fn shift_move_swaps_jobs_only_inside_current_group() {
        let mut app = App {
            jobs: vec![
                job("first", JobStatus::Working, false, false),
                job("second", JobStatus::Working, false, false),
                job("done", JobStatus::Completed, false, false),
            ],
            ..Default::default()
        };
        app.build_rows();
        app.selected = 2;

        let order = app.swap_selected_job_within_group(-1).unwrap();
        assert_eq!(order, vec!["second".to_string(), "first".to_string()]);
        assert_eq!(app.selected, 1);

        assert!(app.swap_selected_job_within_group(-1).is_none());
    }

    #[test]
    fn shift_move_ignores_group_headers() {
        let mut app = App {
            jobs: vec![job("first", JobStatus::Working, false, false)],
            ..Default::default()
        };
        app.build_rows();
        app.selected = 0;

        assert!(app.swap_selected_job_within_group(1).is_none());
    }

    #[test]
    fn ctrl_r_starts_rename_for_selected_job() {
        let mut app = App {
            jobs: vec![job("first", JobStatus::Working, false, false)],
            peek: true,
            help: true,
            ..Default::default()
        };
        app.build_rows();
        app.selected = 1;

        assert!(app.start_rename());
        assert_eq!(app.renaming.as_deref(), Some("first"));
        assert_eq!(app.input, "first");
        assert!(!app.peek);
        assert!(!app.help);
    }

    #[test]
    fn ctrl_r_ignores_group_headers() {
        let mut app = App {
            jobs: vec![job("first", JobStatus::Working, false, false)],
            ..Default::default()
        };
        app.build_rows();
        app.selected = 0;

        assert!(!app.start_rename());
        assert!(app.renaming.is_none());
        assert_eq!(app.input, "");
        assert_eq!(app.message, "select a session to rename");
    }

    #[test]
    fn ctrl_c_clears_active_input_before_exit() {
        let mut app = App {
            input: "draft prompt".to_string(),
            peek: true,
            help: true,
            renaming: Some("first".to_string()),
            ..Default::default()
        };

        assert!(!app.handle_ctrl_c());
        assert!(app.input.is_empty());
        assert!(!app.peek);
        assert!(!app.help);
        assert!(app.renaming.is_none());
        assert_eq!(app.message, "cleared; press Ctrl+C again to exit");
    }

    #[test]
    fn ctrl_c_requires_second_press_to_exit() {
        let mut app = App::default();

        assert!(!app.handle_ctrl_c());
        assert_eq!(app.message, "press Ctrl+C again to exit");
        assert!(app.handle_ctrl_c());
    }

    fn job(id: &str, status: JobStatus, pinned: bool, pr: bool) -> Job {
        let now = now_iso();
        Job {
            id: id.to_string(),
            provider: "codex".to_string(),
            backend: JobBackend::AppServer,
            codex_thread_id: Some(format!("thread-{id}")),
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
            status,
            process_state: ProcessState::Exited,
            pid: None,
            active_worker_pid: None,
            pinned,
            manual_order: None,
            archived: false,
            deleted: false,
            last_summary: None,
            last_output: None,
            blocking_request: None,
            pr_refs: if pr {
                vec![PrRef {
                    url: "https://github.com/acme/repo/pull/1".to_string(),
                    owner: "acme".to_string(),
                    repo: "repo".to_string(),
                    number: 1,
                    status: "unknown".to_string(),
                }]
            } else {
                Vec::new()
            },
            created_at: now.clone(),
            updated_at: now,
            completed_at: None,
            exit_code: None,
            error: None,
        }
    }
}
