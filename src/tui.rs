//! Interactive TUI: pick a task, run it with live per-task progress and a
//! scrollable log pane.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::error::{Error, Result};
use crate::events::{RunObserver, Stream, TaskStatus};
use crate::runner::RunOptions;
use crate::section::Section;
use crate::{Config, Runner};

/// Settings for [`run_tui`].
#[derive(Debug, Clone, Default)]
pub struct TuiOptions {
    pub lang_filter: Option<String>,
    pub dry_run: bool,
    pub env: Vec<String>,
    pub dir: Option<PathBuf>,
    pub show_all: bool,
    pub parallel: bool,
    pub jobs: Option<usize>,
    pub keep_going: bool,
    pub no_cache: bool,
    /// Answer yes to every `prompt`; otherwise tasks with a prompt cannot
    /// run inside the TUI (use `x` to run them in the terminal).
    pub yes: bool,
    /// Show each code block in the log before it runs.
    pub verbose: bool,
}

const MAX_LOG_LINES: usize = 20_000;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// ---------------------------------------------------------------------------
// Run events
// ---------------------------------------------------------------------------

/// Messages from the run thread to the UI.
enum UiEvent {
    Plan(Vec<String>),
    Graph(Vec<(String, Vec<String>)>),
    Output {
        task: String,
        stream: Stream,
        line: String,
    },
    Status {
        task: String,
        status: TaskStatus,
    },
    Finished(std::result::Result<(), String>),
}

struct TuiObserver {
    tx: Mutex<Sender<UiEvent>>,
}

impl TuiObserver {
    fn send(&self, event: UiEvent) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(event);
        }
    }
}

impl RunObserver for TuiObserver {
    fn plan(&self, tasks: &[String]) {
        self.send(UiEvent::Plan(tasks.to_vec()));
    }

    fn graph(&self, edges: &[(String, Vec<String>)]) {
        self.send(UiEvent::Graph(edges.to_vec()));
    }

    fn output(&self, task: &str, stream: Stream, line: &str) {
        self.send(UiEvent::Output {
            task: task.to_string(),
            stream,
            line: line.to_string(),
        });
    }

