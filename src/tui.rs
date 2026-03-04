//! Interactive TUI for task selection and execution

use std::io;
use std::path::PathBuf;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::{Config, Runner};
use crate::runner::Section;

struct App {
    sections: Vec<Section>,
    list_state: ListState,
}

impl App {
    fn new(sections: Vec<Section>) -> Self {
        let mut list_state = ListState::default();
        if !sections.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            sections,
            list_state,
        }
    }

    fn next(&mut self) {
        if self.sections.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => (i + 1) % self.sections.len(),
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn previous(&mut self) {
        if self.sections.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.sections.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn selected_section(&self) -> Option<&Section> {
        self.list_state
            .selected()
            .and_then(|i| self.sections.get(i))
    }
}

/// Run the interactive TUI for task selection
pub fn run_tui(
    markdown_path: PathBuf,
    config: Config,
    lang_filter: Option<String>,
    dry_run: bool,
) -> crate::error::Result<()> {
    let mut runner = Runner::new(config.clone());
    runner.set_dry_run(dry_run);

    let sections = {
        let mut r = Runner::new(config);
        let raw_sections = r.list_task_sections(&markdown_path)?;

        // Apply language filter if specified
        if let Some(ref lang) = lang_filter {
            raw_sections
                .into_iter()
                .filter(|s| s.codes.iter().any(|c| c.lang == *lang))
                .collect()
        } else {
            raw_sections
        }
    };

    // Setup terminal
    enable_raw_mode().map_err(|e| crate::error::Error::Io(e))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .map_err(|e| crate::error::Error::Io(e))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .map_err(|e| crate::error::Error::Io(e))?;

    let mut app = App::new(sections);
    let result = run_app(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode().map_err(|e| crate::error::Error::Io(e))?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .map_err(|e| crate::error::Error::Io(e))?;
    terminal.show_cursor().map_err(|e| crate::error::Error::Io(e))?;

    // Execute selected task if any
    if let Some(task_name) = result? {
        println!("Running task: {}\n", task_name);
        runner.run_task_with_lang_filter(
            &markdown_path,
            &task_name,
            &[],
            lang_filter.as_deref(),
        )?;
    }

    Ok(())
}

fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> crate::error::Result<Option<String>> {
    loop {
        terminal
            .draw(|f| ui(f, app))
            .map_err(|e| crate::error::Error::Io(e))?;

        if let Event::Key(key) = event::read().map_err(|e| crate::error::Error::Io(e))? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
                KeyCode::Down | KeyCode::Char('j') => app.next(),
                KeyCode::Up | KeyCode::Char('k') => app.previous(),
                KeyCode::Enter => {
                    if let Some(section) = app.selected_section() {
                        return Ok(Some(section.title.clone()));
                    }
                }
                _ => {}
            }
        }
    }
}

fn ui(f: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(f.area());

    let items: Vec<ListItem> = app
        .sections
        .iter()
        .map(|s| {
            let content = if let Some(ref desc) = s.description {
                let trimmed = desc.trim();
                if !trimmed.is_empty() {
                    Line::from(vec![
                        Span::styled(
                            s.title.clone(),
                            Style::default()
                                .fg(Color::Green)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" - "),
                        Span::styled(trimmed.to_string(), Style::default().fg(Color::DarkGray)),
                    ])
                } else {
                    Line::from(Span::styled(
                        s.title.clone(),
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ))
                }
            } else {
                Line::from(Span::styled(
                    s.title.clone(),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ))
            };
            ListItem::new(content)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Tasks "))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    f.render_stateful_widget(list, chunks[0], &mut app.list_state);

    let help = Paragraph::new("↑/↓ or j/k: navigate   Enter: run   q/Esc: quit")
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL).title(" Help "));

    f.render_widget(help, chunks[1]);
}
