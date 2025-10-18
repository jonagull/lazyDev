use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io,
    path::PathBuf,
    process::{Child, Command},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Project {
    name: String,
    path: PathBuf,
    command: String,
    status: ProjectStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum ProjectStatus {
    Stopped,
    Running,
    Error(String),
}

#[derive(Debug)]
struct App {
    projects: Vec<Project>,
    selected_index: usize,
    should_quit: bool,
    running_processes: Vec<Option<Child>>,
}

impl App {
    fn new() -> Self {
        let projects = Self::load_projects().unwrap_or_else(|_| {
            // Create default config if none exists
            Self::create_default_config();
            Self::load_projects().unwrap_or_default()
        });
        
        let running_processes = (0..projects.len()).map(|_| None).collect();
        
        Self {
            projects,
            selected_index: 0,
            should_quit: false,
            running_processes,
        }
    }

    fn get_config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dev-dashboard")
            .join("projects.conf")
    }

    fn create_default_config() {
        let config_path = Self::get_config_path();
        if let Some(parent) = config_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        
        let default_config = r#"# Dev Dashboard Project Configuration
# Format: project_name='path/to/project'
# You can also specify commands: project_name='path/to/project|command'

# Example projects (uncomment and modify as needed):
# kumiko-web='/Users/jonathangulliksen/code/kumiko-web'
# backend-api='/Users/jonathangulliksen/code/backend|dotnet run'
# frontend-react='/Users/jonathangulliksen/code/frontend|npm start'
# database='/Users/jonathangulliksen/code/db|docker-compose up'

# Add your projects below:
"#;
        
        let _ = fs::write(&config_path, default_config);
    }

    fn load_projects() -> Result<Vec<Project>, Box<dyn std::error::Error>> {
        let config_path = Self::get_config_path();
        let content = fs::read_to_string(&config_path)?;
        
        let mut projects = Vec::new();
        
        for line in content.lines() {
            let line = line.trim();
            
            // Skip comments and empty lines
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            
            // Parse format: name='path' or name='path|command'
            if let Some(equals_pos) = line.find('=') {
                let name = line[..equals_pos].trim();
                let value = line[equals_pos + 1..].trim();
                
                // Remove quotes
                let value = value.trim_matches('\'').trim_matches('"');
                
                // Split path and command
                let (path_str, command) = if let Some(pipe_pos) = value.find('|') {
                    (value[..pipe_pos].trim(), value[pipe_pos + 1..].trim())
                } else {
                    (value, "ls") // Default command
                };
                
                projects.push(Project {
                    name: name.to_string(),
                    path: PathBuf::from(path_str),
                    command: command.to_string(),
                    status: ProjectStatus::Stopped,
                });
            }
        }
        
        Ok(projects)
    }

    fn edit_config(&self) -> Result<(), Box<dyn std::error::Error>> {
        let config_path = Self::get_config_path();
        
        // Open config file in vim
        let status = Command::new("vim")
            .arg(&config_path)
            .status()?;
            
        if !status.success() {
            eprintln!("Failed to edit config file");
        }
        
        Ok(())
    }

    fn start_project(&mut self, index: usize) {
        if index < self.projects.len() {
            let project = &self.projects[index];
            println!("Starting {} at {}", project.name, project.path.display());
            
            // In a real implementation, you'd spawn the actual process here
            // For now, we'll just simulate it
            self.projects[index].status = ProjectStatus::Running;
        }
    }

    fn stop_project(&mut self, index: usize) {
        if index < self.projects.len() {
            self.projects[index].status = ProjectStatus::Stopped;
            if let Some(mut process) = self.running_processes[index].take() {
                let _ = process.kill();
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and run
    let app = App::new();
    let res = run_app(&mut terminal, app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{err:?}");
    }

    Ok(())
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, mut app: App) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, &app))?;

        if crossterm::event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => {
                        app.should_quit = true;
                    }
                    KeyCode::Up => {
                        if app.selected_index > 0 {
                            app.selected_index -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if app.selected_index < app.projects.len() - 1 {
                            app.selected_index += 1;
                        }
                    }
                    KeyCode::Char(' ') => {
                        // Toggle project status
                        match app.projects[app.selected_index].status {
                            ProjectStatus::Running => {
                                app.stop_project(app.selected_index);
                            }
                            _ => {
                                app.start_project(app.selected_index);
                            }
                        }
                    }
                    KeyCode::Char('e') => {
                        // Edit config file
                        if let Err(e) = app.edit_config() {
                            eprintln!("Error editing config: {}", e);
                        } else {
                            // Reload projects after editing
                            if let Ok(new_projects) = App::load_projects() {
                                app.projects = new_projects;
                                app.running_processes = (0..app.projects.len()).map(|_| None).collect();
                                if app.selected_index >= app.projects.len() {
                                    app.selected_index = 0;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(
            [
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(3),
            ]
            .as_ref(),
        )
        .split(f.size());

    // Title
    let title = Paragraph::new("🚀 Dev Dashboard")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(ratatui::layout::Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    // Project list
    let items: Vec<ListItem> = app
        .projects
        .iter()
        .enumerate()
        .map(|(i, project)| {
            let status_icon = match project.status {
                ProjectStatus::Running => "🟢",
                ProjectStatus::Stopped => "🔴",
                ProjectStatus::Error(_) => "🟡",
            };

            let style = if i == app.selected_index {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", status_icon), Style::default()),
                Span::styled(&project.name, style),
                Span::styled(
                    format!(" - {}", project.command),
                    Style::default().fg(Color::Gray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Projects"))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    f.render_widget(list, chunks[1]);

    // Instructions
    let instructions = Paragraph::new("↑↓ Navigate | SPACE Toggle | E Edit Config | Q Quit")
        .style(Style::default().fg(Color::Gray))
        .alignment(ratatui::layout::Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(instructions, chunks[2]);
}