    fn status(&self, task: &str, status: &TaskStatus) {
        self.send(UiEvent::Status {
            task: task.to_string(),
            status: status.clone(),
        });
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum TaskState {
    Pending,
    Running,
    Retrying(u32, u32),
    Succeeded(Duration),
    Skipped(String),
    Failed(String),
    Cancelled(String),
}

struct TaskRow {
    name: String,
    state: TaskState,
    /// Indices (into `RunView::tasks`) of the tasks this one depends on.
    deps: Vec<usize>,
    /// When the task last started, for the live elapsed time in the graph.
    started: Option<Instant>,
}

struct LogLine {
    task: usize,
    stream: Stream,
    text: String,
}

/// Progress of one run shown in the run view.
struct RunView {
    task: String,
    tasks: Vec<TaskRow>,
    log: Vec<LogLine>,
    selected: usize,
    /// Show the combined log instead of only the selected task's.
    all_logs: bool,
    /// Draw the dependency graph instead of the flat task list.
    graph: bool,
    /// Lines scrolled up from the bottom of the log.
    scroll: usize,
    result: Option<std::result::Result<(), String>>,
    rx: Receiver<UiEvent>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl RunView {
    fn index_of(&mut self, name: &str) -> usize {
        if let Some(i) = self.tasks.iter().position(|t| t.name == name) {
            return i;
        }
        self.tasks.push(TaskRow {
            name: name.to_string(),
            state: TaskState::Pending,
            deps: Vec::new(),
            started: None,
        });
        self.tasks.len() - 1
    }

    fn apply(&mut self, event: UiEvent) {
        match event {
            UiEvent::Plan(names) => {
                for name in names {
                    self.index_of(&name);
                }
            }
            UiEvent::Graph(edges) => {
                for (task, deps) in edges {
                    let i = self.index_of(&task);
                    let deps: Vec<usize> = deps.iter().map(|d| self.index_of(d)).collect();
                    self.tasks[i].deps = deps;
                }
            }
            UiEvent::Output { task, stream, line } => {
                let task = self.index_of(&task);
                self.log.push(LogLine {
                    task,
                    stream,
                    text: line,
                });
                if self.log.len() > MAX_LOG_LINES {
                    self.log.drain(..MAX_LOG_LINES / 10);
                }
            }
            UiEvent::Status { task, status } => {
                let i = self.index_of(&task);
                self.tasks[i].state = match status {
                    TaskStatus::Started { .. } => {
                        self.tasks[i].started = Some(Instant::now());
                        TaskState::Running
                    }
                    TaskStatus::Retrying { attempt, max, .. } => TaskState::Retrying(attempt, max),
                    TaskStatus::Succeeded { elapsed } => TaskState::Succeeded(elapsed),
                    TaskStatus::Skipped { reason } => TaskState::Skipped(reason),
                    TaskStatus::Failed { error } => {
                        self.log.push(LogLine {
                            task: i,
                            stream: Stream::Stderr,
                            text: format!("error: {}", error),
                        });
                        TaskState::Failed(error)
                    }
                    TaskStatus::Cancelled { reason } => TaskState::Cancelled(reason),
                };
            }
            UiEvent::Finished(result) => self.result = Some(result),
        }
    }

    fn drain(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.apply(event);
        }
    }

    fn is_running(&self) -> bool {
        self.result.is_none()
    }

    /// The graph is only worth drawing once the run has dependency edges.
    fn graph_active(&self) -> bool {
        self.graph && self.tasks.iter().any(|t| !t.deps.is_empty())
    }

    /// Move the selection. In the graph `dy` walks a column and `dx` hops
    /// between columns; the flat list only has the vertical axis.
    fn move_selection(&mut self, dx: isize, dy: isize) {
        let n = self.tasks.len();
        if n == 0 {
            return;
        }
        let from = self.selected.min(n - 1);
        let to = if self.graph_active() {
            graph_move(&graph_layout(&self.tasks), from, dx, dy)
        } else {
            match dy {
                1 => (from + 1) % n,
                -1 => (from + n - 1) % n,
                _ => from,
            }
        };
        if to != self.selected {
            self.selected = to;
            self.scroll = 0;
        }
    }

    fn visible_log(&self) -> Vec<&LogLine> {
        self.log
            .iter()
            .filter(|l| self.all_logs || l.task == self.selected)
            .collect()
    }
}

enum Mode {
    List,
    Filter,
    Args { input: String, terminal: bool },
    Run,
}

/// What `run_app` asks the caller to do after the UI closes.
enum Exit {
    Quit,
    /// Leave the TUI and run in the terminal (interactive stdin works there).
    Terminal {
        task: String,
        args: Vec<String>,
    },
}

struct App {
    sections: Vec<Section>,
    filter: String,
    /// Indices into `sections` matching `filter`.
    visible: Vec<usize>,
    list_state: ListState,
    mode: Mode,
    run: Option<RunView>,
    tick: usize,
    path: PathBuf,
    config: Config,
    opts: TuiOptions,
}

impl App {
    fn new(sections: Vec<Section>, path: PathBuf, config: Config, opts: TuiOptions) -> Self {
        let mut app = Self {
            sections,
            filter: String::new(),
            visible: Vec::new(),
            list_state: ListState::default(),
            mode: Mode::List,
            run: None,
            tick: 0,
            path,
            config,
            opts,
        };
        app.refilter();
        app
    }

    fn refilter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.visible = self
            .sections
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                needle.is_empty()
                    || s.title.to_lowercase().contains(&needle)
                    || s.aliases.iter().any(|a| a.to_lowercase().contains(&needle))
                    || s.description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&needle))
            })
            .map(|(i, _)| i)
            .collect();
        self.list_state.select(if self.visible.is_empty() {
            None
        } else {
            Some(0)
        });
    }

    fn next(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        let i = self
            .list_state
            .selected()
            .map_or(0, |i| (i + 1) % self.visible.len());
        self.list_state.select(Some(i));
    }

    fn previous(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(0) | None => self.visible.len() - 1,
            Some(i) => i - 1,
        };
        self.list_state.select(Some(i));
    }

    fn selected_section(&self) -> Option<&Section> {
        self.list_state
            .selected()
            .and_then(|i| self.visible.get(i))
            .and_then(|&i| self.sections.get(i))
    }

    fn needs_args(section: &Section) -> bool {
        section.params.iter().any(|p| p.default.is_none())
    }

    /// Run the selected task: prompt for arguments if it has required
    /// params (or `force_prompt`), otherwise start right away.
    fn begin(&mut self, terminal: bool, force_prompt: bool) -> Option<Exit> {
        let section = self.selected_section()?;
        if force_prompt || Self::needs_args(section) {
            self.mode = Mode::Args {
                input: String::new(),
                terminal,
            };
            return None;
        }
        self.launch(terminal, Vec::new())
    }

    fn launch(&mut self, terminal: bool, args: Vec<String>) -> Option<Exit> {
        let task = self.selected_section()?.title.clone();
        // Dry-run prints straight to stdout, which would garble the screen.
        if terminal || self.opts.dry_run {
            return Some(Exit::Terminal { task, args });
        }
        self.start_run(task, args);
        None
    }

    fn start_run(&mut self, task: String, args: Vec<String>) {
        let (tx, rx) = mpsc::channel();
        let observer = Arc::new(TuiObserver { tx: Mutex::new(tx) });
        let cancel = Arc::new(AtomicBool::new(false));

        let handle = {
            let config = self.config.clone();
            let path = self.path.clone();
            let opts = self.opts.clone();
            let task = task.clone();
            let cancel = Arc::clone(&cancel);
            let observer = Arc::clone(&observer);
            std::thread::spawn(move || {
                let result = (|| -> Result<()> {
                    let mut runner = Runner::new(config);
                    runner.set_env_overrides(Runner::parse_env_overrides(&opts.env)?);
                    runner.set_working_dir(opts.dir.clone());
                    runner.set_use_cache(!opts.no_cache);
                    runner.set_assume_yes(opts.yes);
                    runner.set_verbose(opts.verbose);
                    runner.run_task_with_options(
                        &path,
                        &task,
                        RunOptions {
                            args,
                            lang_filter: opts.lang_filter.clone(),
                            parallel: opts.parallel,
                            jobs: opts.jobs,
                            keep_going: opts.keep_going,
                            observer: Some(observer.clone() as Arc<dyn RunObserver>),
                            cancel: Some(cancel),
                            ..Default::default()
                        },
                    )
                })();
                observer.send(UiEvent::Finished(result.map_err(|e| e.to_string())));
            })
        };

        self.run = Some(RunView {
            task,
            tasks: Vec::new(),
            log: Vec::new(),
            selected: 0,
            all_logs: true,
            graph: true,
            scroll: 0,
            result: None,
            rx,
            cancel,
            handle: Some(handle),
        });
        self.mode = Mode::Run;
    }

    fn close_run(&mut self) {
        if let Some(mut run) = self.run.take()
            && let Some(handle) = run.handle.take()
        {
            let _ = handle.join();
        }
        self.mode = Mode::List;
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Exit> {
        let ctrl_c = modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c');
        match &mut self.mode {
            Mode::List => match code {
                _ if ctrl_c => return Some(Exit::Quit),
                KeyCode::Char('q') => return Some(Exit::Quit),
                KeyCode::Esc => {
                    if self.filter.is_empty() {
                        return Some(Exit::Quit);
                    }
                    self.filter.clear();
                    self.refilter();
                }
                KeyCode::Down | KeyCode::Char('j') => self.next(),
                KeyCode::Up | KeyCode::Char('k') => self.previous(),
                KeyCode::Char('/') => self.mode = Mode::Filter,
                KeyCode::Enter => return self.begin(false, false),
                KeyCode::Char('a') => return self.begin(false, true),
                KeyCode::Char('x') => return self.begin(true, false),
                _ => {}
            },
            Mode::Filter => match code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.refilter();
                    self.mode = Mode::List;
                }
                KeyCode::Enter => self.mode = Mode::List,
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.refilter();
                }
                KeyCode::Down => self.next(),
                KeyCode::Up => self.previous(),
                KeyCode::Char(c) if !ctrl_c => {
                    self.filter.push(c);
                    self.refilter();
                }
                _ => {}
            },
            Mode::Args { input, terminal } => match code {
                KeyCode::Esc => self.mode = Mode::List,
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Enter => {
                    let args = split_args(input);
                    let terminal = *terminal;
                    self.mode = Mode::List;
                    return self.launch(terminal, args);
                }
                KeyCode::Char(c) if !ctrl_c => input.push(c),
                _ => {}
            },
            Mode::Run => {
                let Some(run) = self.run.as_mut() else {
                    self.mode = Mode::List;
                    return None;
                };
                match code {
                    _ if ctrl_c => {
                        run.cancel.store(true, Ordering::SeqCst);
                    }
                    KeyCode::Char('q') | KeyCode::Esc => {
                        if run.is_running() {
                            run.cancel.store(true, Ordering::SeqCst);
                        } else {
                            self.close_run();
                        }
                    }
                    KeyCode::Enter if !run.is_running() => self.close_run(),
                    KeyCode::Char('r') if !run.is_running() => {
                        let task = run.task.clone();
                        self.close_run();
                        // Re-run without re-prompting: arguments are not kept.
                        self.start_run(task, Vec::new());
                    }
                    KeyCode::Down | KeyCode::Char('j') => run.move_selection(0, 1),
                    KeyCode::Up | KeyCode::Char('k') => run.move_selection(0, -1),
                    KeyCode::Right | KeyCode::Char('l') => run.move_selection(1, 0),
                    KeyCode::Left | KeyCode::Char('h') => run.move_selection(-1, 0),
                    KeyCode::Char('v') => run.graph = !run.graph,
                    KeyCode::Tab => {
                        run.all_logs = !run.all_logs;
                        run.scroll = 0;
                    }
                    KeyCode::PageUp => run.scroll = run.scroll.saturating_add(10),
                    KeyCode::PageDown => run.scroll = run.scroll.saturating_sub(10),
                    KeyCode::Char('g') => run.scroll = usize::MAX / 2,
                    KeyCode::Char('G') => run.scroll = 0,
                    _ => {}
                }
            }
        }
        None
    }
}

/// Split an argument line on whitespace, honoring single and double quotes.
fn split_args(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;
    for c in input.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has_token = true;
            }
            (None, c) if c.is_whitespace() => {
                if has_token || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has_token || !cur.is_empty() {
        args.push(cur);
    }
    args
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the interactive TUI for task selection
pub fn run_tui(markdown_path: PathBuf, config: Config, opts: TuiOptions) -> Result<()> {
    let sections: Vec<Section> = {
        let mut r = Runner::new(config.clone());
        r.list_task_sections(&markdown_path)?
            .into_iter()
            .filter(|s| opts.show_all || !s.private)
            .filter(|s| {
                opts.lang_filter
                    .as_ref()
                    .is_none_or(|lang| s.codes.iter().any(|c| c.lang == *lang))
            })
            .collect()
    };

    enable_raw_mode().map_err(Error::Io)?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture).map_err(Error::Io)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(Error::Io)?;

    let mut app = App::new(
        sections,
        markdown_path.clone(),
        config.clone(),
        opts.clone(),
    );
    let result = run_app(&mut terminal, &mut app);

    // Cancel any run still in flight so the process can exit cleanly.
    if let Some(run) = app.run.take() {
        run.cancel.store(true, Ordering::SeqCst);
        if let Some(handle) = run.handle {
            let _ = handle.join();
        }
    }

    disable_raw_mode().map_err(Error::Io)?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .map_err(Error::Io)?;
    terminal.show_cursor().map_err(Error::Io)?;

    if let Exit::Terminal { task, args } = result? {
        let mut runner = Runner::new(config);
        runner.set_dry_run(opts.dry_run);
        runner.set_env_overrides(Runner::parse_env_overrides(&opts.env)?);
        runner.set_working_dir(opts.dir);
        runner.set_use_cache(!opts.no_cache);
        runner.set_assume_yes(opts.yes);
        runner.set_verbose(opts.verbose);
        runner.run_task_with_options(
            &markdown_path,
            &task,
            RunOptions {
                args,
                lang_filter: opts.lang_filter,
                parallel: opts.parallel,
                jobs: opts.jobs,
                keep_going: opts.keep_going,
                ..Default::default()
            },
        )?;
    }

    Ok(())
}

fn run_app<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<Exit>
where
    B::Error: Send + Sync + 'static,
{
    loop {
        if let Some(run) = app.run.as_mut() {
            run.drain();
        }
        app.tick = app.tick.wrapping_add(1);

        terminal
            .draw(|f| ui(f, app))
            .map_err(|e| Error::Io(io::Error::other(e)))?;

        if event::poll(Duration::from_millis(60)).map_err(Error::Io)?
            && let Event::Key(key) = event::read().map_err(Error::Io)?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if let Some(exit) = app.handle_key(key.code, key.modifiers) {
                return Ok(exit);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn ui(f: &mut ratatui::Frame, app: &mut App) {
    if matches!(app.mode, Mode::Run) && app.run.is_some() {
        draw_run(f, app);
    } else {
        draw_list(f, app);
    }
}

fn title_style() -> Style {
    Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD)
}

fn draw_list(f: &mut ratatui::Frame, app: &mut App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(f.area());

    // Search / prompt bar
    let (bar_title, bar_text) = match &app.mode {
        Mode::Filter => (" Search ", format!("/{}▏", app.filter)),
        Mode::Args { input, .. } => {
            let hint = app
                .selected_section()
                .map(|s| {
                    s.params
                        .iter()
                        .map(|p| match &p.default {
                            Some(d) => format!("{}={}", p.name, d),
                            None => format!("{}*", p.name),
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            (
                " Arguments ",
                if hint.is_empty() {
                    format!("{}▏", input)
                } else {
                    format!("{}▏   params: {}", input, hint)
                },
            )
        }
        _ if !app.filter.is_empty() => (" Filter ", format!("/{}", app.filter)),
        _ => (" Filter ", "press / to search".to_string()),
    };
    f.render_widget(
        Paragraph::new(bar_text)
            .style(Style::default().fg(Color::Gray))
            .block(Block::default().borders(Borders::ALL).title(bar_title)),
        rows[0],
    );

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(rows[1]);

    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|&i| {
            let s = &app.sections[i];
            let mut spans = vec![Span::styled(s.title.clone(), title_style())];
            if !s.aliases.is_empty() {
                spans.push(Span::styled(
                    format!(" ({})", s.aliases.join(", ")),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            if s.parallel {
                spans.push(Span::styled(" ∥", Style::default().fg(Color::Cyan)));
            }
            if !s.depends.is_empty() {
                spans.push(Span::styled(
                    format!(" ←{}", s.depends.len()),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Tasks ({}) ", app.visible.len())),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, cols[0], &mut app.list_state);

    // Detail pane
    let detail = match app.selected_section() {
        Some(s) => section_details(s),
        None => vec![Line::from("No tasks")],
    };
    f.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Details ")),
        cols[1],
    );

    let help = match app.mode {
        Mode::Filter => "type to filter   Enter: accept   Esc: clear",
        Mode::Args { .. } => "Enter: run   Esc: cancel   (quote args with \"…\")",
        _ => {
            "↑/↓ j/k: move   Enter: run   a: run with args   x: run in terminal   /: search   q: quit"
        }
    };
    f.render_widget(
        Paragraph::new(help)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" Help ")),
        rows[2],
    );
}

fn section_details(s: &Section) -> Vec<Line<'static>> {
    let label = |text: &str| Span::styled(format!("{:<9}", text), Style::default().fg(Color::Cyan));
    let mut lines = vec![Line::from(Span::styled(s.title.clone(), title_style()))];

    if let Some(desc) = s
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        lines.push(Line::from(desc.to_string()));
    }
    lines.push(Line::raw(""));

    if !s.depends.is_empty() {
        lines.push(Line::from(vec![
            label("depends"),
            Span::raw(s.depends.join(", ")),
        ]));
    }
    if !s.params.is_empty() {
        let params = crate::section::ParamDef::describe_all(&s.params);
        lines.push(Line::from(vec![label("params"), Span::raw(params)]));
    }
    let mut flags = Vec::new();
    if s.parallel {
        flags.push("parallel deps".to_string());
    }
    if let Some(c) = &s.condition {
        flags.push(format!("if {}", c));
    }
    if s.retries > 0 {
        flags.push(format!("retries {}", s.retries));
    }
    if let Some(t) = s.timeout {
        flags.push(format!("timeout {}s", t.as_secs_f64()));
    }
    if s.capture {
        flags.push("captures output".into());
    }
    if !s.sources.is_empty() {
        flags.push("cached on sources".into());
    }
    if !s.status.is_empty() {
        flags.push("status checks".into());
    }
    if let Some(spec) = &s.for_each {
        flags.push(format!("for {}", spec.summary()));
    }
    if !s.defer.is_empty() {
        flags.push(format!("defer {}", s.defer.join(", ")));
    }
    if s.once {
        flags.push("once".into());
    }
    if !s.requires.is_empty() {
        flags.push(format!("requires {}", s.requires.join(", ")));
    }
    if !s.vars.is_empty() {
        let names: Vec<&str> = s.vars.iter().map(|v| v.name.as_str()).collect();
        flags.push(format!("vars {}", names.join(", ")));
    }
    if s.prompt.is_some() {
        flags.push("asks to confirm".into());
    }
    if s.ignore_error {
        flags.push("ignores errors".into());
    }
    if !flags.is_empty() {
        lines.push(Line::from(vec![
            label("options"),
            Span::raw(flags.join(" · ")),
        ]));
    }
    if let Some(dir) = &s.dir {
        lines.push(Line::from(vec![
            label("dir"),
            Span::raw(dir.display().to_string()),
        ]));
    }
    if !s.env.is_empty() {
        let env = s
            .env
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(Line::from(vec![label("env"), Span::raw(env)]));
    }

    for block in &s.codes {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!("[{}]", block.lang),
            Style::default().fg(Color::Yellow),
        )));
        for (n, line) in block.code.lines().enumerate() {
            if n >= 12 {
                lines.push(Line::from(Span::styled(
                    "…",
                    Style::default().fg(Color::DarkGray),
                )));
                break;
            }
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::Gray),
            )));
        }
    }
    lines
}

fn state_span(state: &TaskState, tick: usize) -> Span<'static> {
    match state {
        TaskState::Pending => Span::styled("○", Style::default().fg(Color::DarkGray)),
        TaskState::Running => Span::styled(
            SPINNER[(tick / 2) % SPINNER.len()],
            Style::default().fg(Color::Cyan),
        ),
        TaskState::Retrying(..) => Span::styled("↻", Style::default().fg(Color::Yellow)),
        TaskState::Succeeded(_) => Span::styled("✔", Style::default().fg(Color::Green)),
        TaskState::Skipped(_) => Span::styled("⊘", Style::default().fg(Color::DarkGray)),
        TaskState::Failed(_) => Span::styled("✘", Style::default().fg(Color::Red)),
        TaskState::Cancelled(_) => Span::styled("⊘", Style::default().fg(Color::Yellow)),
    }
}

fn state_note(state: &TaskState) -> String {
    match state {
        TaskState::Succeeded(d) => format!(" {:.1}s", d.as_secs_f64()),
        TaskState::Retrying(a, m) => format!(" retry {}/{}", a, m),
        TaskState::Skipped(r) => format!(" {}", r),
        TaskState::Cancelled(r) => format!(" {}", r),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Dependency graph
// ---------------------------------------------------------------------------

/// Rows a node box occupies, and the pitch between stacked boxes.
const NODE_H: usize = 3;
const ROW_PITCH: usize = NODE_H + 1;
/// Blank columns between node boxes, where the connectors run.
const COL_GAP: usize = 4;
const MAX_NAME_W: usize = 28;
/// Width reserved for the status note (`12.3s`, `↻ 1/3`, `skip`).
const NOTE_W: usize = 6;

/// Stage layout of the run: a task sits one column right of its deepest
/// dependency, like the stages of a GitHub Actions workflow graph.
struct GraphLayout {
    /// Task indices of each column, top to bottom.
    cols: Vec<Vec<usize>>,
    /// `(column, row)` of every task.
    pos: Vec<(usize, usize)>,
}

fn graph_layout(tasks: &[TaskRow]) -> GraphLayout {
    let n = tasks.len();
    let mut depth = vec![0usize; n];
    // Longest path from a root. Plans are acyclic; the pass cap only guards
    // against malformed input.
    for _ in 0..n {
        let mut changed = false;
        for (i, t) in tasks.iter().enumerate() {
            for &d in &t.deps {
                if d != i && depth[i] < depth[d] + 1 {
                    depth[i] = depth[d] + 1;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    let ncols = depth.iter().max().map_or(0, |d| d + 1);
    let mut cols: Vec<Vec<usize>> = vec![Vec::new(); ncols];
    for (i, d) in depth.iter().enumerate() {
        cols[*d].push(i);
    }

    // Order each column by the mean row of its dependencies so connectors
    // cross as little as possible; ties keep plan order.
    let mut row_of: Vec<Option<usize>> = vec![None; n];
    for col in &mut cols {
        let key = |i: usize| -> usize {
            let rows: Vec<usize> = tasks[i].deps.iter().filter_map(|&d| row_of[d]).collect();
            if rows.is_empty() {
                0
            } else {
                rows.iter().sum::<usize>() * 1000 / rows.len()
            }
        };
        col.sort_by_key(|&i| (key(i), i));
        for (r, &i) in col.iter().enumerate() {
            row_of[i] = Some(r);
        }
    }

    let mut pos = vec![(0, 0); n];
    for (c, col) in cols.iter().enumerate() {
        for (r, &i) in col.iter().enumerate() {
            pos[i] = (c, r);
        }
    }
    GraphLayout { cols, pos }
}

/// Selection target after moving `dy` within a column or `dx` between columns.
fn graph_move(layout: &GraphLayout, from: usize, dx: isize, dy: isize) -> usize {
    let Some(&(c, r)) = layout.pos.get(from) else {
        return from;
    };
    if dy != 0 {
        let col = &layout.cols[c];
        let r = (r as isize + dy).rem_euclid(col.len() as isize) as usize;
        return col[r];
    }
    let nc = c as isize + dx;
    if nc < 0 || nc as usize >= layout.cols.len() {
        return from;
    }
    let col = &layout.cols[nc as usize];
    col[r.min(col.len() - 1)]
}

fn text_width(s: &str) -> usize {
    Span::raw(s).width()
}

/// Truncate `s` to at most `max` columns, ending in `…` when cut.
fn fit(s: &str, max: usize) -> String {
    if text_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for c in s.chars() {
        let mut next = out.clone();
        next.push(c);
        if text_width(&next) + 1 > max {
            break;
        }
        out = next;
    }
    out.push('…');
    out
}

fn graph_node_width(tasks: &[TaskRow]) -> usize {
    let name_w = tasks
        .iter()
        .map(|t| text_width(&t.name))
        .max()
        .unwrap_or(0)
        .min(MAX_NAME_W);
    // border, pad, icon, space, name, space, note, pad, border
    name_w + NOTE_W + 7
}

/// Total size of the drawn graph in cells.
fn graph_size(layout: &GraphLayout, node_w: usize) -> (usize, usize) {
    let cols = layout.cols.len();
    let rows = layout.cols.iter().map(Vec::len).max().unwrap_or(0);
    (
        cols * node_w + cols.saturating_sub(1) * COL_GAP,
        (rows * ROW_PITCH).saturating_sub(ROW_PITCH - NODE_H),
    )
}

const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

fn connector(mask: u8) -> &'static str {
    match mask {
        0 => " ",
        m if m == UP | DOWN || m == UP || m == DOWN => "│",
        m if m == LEFT | RIGHT || m == LEFT || m == RIGHT => "─",
        m if m == DOWN | RIGHT => "┌",
        m if m == DOWN | LEFT => "┐",
        m if m == UP | RIGHT => "└",
        m if m == UP | LEFT => "┘",
        m if m == UP | DOWN | RIGHT => "├",
        m if m == UP | DOWN | LEFT => "┤",
        m if m == LEFT | RIGHT | DOWN => "┬",
        m if m == LEFT | RIGHT | UP => "┴",
        _ => "┼",
    }
}

/// Draw an orthogonal polyline into the connector canvas.
fn draw_path(canvas: &mut [Vec<u8>], points: &[(usize, usize)]) {
    for pair in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        let (toward, back) = if y0 == y1 {
            if x1 >= x0 {
                (RIGHT, LEFT)
            } else {
                (LEFT, RIGHT)
            }
        } else if y1 > y0 {
            (DOWN, UP)
        } else {
            (UP, DOWN)
        };
        let cells: Vec<(usize, usize)> = if y0 == y1 {
            let (a, b) = (x0.min(x1), x0.max(x1));
            let mut xs: Vec<usize> = (a..=b).collect();
            if x1 < x0 {
                xs.reverse();
            }
            xs.into_iter().map(|x| (x, y0)).collect()
        } else {
            let (a, b) = (y0.min(y1), y0.max(y1));
            let mut ys: Vec<usize> = (a..=b).collect();
            if y1 < y0 {
                ys.reverse();
            }
            ys.into_iter().map(|y| (x0, y)).collect()
        };
        let last = cells.len() - 1;
        for (k, &(x, y)) in cells.iter().enumerate() {
            if k > 0 {
                canvas[y][x] |= back;
            }
            if k < last {
                canvas[y][x] |= toward;
            }
        }
    }
}

/// Short status note shown at the right of a node.
fn node_note(t: &TaskRow) -> String {
    match &t.state {
        TaskState::Pending => String::new(),
        TaskState::Running => t
            .started
            .map(|s| format!("{:.1}s", s.elapsed().as_secs_f64()))
            .unwrap_or_default(),
        TaskState::Retrying(a, m) => format!("↻ {}/{}", a, m),
        TaskState::Succeeded(d) => format!("{:.1}s", d.as_secs_f64()),
        TaskState::Skipped(_) => "skip".into(),
        TaskState::Failed(_) => "fail".into(),
        TaskState::Cancelled(_) => "cancel".into(),
    }
}

fn node_color(state: &TaskState) -> Color {
    match state {
        TaskState::Pending | TaskState::Skipped(_) => Color::DarkGray,
        TaskState::Running => Color::Cyan,
        TaskState::Retrying(..) | TaskState::Cancelled(_) => Color::Yellow,
        TaskState::Succeeded(_) => Color::Green,
        TaskState::Failed(_) => Color::Red,
    }
}

/// Write `text` at graph cell `(x, y)`, shifted by the scroll `offset` and
/// clipped to `area`.
fn put(
    buf: &mut Buffer,
    area: Rect,
    offset: (usize, usize),
    x: usize,
    y: usize,
    text: &str,
    style: Style,
) {
    if y < offset.1 || y - offset.1 >= area.height as usize || x < offset.0 {
        return;
    }
    let sx = x - offset.0;
    if sx >= area.width as usize {
        return;
    }
    let room = area.width as usize - sx;
    buf.set_string(
        area.x + sx as u16,
        area.y + (y - offset.1) as u16,
        fit(text, room),
        style,
    );
}

fn render_graph(buf: &mut Buffer, area: Rect, run: &RunView, layout: &GraphLayout, tick: usize) {
    if area.width == 0 || area.height == 0 || run.tasks.is_empty() {
        return;
    }
    let node_w = graph_node_width(&run.tasks);
    let pitch = node_w + COL_GAP;
    let (total_w, total_h) = graph_size(layout, node_w);
    let selected = run.selected.min(run.tasks.len() - 1);

    // Keep the selected node centred when the graph is larger than the pane.
    let (sc, sr) = layout.pos[selected];
    let scroll = |sel: usize, size: usize, view: usize, total: usize| {
        if total <= view {
            0
        } else {
            (sel + size / 2).saturating_sub(view / 2).min(total - view)
        }
    };
    let offset = (
        scroll(sc * pitch, node_w, area.width as usize, total_w),
        scroll(sr * ROW_PITCH, NODE_H, area.height as usize, total_h),
    );

    // Connectors first, so boxes drawn afterwards cover the ends that tuck
    // under their borders. An edge to the next column runs straight to a
    // trunk in the gap before the dependent; edges into the same task share
    // that trunk. An edge spanning more columns drops into the blank row
    // below its source and travels there, so it never passes through a box.
    let mut canvas = vec![vec![0u8; total_w]; total_h];
    let mid = |row: usize| row * ROW_PITCH + 1;
    for (i, t) in run.tasks.iter().enumerate() {
        let (c, r) = layout.pos[i];
        let (x_in, y_in) = (c * pitch, mid(r));
        if x_in < 2 {
            continue;
        }
        let trunk = x_in - 2;
        for &d in &t.deps {
            let (dc, dr) = layout.pos[d];
            let (x_out, y_out) = (dc * pitch + node_w, mid(dr));
            if dc >= c || x_out > trunk {
                continue;
            }
            let mut path = vec![(x_out - 1, y_out)];
            if dc + 1 < c {
                let lane = y_out + 2;
                path.extend([(x_out, y_out), (x_out, lane), (trunk, lane)]);
            } else {
                path.push((trunk, y_out));
            }
            path.extend([(trunk, y_in), (x_in, y_in)]);
            draw_path(&mut canvas, &path);
        }
    }
    let wire = Style::default().fg(Color::DarkGray);
    for (y, row) in canvas.iter().enumerate() {
        for (x, &mask) in row.iter().enumerate() {
            if mask != 0 {
                put(buf, area, offset, x, y, connector(mask), wire);
            }
        }
    }

    let name_w = node_w - NOTE_W - 7;
    for (i, t) in run.tasks.iter().enumerate() {
        let (c, r) = layout.pos[i];
        let (x, y) = (c * pitch, r * ROW_PITCH);
        let is_selected = i == selected;
        let color = node_color(&t.state);
        let mut edge = Style::default().fg(color);
        if is_selected {
            edge = edge.add_modifier(Modifier::BOLD);
        }
        let (tl, tr, bl, br, h, v) = if is_selected {
            ("┏", "┓", "┗", "┛", "━", "┃")
        } else {
            ("┌", "┐", "└", "┘", "─", "│")
        };
        let bar = h.repeat(node_w - 2);
        put(buf, area, offset, x, y, &format!("{tl}{bar}{tr}"), edge);
        put(buf, area, offset, x, y + 1, v, edge);
        put(buf, area, offset, x + node_w - 1, y + 1, v, edge);
        put(buf, area, offset, x, y + 2, &format!("{bl}{bar}{br}"), edge);

        let icon = state_span(&t.state, tick);
        let name = fit(&t.name, name_w);
        let pad = name_w - text_width(&name);
        let note = node_note(t);
        let note_pad = NOTE_W - text_width(&fit(&note, NOTE_W));
        let mut name_style = Style::default();
        if is_selected {
            name_style = name_style.add_modifier(Modifier::BOLD);
        }
        let inner = format!(
            " {} {}{} {}{} ",
            icon.content,
            name,
            " ".repeat(pad),
            " ".repeat(note_pad),
            fit(&note, NOTE_W)
        );
        put(buf, area, offset, x + 1, y + 1, &inner, name_style);
        // Re-paint the icon and note in their state colors.
        put(buf, area, offset, x + 2, y + 1, &icon.content, icon.style);
        let note_x = x + node_w - 2 - NOTE_W;
        put(
            buf,
            area,
            offset,
            note_x,
            y + 1,
            &format!("{}{}", " ".repeat(note_pad), fit(&note, NOTE_W)),
            Style::default().fg(Color::DarkGray),
        );
    }
}

fn draw_run(f: &mut ratatui::Frame, app: &mut App) {
    let tick = app.tick;
    let Some(run) = app.run.as_ref() else { return };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(f.area());

    let (status_text, status_color) = match &run.result {
        None => (
            format!("{} running", SPINNER[(tick / 2) % SPINNER.len()]),
            Color::Cyan,
        ),
        Some(Ok(())) => ("✔ finished".to_string(), Color::Green),
        Some(Err(e)) => (format!("✘ {}", e), Color::Red),
    };
    let done = run
        .tasks
        .iter()
        .filter(|t| {
            matches!(
                t.state,
                TaskState::Succeeded(_) | TaskState::Skipped(_) | TaskState::Failed(_)
            )
        })
        .count();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(run.task.clone(), title_style()),
            Span::raw("  "),
            Span::styled(status_text, Style::default().fg(status_color)),
            Span::styled(
                format!("  {}/{} tasks", done, run.tasks.len()),
                Style::default().fg(Color::DarkGray),
            ),
        ]))
        .block(Block::default().borders(Borders::ALL).title(" Run ")),
        rows[0],
    );

    if run.graph_active() {
        let layout = graph_layout(&run.tasks);
        let (_, graph_h) = graph_size(&layout, graph_node_width(&run.tasks));
        // Leave at least two fifths of the body for the log.
        let max_h = (rows[1].height * 3 / 5).max(5);
        let height = (graph_h as u16 + 2).clamp(5, max_h).min(rows[1].height);
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(height), Constraint::Min(3)])
            .split(rows[1]);
        let block = Block::default().borders(Borders::ALL).title(" Graph ");
        let inner = block.inner(parts[0]);
        f.render_widget(block, parts[0]);
        render_graph(f.buffer_mut(), inner, run, &layout, tick);
        f.render_widget(log_widget(run, parts[1]), parts[1]);
    } else {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(rows[1]);

        let items: Vec<ListItem> = run
            .tasks
            .iter()
            .map(|t| {
                ListItem::new(Line::from(vec![
                    state_span(&t.state, tick),
                    Span::raw(" "),
                    Span::raw(t.name.clone()),
                    Span::styled(state_note(&t.state), Style::default().fg(Color::DarkGray)),
                ]))
            })
            .collect();
        let mut state = ListState::default();
        if !run.tasks.is_empty() {
            state.select(Some(run.selected.min(run.tasks.len() - 1)));
        }
        f.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title(" Tasks "))
                .highlight_style(
                    Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
                ),
            cols[0],
            &mut state,
        );

        f.render_widget(log_widget(run, cols[1]), cols[1]);
    }

    let help = if run.is_running() {
        "↑↓←→ select  v graph/list  Tab log  PgUp/PgDn scroll  q abort"
    } else {
        "↑↓←→ select  v graph/list  Tab log  PgUp/PgDn scroll  r re-run  q back"
    };
    f.render_widget(
        Paragraph::new(help)
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL).title(" Help ")),
        rows[2],
    );
}

/// Width of the task-name column in the combined log.
fn log_name_width(tasks: &[TaskRow]) -> usize {
    tasks
        .iter()
        .map(|t| text_width(&t.name))
        .max()
        .unwrap_or(0)
        .min(MAX_NAME_W)
}

/// `name` cut or padded with spaces to exactly `width` columns.
fn pad_name(name: &str, width: usize) -> String {
    let name = fit(name, width);
    let pad = width.saturating_sub(text_width(&name));
    format!("{}{}", name, " ".repeat(pad))
}

fn log_widget<'a>(run: &'a RunView, area: Rect) -> Paragraph<'a> {
    let height = area.height.saturating_sub(2) as usize;
    let lines = run.visible_log();
    let end = lines.len().saturating_sub(run.scroll.min(lines.len()));
    let start = end.saturating_sub(height);

    // Pad every name to the widest one so the `│` lines up.
    let name_w = log_name_width(&run.tasks);
    let text: Vec<Line> = lines[start..end]
        .iter()
        .map(|l| {
            let style = match l.stream {
                Stream::Stdout => Style::default(),
                Stream::Stderr => Style::default().fg(Color::Yellow),
            };
            if run.all_logs {
                Line::from(vec![
                    Span::styled(
                        format!("{} │ ", pad_name(&run.tasks[l.task].name, name_w)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(l.text.clone(), style),
                ])
            } else {
                Line::from(Span::styled(l.text.clone(), style))
            }
        })
        .collect();

    let title = if run.all_logs {
        " Log (all) ".to_string()
    } else {
        format!(
            " Log ({}) ",
            run.tasks.get(run.selected).map_or("", |t| t.name.as_str())
        )
    };
    Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section::ParamDef;
    use ratatui::backend::TestBackend;

    fn section(title: &str) -> Section {
        Section {
            title: title.into(),
            ..Default::default()
        }
    }

    fn app(sections: Vec<Section>) -> App {
        App::new(
            sections,
            PathBuf::from("README.md"),
            Config::default(),
            TuiOptions::default(),
        )
    }

    #[test]
    fn splits_args_with_quotes() {
        assert_eq!(split_args("a b  c"), vec!["a", "b", "c"]);
        assert_eq!(split_args(r#"a "b c" 'd e'"#), vec!["a", "b c", "d e"]);
        assert_eq!(split_args(r#"x="1 2""#), vec!["x=1 2"]);
        assert_eq!(split_args(r#""""#), vec![""]);
        assert!(split_args("   ").is_empty());
    }

    #[test]
    fn filters_and_navigates() {
        let mut a = app(vec![section("build"), section("test"), section("deploy")]);
        assert_eq!(a.visible.len(), 3);
        a.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        a.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
        a.handle_key(KeyCode::Char('s'), KeyModifiers::NONE);
        assert_eq!(a.visible, vec![1]);
        assert_eq!(a.selected_section().unwrap().title, "test");
        a.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(a.visible.len(), 3);
        assert!(matches!(a.mode, Mode::List));
    }

    #[test]
    fn required_params_prompt_for_args() {
        let mut s = section("deploy");
        s.params = vec![ParamDef {
            name: "env".into(),
            default: None,
            ..Default::default()
        }];
        let mut a = app(vec![s]);
        assert!(a.handle_key(KeyCode::Enter, KeyModifiers::NONE).is_none());
        assert!(matches!(a.mode, Mode::Args { .. }));
        for c in "prod".chars() {
            a.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        // `x` style terminal run returns the parsed args.
        a.mode = Mode::Args {
            input: "prod".into(),
            terminal: true,
        };
        match a.handle_key(KeyCode::Enter, KeyModifiers::NONE) {
            Some(Exit::Terminal { task, args }) => {
                assert_eq!(task, "deploy");
                assert_eq!(args, vec!["prod"]);
            }
            _ => panic!("expected terminal exit"),
        }
    }

    #[test]
    fn run_view_tracks_status_and_output() {
        let (tx, rx) = mpsc::channel();
        let mut view = RunView {
            task: "ci".into(),
            tasks: Vec::new(),
            log: Vec::new(),
            selected: 0,
            all_logs: true,
            graph: true,
            scroll: 0,
            result: None,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            handle: None,
        };
        tx.send(UiEvent::Plan(vec!["lint".into(), "ci".into()]))
            .unwrap();
        tx.send(UiEvent::Status {
            task: "lint".into(),
            status: TaskStatus::Started { dependency: true },
        })
        .unwrap();
        tx.send(UiEvent::Output {
            task: "lint".into(),
            stream: Stream::Stdout,
            line: "ok".into(),
        })
        .unwrap();
        tx.send(UiEvent::Status {
            task: "lint".into(),
            status: TaskStatus::Failed {
                error: "boom".into(),
            },
        })
        .unwrap();
        view.drain();
        assert_eq!(view.tasks.len(), 2);
        assert_eq!(view.tasks[0].state, TaskState::Failed("boom".into()));
        assert_eq!(view.tasks[1].state, TaskState::Pending);
        assert_eq!(view.log.len(), 2);
        assert!(view.is_running());
        tx.send(UiEvent::Finished(Err("boom".into()))).unwrap();
        view.drain();
        assert!(!view.is_running());
    }

    #[test]
    fn renders_list_and_run_views() {
        let mut a = app(vec![section("build"), section("test")]);
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|f| ui(f, &mut a)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(rendered.contains("build"));
        assert!(rendered.contains("Details"));
    }

    #[rstest::rstest]
    #[case("", &[])]
    #[case("   \t ", &[])]
    #[case("one", &["one"])]
    #[case("a\tb", &["a", "b"])]
    #[case("'single quoted'", &["single quoted"])]
    #[case(r#""it's""#, &["it's"])]
    #[case(r#"'say "hi"'"#, &[r#"say "hi""#])]
    #[case(r#"a"b c"d"#, &["ab cd"])]
    #[case("''", &[""])]
    #[case("a '' b", &["a", "", "b"])]
    #[case(r#""unterminated quote"#, &["unterminated quote"])]
    #[case("--flag=value", &["--flag=value"])]
    fn split_args_cases(#[case] input: &str, #[case] expected: &[&str]) {
        assert_eq!(split_args(input), expected);
    }

    proptest::proptest! {
        #[test]
        fn split_args_never_panics(input in ".{0,40}") {
            let _ = split_args(&input);
        }

        #[test]
        fn plain_words_round_trip(words in proptest::collection::vec("[a-zA-Z0-9_./=-]{1,8}", 0..6)) {
            let joined = words.join(" ");
            proptest::prop_assert_eq!(split_args(&joined), words);
        }

        #[test]
        fn quoting_protects_whitespace(words in proptest::collection::vec("[a-z]{1,4}( [a-z]{1,4}){0,2}", 1..4)) {
            let line = words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(" ");
            proptest::prop_assert_eq!(split_args(&line), words);
        }

        #[test]
        fn no_token_contains_unquoted_whitespace(input in "[a-z \t]{0,30}") {
            for token in split_args(&input) {
                proptest::prop_assert!(!token.contains(char::is_whitespace));
                proptest::prop_assert!(!token.is_empty());
            }
        }
    }

    fn run_view() -> (mpsc::Sender<UiEvent>, RunView) {
        let (tx, rx) = mpsc::channel();
        let view = RunView {
            task: "ci".into(),
            tasks: Vec::new(),
            log: Vec::new(),
            selected: 0,
            all_logs: false,
            graph: true,
            scroll: 0,
            result: None,
            rx,
            cancel: Arc::new(AtomicBool::new(false)),
            handle: None,
        };
        (tx, view)
    }

    #[rstest::rstest]
    #[case(TaskStatus::Started { dependency: false }, TaskState::Running)]
    #[case(
        TaskStatus::Retrying { attempt: 2, max: 4, delay: Duration::from_secs(1) },
        TaskState::Retrying(2, 4)
    )]
    #[case(
        TaskStatus::Succeeded { elapsed: Duration::from_secs(2) },
        TaskState::Succeeded(Duration::from_secs(2))
    )]
    #[case(TaskStatus::Skipped { reason: "up to date".into() }, TaskState::Skipped("up to date".into()))]
    #[case(TaskStatus::Failed { error: "x".into() }, TaskState::Failed("x".into()))]
    #[case(TaskStatus::Cancelled { reason: "aborted".into() }, TaskState::Cancelled("aborted".into()))]
    fn statuses_map_to_task_states(#[case] status: TaskStatus, #[case] expected: TaskState) {
        let (_tx, mut view) = run_view();
        view.apply(UiEvent::Status {
            task: "t".into(),
            status,
        });
        assert_eq!(view.tasks[0].state, expected);
    }

    fn graph_view(edges: &[(&str, &[&str])]) -> RunView {
        let (_tx, mut view) = run_view();
        let names: Vec<String> = edges.iter().map(|(n, _)| n.to_string()).collect();
        view.apply(UiEvent::Plan(names));
        view.apply(UiEvent::Graph(
            edges
                .iter()
                .map(|(n, d)| (n.to_string(), d.iter().map(|s| s.to_string()).collect()))
                .collect(),
        ));
        view
    }

    fn column_names(view: &RunView) -> Vec<Vec<&str>> {
        graph_layout(&view.tasks)
            .cols
            .iter()
            .map(|c| c.iter().map(|&i| view.tasks[i].name.as_str()).collect())
            .collect()
    }

    #[test]
    fn graph_columns_follow_the_longest_dependency_path() {
        let view = graph_view(&[
            ("lint", &[]),
            ("build", &[]),
            ("test", &["build"]),
            ("ci", &["lint", "test"]),
        ]);
        assert_eq!(
            column_names(&view),
            [vec!["lint", "build"], vec!["test"], vec!["ci"]]
        );
    }

    #[test]
    fn graph_columns_survive_a_cycle() {
        let view = graph_view(&[("a", &["b"]), ("b", &["a"]), ("c", &["c"])]);
        let layout = graph_layout(&view.tasks);
        assert_eq!(layout.pos.len(), 3);
    }

    #[test]
    fn graph_is_inactive_without_edges_or_when_toggled_off() {
        let mut view = graph_view(&[("a", &[]), ("b", &[])]);
        assert!(!view.graph_active());
        let mut view2 = graph_view(&[("a", &[]), ("b", &["a"])]);
        assert!(view2.graph_active());
        view2.graph = false;
        assert!(!view2.graph_active());
        view.graph = true;
        assert!(!view.graph_active());
    }

    #[test]
    fn graph_navigation_moves_within_and_between_columns() {
        let mut view = graph_view(&[
            ("lint", &[]),
            ("build", &[]),
            ("test", &["build"]),
            ("ci", &["lint", "test"]),
        ]);
        let selected = |v: &RunView| v.tasks[v.selected].name.clone();
        assert_eq!(selected(&view), "lint");
        view.move_selection(0, 1);
        assert_eq!(selected(&view), "build");
        view.move_selection(0, 1);
        assert_eq!(selected(&view), "lint", "wraps within the column");
        view.move_selection(1, 0);
        assert_eq!(selected(&view), "test");
        view.move_selection(1, 0);
        assert_eq!(selected(&view), "ci");
        view.move_selection(1, 0);
        assert_eq!(selected(&view), "ci", "stops at the last column");
        view.move_selection(-1, 0);
        assert_eq!(selected(&view), "test");
    }

    #[test]
    fn list_navigation_ignores_horizontal_moves() {
        let mut view = graph_view(&[("a", &[]), ("b", &[])]);
        view.move_selection(1, 0);
        assert_eq!(view.selected, 0);
        view.move_selection(0, -1);
        assert_eq!(view.selected, 1);
    }

    #[test]
    fn started_tasks_record_their_start_time() {
        let (_tx, mut view) = run_view();
        view.apply(UiEvent::Plan(vec!["a".into()]));
        assert!(view.tasks[0].started.is_none());
        view.apply(UiEvent::Status {
            task: "a".into(),
            status: TaskStatus::Started { dependency: false },
        });
        assert!(view.tasks[0].started.is_some());
    }

    #[rstest::rstest]
    #[case("build", 10, "build")]
    #[case("build", 5, "build")]
    #[case("build-all", 5, "buil…")]
    #[case("日本語テスト", 7, "日本語…")]
    fn fit_truncates_by_display_width(#[case] s: &str, #[case] max: usize, #[case] expected: &str) {
        assert_eq!(fit(s, max), expected);
    }

    #[rstest::rstest]
    #[case("a", 4, "a   ")]
    #[case("日本", 6, "日本  ")]
    #[case("build-all", 5, "buil…")]
    #[case("", 2, "  ")]
    fn pad_name_fills_to_exact_display_width(
        #[case] name: &str,
        #[case] width: usize,
        #[case] expected: &str,
    ) {
        let padded = pad_name(name, width);
        assert_eq!(padded, expected);
        assert_eq!(text_width(&padded), width);
    }

    #[test]
    fn renders_the_dependency_graph() {
        let mut view = graph_view(&[
            ("lint", &[]),
            ("build", &[]),
            ("test", &["build"]),
            ("ci", &["lint", "test"]),
        ]);
        view.apply(UiEvent::Status {
            task: "lint".into(),
            status: TaskStatus::Succeeded {
                elapsed: Duration::from_millis(1500),
            },
        });
        view.apply(UiEvent::Status {
            task: "build".into(),
            status: TaskStatus::Failed {
                error: "boom".into(),
            },
        });

        let layout = graph_layout(&view.tasks);
        let area = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(area);
        render_graph(&mut buf, area, &view, &layout, 0);
        let lines: Vec<String> = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        let text = lines.join("\n");
        assert!(text.contains("✔ lint"), "{text}");
        assert!(text.contains("1.5s"), "{text}");
        assert!(text.contains("✘ build"), "{text}");
        assert!(text.contains("fail"), "{text}");
        assert!(text.contains("○ ci"), "{text}");
        // `lint -> ci` spans two columns and runs below the `test` box.
        assert!(
            !lines[1].contains("test ─") && !lines[1].contains("test  ─"),
            "{text}"
        );
        assert!(lines[3].contains('└') && lines[3].contains('┘'), "{text}");
        // Both prerequisites of `ci` merge onto one trunk before it.
        assert!(lines[1].contains("┬─│ ○ ci"), "{text}");
    }

    #[test]
    fn a_small_pane_scrolls_instead_of_panicking() {
        let view = graph_view(&[("a", &[]), ("b", &["a"]), ("c", &["b"])]);
        let layout = graph_layout(&view.tasks);
        for (w, h) in [(0, 0), (1, 1), (10, 3), (30, 5)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            render_graph(&mut buf, area, &view, &layout, 3);
        }
    }

    #[test]
    fn run_screen_draws_the_graph_with_the_log() {
        let (tx, view) = run_view();
        let mut a = app(vec![section("ci")]);
        a.run = Some(view);
        a.mode = Mode::Run;
        tx.send(UiEvent::Plan(vec!["build".into(), "ci".into()]))
            .unwrap();
        tx.send(UiEvent::Graph(vec![
            ("build".into(), vec![]),
            ("ci".into(), vec!["build".into()]),
        ]))
        .unwrap();
        a.run.as_mut().unwrap().drain();
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|f| ui(f, &mut a)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(rendered.contains("Graph"));
        assert!(rendered.contains("build"));
        assert!(rendered.contains("Log"));
        assert!(!rendered.contains("Tasks"));
    }

    #[test]
    fn unknown_tasks_are_added_on_demand() {
        let (_tx, mut view) = run_view();
        view.apply(UiEvent::Output {
            task: "late".into(),
            stream: Stream::Stdout,
            line: "x".into(),
        });
        assert_eq!(view.tasks.len(), 1);
        assert_eq!(view.tasks[0].name, "late");
        assert_eq!(view.tasks[0].state, TaskState::Pending);
    }

    #[test]
    fn plan_events_do_not_duplicate_rows() {
        let (_tx, mut view) = run_view();
        view.apply(UiEvent::Plan(vec!["a".into(), "b".into()]));
        view.apply(UiEvent::Plan(vec!["b".into(), "a".into(), "c".into()]));
        let names: Vec<&str> = view.tasks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn failures_append_an_error_line_to_the_log() {
        let (_tx, mut view) = run_view();
        view.apply(UiEvent::Status {
            task: "t".into(),
            status: TaskStatus::Failed {
                error: "boom".into(),
            },
        });
        assert_eq!(view.log.len(), 1);
        assert_eq!(view.log[0].text, "error: boom");
        assert_eq!(view.log[0].stream, Stream::Stderr);
    }

    #[test]
    fn visible_log_follows_the_selection_unless_all_logs() {
        let (_tx, mut view) = run_view();
        view.apply(UiEvent::Plan(vec!["a".into(), "b".into()]));
        for (task, line) in [("a", "a1"), ("b", "b1"), ("a", "a2")] {
            view.apply(UiEvent::Output {
                task: task.into(),
                stream: Stream::Stdout,
                line: line.into(),
            });
        }
        let texts = |v: &RunView| -> Vec<String> {
            v.visible_log().iter().map(|l| l.text.clone()).collect()
        };
        view.selected = 0;
        assert_eq!(texts(&view), ["a1", "a2"]);
        view.selected = 1;
        assert_eq!(texts(&view), ["b1"]);
        view.all_logs = true;
        assert_eq!(texts(&view), ["a1", "b1", "a2"]);
    }

    #[test]
    fn the_log_is_capped() {
        let (_tx, mut view) = run_view();
        for i in 0..(MAX_LOG_LINES + 500) {
            view.apply(UiEvent::Output {
                task: "t".into(),
                stream: Stream::Stdout,
                line: i.to_string(),
            });
        }
        assert!(view.log.len() <= MAX_LOG_LINES);
        // The newest line is always kept.
        assert_eq!(
            view.log.last().unwrap().text,
            (MAX_LOG_LINES + 499).to_string()
        );
    }

    #[test]
    fn finishing_stops_the_run() {
        let (tx, mut view) = run_view();
        assert!(view.is_running());
        tx.send(UiEvent::Finished(Ok(()))).unwrap();
        view.drain();
        assert!(!view.is_running());
        assert_eq!(view.result, Some(Ok(())));
    }

    #[rstest::rstest]
    #[case(TaskState::Pending, "")]
    #[case(TaskState::Running, "")]
    #[case(TaskState::Failed("boom".into()), "")]
    #[case(TaskState::Succeeded(Duration::from_millis(1500)), " 1.5s")]
    #[case(TaskState::Succeeded(Duration::ZERO), " 0.0s")]
    #[case(TaskState::Retrying(2, 4), " retry 2/4")]
    #[case(TaskState::Skipped("up to date".into()), " up to date")]
    #[case(TaskState::Cancelled("aborted".into()), " aborted")]
    fn state_notes(#[case] state: TaskState, #[case] expected: &str) {
        assert_eq!(state_note(&state), expected);
    }

    #[test]
    fn the_running_spinner_cycles_through_its_frames() {
        let frames: Vec<String> = (0..SPINNER.len() * 2)
            .map(|t| state_span(&TaskState::Running, t * 2).content.to_string())
            .collect();
        assert_eq!(frames[..SPINNER.len()], frames[SPINNER.len()..]);
        let distinct: std::collections::HashSet<&String> = frames.iter().collect();
        assert_eq!(distinct.len(), SPINNER.len());
    }

    fn details_text(s: &Section) -> Vec<String> {
        section_details(s)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|sp| sp.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn details_show_every_option() {
        let mut s = section("deploy");
        s.description = Some("  Ships it.  ".into());
        s.depends = vec!["build".into(), "test".into()];
        s.params = vec![
            ParamDef {
                name: "env".into(),
                default: None,
                ..Default::default()
            },
            ParamDef {
                name: "tag".into(),
                default: Some("latest".into()),
                ..Default::default()
            },
        ];
        s.parallel = true;
        s.condition = Some("flag(\"CI\")".into());
        s.retries = 2;
        s.timeout = Some(Duration::from_millis(2500));
        s.capture = true;
        s.sources = vec!["src/**".into()];
        s.status = vec!["true".into()];
        s.for_each = Some(crate::section::LoopSpec::List(vec!["a".into(), "b".into()]));
        s.defer = vec!["cleanup".into()];
        s.once = true;
        s.requires = vec!["TOKEN".into()];
        s.prompt = Some("sure?".into());
        s.vars = vec![crate::vars::VarDef {
            name: "NAME".into(),
            value: crate::vars::VarValue::Literal("app".into()),
        }];
        s.ignore_error = true;
        s.dir = Some(PathBuf::from("svc"));
        s.env = vec![("A".into(), "1".into()), ("B".into(), "2".into())];
        s.codes = vec![crate::section::CodeBlock {
            lang: "bash".into(),
            code: "echo hi".into(),
        }];

        let text = details_text(&s).join("\n");
        for needle in [
            "deploy",
            "Ships it.",
            "build, test",
            "env (required), tag=latest",
            "parallel deps",
            "if flag(\"CI\")",
            "retries 2",
            "timeout 2.5s",
            "captures output",
            "cached on sources",
            "status checks",
            "for a, b",
            "defer cleanup",
            "once",
            "requires TOKEN",
            "asks to confirm",
            "vars NAME",
            "ignores errors",
            "svc",
            "A=1 B=2",
            "[bash]",
            "echo hi",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }

    #[test]
    fn details_of_a_bare_task_have_no_option_rows() {
        let text = details_text(&section("plain")).join("\n");
        for label in ["depends", "params", "options", "dir", "env"] {
            assert!(
                !text.contains(label),
                "unexpected {label:?} row in:\n{text}"
            );
        }
    }

    #[test]
    fn long_code_blocks_are_truncated() {
        let mut s = section("long");
        let code = (0..30)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        s.codes = vec![crate::section::CodeBlock {
            lang: "bash".into(),
            code,
        }];
        let lines = details_text(&s);
        assert!(lines.contains(&"line11".to_string()));
        assert!(!lines.contains(&"line12".to_string()));
        assert!(lines.contains(&"…".to_string()));
    }

    #[test]
    fn navigation_wraps_and_handles_empty_lists() {
        let mut a = app(vec![section("a"), section("b"), section("c")]);
        assert_eq!(a.selected_section().unwrap().title, "a");
        a.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(a.selected_section().unwrap().title, "b");
        a.handle_key(KeyCode::Up, KeyModifiers::NONE);
        a.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(a.selected_section().unwrap().title, "c");
        a.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(a.selected_section().unwrap().title, "a");

        let mut empty = app(vec![]);
        empty.handle_key(KeyCode::Down, KeyModifiers::NONE);
        empty.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert!(empty.selected_section().is_none());
    }

    #[test]
    fn filtering_matches_case_insensitively_and_can_be_empty() {
        let mut a = app(vec![section("Build"), section("test")]);
        a.handle_key(KeyCode::Char('/'), KeyModifiers::NONE);
        for c in "BUI".chars() {
            a.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(a.visible, vec![0]);
        for c in "zzz".chars() {
            a.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert!(a.visible.is_empty());
        assert!(a.selected_section().is_none());
        // Backspacing out of the filter restores matches.
        for _ in 0..3 {
            a.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        }
        assert_eq!(a.visible, vec![0]);
    }

    #[test]
    fn only_required_params_trigger_the_prompt() {
        let optional = {
            let mut s = section("opt");
            s.params = vec![ParamDef {
                name: "x".into(),
                default: Some("1".into()),
                ..Default::default()
            }];
            s
        };
        let required = {
            let mut s = section("req");
            s.params = vec![ParamDef {
                name: "x".into(),
                default: None,
                ..Default::default()
            }];
            s
        };
        assert!(!App::needs_args(&section("none")));
        assert!(!App::needs_args(&optional));
        assert!(App::needs_args(&required));
    }
}
