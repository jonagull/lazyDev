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
    io::{self, BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use signal_hook::{consts::SIGINT, flag};
use sysinfo::{System, Pid};
use chrono::{DateTime, Local};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectConfig {
    name: String,
    path: PathBuf,
    command: String,
    daemon_mode: bool,
}

#[derive(Debug, Clone)]
struct Project {
    config: ProjectConfig,
    status: ProjectStatus,
}

#[derive(Debug, Clone)]
enum ProjectStatus {
    Stopped,
    Running {
        pid: Pid,
        start_time: DateTime<Local>,
        cpu_usage: f32,
        memory_usage: u64,
    },
    Daemon {
        pid: Pid,
        start_time: DateTime<Local>,
        cpu_usage: f32,
        memory_usage: u64,
    },
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonInfo {
    project_name: String,
    pid: u32,
    start_time: DateTime<Local>,
    command: String,
    working_dir: String,
}

#[derive(Debug, Clone)]
struct ProcessOutput {
    stdout: Vec<String>,
    stderr: Vec<String>,
    max_lines: usize,
}

impl ProcessOutput {
    fn new(max_lines: usize) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            max_lines,
        }
    }

    fn add_stdout(&mut self, line: String) {
        self.stdout.push(line);
        if self.stdout.len() > self.max_lines {
            self.stdout.remove(0);
        }
    }

    fn add_stderr(&mut self, line: String) {
        self.stderr.push(line);
        if self.stderr.len() > self.max_lines {
            self.stderr.remove(0);
        }
    }

    fn get_all_output(&self) -> String {
        let mut output = String::new();
        
        if !self.stderr.is_empty() {
            output.push_str("=== STDERR ===\n");
            for line in &self.stderr {
                output.push_str(&format!("{}\n", line));
            }
            output.push_str("\n");
        }
        
        if !self.stdout.is_empty() {
            output.push_str("=== STDOUT ===\n");
            for line in &self.stdout {
                output.push_str(&format!("{}\n", line));
            }
        }
        
        if output.is_empty() {
            output.push_str("No output yet...");
        }
        
        output
    }
}

#[derive(Debug)]
struct App {
    projects: Vec<Project>,
    selected_index: usize,
    should_quit: bool,
    running_processes: Vec<Option<Child>>,
    process_outputs: Vec<Arc<Mutex<ProcessOutput>>>,
    system: System,
    last_update: Instant,
    output_scroll: usize,
    auto_scroll: bool,
    show_system_processes: bool,
}

impl App {
    fn new() -> Self {
        let projects = Self::load_projects().unwrap_or_else(|_| {
            // Create default config if none exists
            Self::create_default_config();
            Self::load_projects().unwrap_or_default()
        });
        
        let running_processes = (0..projects.len()).map(|_| None).collect();
        let process_outputs = (0..projects.len()).map(|_| Arc::new(Mutex::new(ProcessOutput::new(100)))).collect();
        
        let mut app = Self {
            projects,
            selected_index: 0,
            should_quit: false,
            running_processes,
            process_outputs,
            system: System::new_all(),
            last_update: Instant::now(),
            output_scroll: 0,
            auto_scroll: true,
            show_system_processes: true,
        };
        
        // Discover existing daemons on startup
        app.discover_daemons();
        
        app
    }

    fn get_config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dev-dashboard")
            .join("projects.conf")
    }

    fn get_daemon_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dev-dashboard")
            .join("daemons")
    }

    fn get_daemon_file_path(project_name: &str) -> PathBuf {
        Self::get_daemon_dir().join(format!("{}.json", project_name))
    }

    fn create_default_config() {
        let config_path = Self::get_config_path();
        if let Some(parent) = config_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        
        let default_config = r#"# Dev Dashboard Project Configuration
# Format: project_name='path/to/project,command,daemon_mode'
# If no command is specified, it defaults to 'ls' (just shows directory contents)
# If no daemon_mode is specified, it defaults to 'false'

# Common development commands:
# dotnet run          - Run .NET application
# npm start           - Start Node.js development server
# npm run dev         - Start development server
# yarn dev            - Start development server with Yarn
# python -m uvicorn   - Start Python FastAPI server
# cargo run           - Run Rust application
# docker-compose up   - Start Docker services
# go run main.go      - Run Go application

# Example projects (uncomment and modify as needed):
# kumiko-web='/Users/jonathangulliksen/code/kumiko-web,npm start,false'
# backend-api='/Users/jonathangulliksen/code/backend,dotnet run,true'
# frontend-react='/Users/jonathangulliksen/code/frontend,npm run dev,false'
# database='/Users/jonathangulliksen/code/db,docker-compose up,true'
# rust-service='/Users/jonathangulliksen/code/rust-service,cargo run,false'
# python-api='/Users/jonathangulliksen/code/python-api,python -m uvicorn main:app --reload,true'

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
            
            // Parse format: name='path' or name='path,command'
            if let Some(equals_pos) = line.find('=') {
                let name = line[..equals_pos].trim();
                let value = line[equals_pos + 1..].trim();
                
                // Remove quotes
                let value = value.trim_matches('\'').trim_matches('"');
                
                // Split path, command, and daemon_mode
                let parts: Vec<&str> = value.split(',').map(|s| s.trim()).collect();
                let path_str = parts.get(0).unwrap_or(&value);
                let command = parts.get(1).unwrap_or(&"ls");
                let daemon_mode = parts.get(2).unwrap_or(&"false").parse::<bool>().unwrap_or(false);
                
                projects.push(Project {
                    config: ProjectConfig {
                        name: name.to_string(),
                        path: PathBuf::from(path_str),
                        command: command.to_string(),
                        daemon_mode,
                    },
                    status: ProjectStatus::Stopped,
                });
            }
        }
        
        Ok(projects)
    }

    fn save_daemon_info(&self, project_name: &str, pid: u32, command: &str, working_dir: &str) -> Result<(), Box<dyn std::error::Error>> {
        let daemon_dir = Self::get_daemon_dir();
        fs::create_dir_all(&daemon_dir)?;
        
        let daemon_info = DaemonInfo {
            project_name: project_name.to_string(),
            pid,
            start_time: Local::now(),
            command: command.to_string(),
            working_dir: working_dir.to_string(),
        };
        
        let daemon_file = Self::get_daemon_file_path(project_name);
        let json = serde_json::to_string_pretty(&daemon_info)?;
        fs::write(daemon_file, json)?;
        
        Ok(())
    }

    fn save_projects_config(&self) -> Result<(), Box<dyn std::error::Error>> {
        let config_path = Self::get_config_path();
        let mut content = String::new();
        
        content.push_str("# Dev Dashboard Project Configuration\n");
        content.push_str("# Format: project_name='path/to/project,command,daemon_mode'\n");
        content.push_str("# If no command is specified, it defaults to 'ls' (just shows directory contents)\n");
        content.push_str("# If no daemon_mode is specified, it defaults to 'false'\n\n");
        
        for project in &self.projects {
            let daemon_str = if project.config.daemon_mode { "true" } else { "false" };
            content.push_str(&format!(
                "{}='{},{},{}'\n",
                project.config.name,
                project.config.path.to_string_lossy(),
                project.config.command,
                daemon_str
            ));
        }
        
        fs::write(config_path, content)?;
        Ok(())
    }

    fn load_daemon_info(&self, project_name: &str) -> Result<Option<DaemonInfo>, Box<dyn std::error::Error>> {
        let daemon_file = Self::get_daemon_file_path(project_name);
        if daemon_file.exists() {
            let content = fs::read_to_string(daemon_file)?;
            let daemon_info: DaemonInfo = serde_json::from_str(&content)?;
            Ok(Some(daemon_info))
        } else {
            Ok(None)
        }
    }

    fn remove_daemon_info(&self, project_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let daemon_file = Self::get_daemon_file_path(project_name);
        if daemon_file.exists() {
            fs::remove_file(daemon_file)?;
        }
        Ok(())
    }

    fn discover_daemons(&mut self) {
        let project_names: Vec<String> = self.projects.iter().map(|p| p.config.name.clone()).collect();
        
        for (index, project_name) in project_names.iter().enumerate() {
            if let Ok(Some(daemon_info)) = self.load_daemon_info(project_name) {
                // Check if the process is still running
                self.system.refresh_processes();
                if let Some(process) = self.system.process(Pid::from_u32(daemon_info.pid)) {
                    // Process is still running, update status
                    self.projects[index].status = ProjectStatus::Daemon {
                        pid: Pid::from_u32(daemon_info.pid),
                        start_time: daemon_info.start_time,
                        cpu_usage: process.cpu_usage(),
                        memory_usage: process.memory(),
                    };
                } else {
                    // Process is dead, clean up
                    let _ = self.remove_daemon_info(project_name);
                }
            }
        }
    }

    fn edit_config(&self) -> Result<(), Box<dyn std::error::Error>> {
        let config_path = Self::get_config_path();
        
        // Open config file in nvim
        let status = Command::new("nvim")
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
            
            // Split command into parts
            let mut cmd_parts = project.config.command.split_whitespace();
            let program = cmd_parts.next().unwrap_or("ls");
            let args: Vec<&str> = cmd_parts.collect();
            
            let mut command = Command::new(program);
            command.args(&args).current_dir(&project.config.path);
            
            if project.config.daemon_mode {
                // Daemon mode: detach from parent, no output capture
                command.stdout(Stdio::null()).stderr(Stdio::null());
                
                match command.spawn() {
                    Ok(child) => {
                        let pid = Pid::from_u32(child.id());
                        
                        // Save daemon info to file
                        if let Err(e) = self.save_daemon_info(
                            &project.config.name,
                            child.id(),
                            &project.config.command,
                            &project.config.path.to_string_lossy()
                        ) {
                            self.projects[index].status = ProjectStatus::Error(format!("Failed to save daemon info: {}", e));
                            return;
                        }
                        
                        self.projects[index].status = ProjectStatus::Daemon {
                            pid,
                            start_time: Local::now(),
                            cpu_usage: 0.0,
                            memory_usage: 0,
                        };
                    }
                    Err(e) => {
                        self.projects[index].status = ProjectStatus::Error(format!("Failed to start daemon: {}", e));
                    }
                }
            } else {
                // Regular mode: capture output
                command.stdout(Stdio::piped()).stderr(Stdio::piped());
                
                match command.spawn() {
                    Ok(mut child) => {
                        let pid = Pid::from_u32(child.id());
                        
                        // Get handles to stdout and stderr
                        let stdout = child.stdout.take().unwrap();
                        let stderr = child.stderr.take().unwrap();
                        
                        // Clone the output buffer for this process
                        let output_buffer = Arc::clone(&self.process_outputs[index]);
                        
                        // Spawn thread to read stdout
                        let output_buffer_stdout = Arc::clone(&output_buffer);
                        thread::spawn(move || {
                            let reader = BufReader::new(stdout);
                            for line in reader.lines() {
                                if let Ok(line) = line {
                                    if let Ok(mut output) = output_buffer_stdout.lock() {
                                        output.add_stdout(line);
                                    }
                                }
                            }
                        });
                        
                        // Spawn thread to read stderr
                        let output_buffer_stderr = Arc::clone(&output_buffer);
                        thread::spawn(move || {
                            let reader = BufReader::new(stderr);
                            for line in reader.lines() {
                                if let Ok(line) = line {
                                    if let Ok(mut output) = output_buffer_stderr.lock() {
                                        output.add_stderr(line);
                                    }
                                }
                            }
                        });
                        
                        self.running_processes[index] = Some(child);
                        self.projects[index].status = ProjectStatus::Running {
                            pid,
                            start_time: Local::now(),
                            cpu_usage: 0.0,
                            memory_usage: 0,
                        };
                    }
                    Err(e) => {
                        self.projects[index].status = ProjectStatus::Error(format!("Failed to start: {}", e));
                    }
                }
            }
        }
    }

    fn stop_project(&mut self, index: usize) {
        if index < self.projects.len() {
            let project_name = &self.projects[index].config.name;
            
            match &self.projects[index].status {
                ProjectStatus::Daemon { pid, .. } => {
                    // Stop daemon process by PID
                    if let Err(e) = std::process::Command::new("kill")
                        .arg("-TERM")
                        .arg(pid.as_u32().to_string())
                        .output()
                    {
                        self.projects[index].status = ProjectStatus::Error(format!("Failed to stop daemon: {}", e));
                        return;
                    }
                    
                    // Remove daemon info file
                    let _ = self.remove_daemon_info(project_name);
                }
                ProjectStatus::Running { .. } => {
                    // Stop regular process
                    if let Some(mut process) = self.running_processes[index].take() {
                        // Try to terminate gracefully first
                        if let Err(_) = process.kill() {
                            // If graceful kill fails, try to force kill
                            let _ = process.kill();
                        }
                        
                        // Wait for the process to actually terminate with a timeout
                        let start = Instant::now();
                        while start.elapsed() < Duration::from_secs(5) {
                            match process.try_wait() {
                                Ok(Some(_)) => break, // Process terminated
                                Ok(None) => {
                                    // Process still running, wait a bit
                                    thread::sleep(Duration::from_millis(100));
                                }
                                Err(_) => break, // Error occurred
                            }
                        }
                        
                        // If process is still running, force kill it
                        let _ = process.kill();
                        let _ = process.wait();
                    }
                }
                _ => {}
            }
            
            self.projects[index].status = ProjectStatus::Stopped;
            
            // Clear the output buffer when stopping
            if let Ok(mut output) = self.process_outputs[index].lock() {
                *output = ProcessOutput::new(100);
            }
        }
    }

    fn stop_all_projects(&mut self) {
        for index in 0..self.projects.len() {
            if matches!(self.projects[index].status, ProjectStatus::Running { .. } | ProjectStatus::Daemon { .. }) {
                self.stop_project(index);
            }
        }
    }

    fn scroll_output_up(&mut self) {
        if self.output_scroll > 0 {
            self.output_scroll -= 1;
            self.auto_scroll = false;
        }
    }

    fn scroll_output_down(&mut self) {
        self.output_scroll += 1;
        self.auto_scroll = false;
    }

    fn scroll_output_to_bottom(&mut self) {
        self.auto_scroll = true;
        self.output_scroll = 0;
    }

    fn toggle_auto_scroll(&mut self) {
        self.auto_scroll = !self.auto_scroll;
        if self.auto_scroll {
            self.output_scroll = 0;
        }
    }

    fn toggle_system_processes(&mut self) {
        self.show_system_processes = !self.show_system_processes;
    }

    fn update_process_info(&mut self) {
        if self.last_update.elapsed() < Duration::from_millis(500) {
            return; // Don't update too frequently
        }
        
        self.system.refresh_processes();
        self.last_update = Instant::now();
        
        for (index, project) in self.projects.iter_mut().enumerate() {
            match &mut project.status {
                ProjectStatus::Running { pid, start_time: _, cpu_usage, memory_usage } => {
                    if let Some(process) = self.system.process(*pid) {
                        *cpu_usage = process.cpu_usage();
                        *memory_usage = process.memory();
                    } else {
                        // Process died
                        project.status = ProjectStatus::Stopped;
                        self.running_processes[index] = None;
                    }
                }
                ProjectStatus::Daemon { pid, start_time: _, cpu_usage, memory_usage } => {
                    if let Some(process) = self.system.process(*pid) {
                        *cpu_usage = process.cpu_usage();
                        *memory_usage = process.memory();
                    } else {
                        // Daemon process died, clean up
                        project.status = ProjectStatus::Stopped;
                        // Note: We can't call remove_daemon_info here due to borrowing rules
                        // The cleanup will happen on next startup
                    }
                }
                _ => {}
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup signal handling for graceful shutdown
    let shutdown_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    flag::register(SIGINT, Arc::clone(&shutdown_flag))?;

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and run
    let mut app = App::new();
    let res = run_app(&mut terminal, &mut app, Arc::clone(&shutdown_flag));

    // Stop all running processes before exiting
    app.stop_all_projects();

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

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App, shutdown_flag: Arc<std::sync::atomic::AtomicBool>) -> io::Result<()> {
    loop {
        // Check for shutdown signal
        if shutdown_flag.load(std::sync::atomic::Ordering::Relaxed) {
            app.should_quit = true;
        }

        app.update_process_info();
        terminal.draw(|f| ui(f, app))?;

        if crossterm::event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => {
                        app.should_quit = true;
                    }
                    KeyCode::Up => {
                        // Check if we have modifier keys for output scrolling
                        if key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) {
                            app.scroll_output_up();
                        } else {
                            if app.selected_index > 0 {
                                app.selected_index -= 1;
                            }
                        }
                    }
                    KeyCode::Down => {
                        // Check if we have modifier keys for output scrolling
                        if key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) {
                            app.scroll_output_down();
                        } else {
                            if app.selected_index < app.projects.len() - 1 {
                                app.selected_index += 1;
                            }
                        }
                    }
                    KeyCode::PageUp => {
                        app.scroll_output_up();
                    }
                    KeyCode::PageDown => {
                        app.scroll_output_down();
                    }
                    KeyCode::End => {
                        app.scroll_output_to_bottom();
                    }
                    KeyCode::Char('a') => {
                        app.toggle_auto_scroll();
                    }
                    KeyCode::Char('p') => {
                        app.toggle_system_processes();
                    }
                    KeyCode::Char('d') => {
                        // Toggle daemon mode for selected project
                        if app.selected_index < app.projects.len() {
                            app.projects[app.selected_index].config.daemon_mode = !app.projects[app.selected_index].config.daemon_mode;
                            // Save the updated configuration
                            if let Err(e) = app.save_projects_config() {
                                eprintln!("Failed to save config: {}", e);
                            }
                        }
                    }
                    KeyCode::Char(' ') => {
                        // Toggle project status
                        match &app.projects[app.selected_index].status {
                            ProjectStatus::Running { .. } | ProjectStatus::Daemon { .. } => {
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
                                app.process_outputs = (0..app.projects.len()).map(|_| Arc::new(Mutex::new(ProcessOutput::new(100)))).collect();
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

fn get_system_processes(system: &System) -> Vec<(String, u32, f32, u64)> {
    let mut processes = Vec::new();
    
    // Filter for common development processes
    let target_processes = ["node", "npm", "yarn", "pnpm", "python", "python3", "cargo", "go", "dotnet", "java", "docker"];
    
    for (_, process) in system.processes() {
        let name = process.name();
        let pid = process.pid().as_u32();
        let cpu = process.cpu_usage();
        let memory = process.memory();
        
        // Only include processes that match our target list
        if target_processes.iter().any(|&target| name.to_lowercase().contains(target)) {
            processes.push((name.to_string(), pid, cpu, memory));
        }
    }
    
    // Sort by CPU usage (highest first)
    processes.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    
    // Limit to top 10 processes to avoid cluttering the UI
    processes.truncate(10);
    
    processes
}

fn ui(f: &mut Frame, app: &mut App) {
    // Main horizontal split: left panel (projects) and right panel (output)
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(f.size());

    // Left panel: Projects list and System Processes
    let left_chunks = if app.show_system_processes {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // Title
                Constraint::Percentage(50),  // Projects
                Constraint::Length(3),  // System Processes title
                Constraint::Percentage(40),  // System Processes
                Constraint::Length(3)   // Instructions
            ])
            .split(main_chunks[0])
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // Title
                Constraint::Min(0),     // Projects (full height)
                Constraint::Length(3)   // Instructions
            ])
            .split(main_chunks[0])
    };

    // Title for left panel
    let title = Paragraph::new("🚀 Projects")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(ratatui::layout::Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, left_chunks[0]);

    // Project list - more compact for left panel
    let items: Vec<ListItem> = app
        .projects
        .iter()
        .enumerate()
        .map(|(i, project)| {
            let (status_icon, status_text) = match &project.status {
                ProjectStatus::Running { pid, start_time, cpu_usage, memory_usage } => {
                    let uptime = Local::now() - *start_time;
                    let uptime_str = if uptime.num_minutes() > 0 {
                        format!("{}m", uptime.num_minutes())
                    } else {
                        format!("{}s", uptime.num_seconds())
                    };
                    
                    let memory_mb = *memory_usage / 1024 / 1024;
                    let info = format!("PID:{} {:.1}% {}MB {}", pid, cpu_usage, memory_mb, uptime_str);
                    ("🟢", info)
                }
                ProjectStatus::Daemon { pid, start_time, cpu_usage, memory_usage } => {
                    let uptime = Local::now() - *start_time;
                    let uptime_str = if uptime.num_minutes() > 0 {
                        format!("{}m", uptime.num_minutes())
                    } else {
                        format!("{}s", uptime.num_seconds())
                    };
                    
                    let memory_mb = *memory_usage / 1024 / 1024;
                    let info = format!("DAEMON PID:{} {:.1}% {}MB {}", pid, cpu_usage, memory_mb, uptime_str);
                    ("🔵", info)
                }
                ProjectStatus::Stopped => {
                    let mode_indicator = if project.config.daemon_mode { " (daemon)" } else { "" };
                    ("🔴", format!("Stopped{}", mode_indicator))
                }
                ProjectStatus::Error(msg) => {
                    ("🟡", format!("Error: {}", if msg.len() > 20 { &msg[..20] } else { msg }))
                }
            };

            let style = if i == app.selected_index {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            // Compact format for left panel
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", status_icon), Style::default()),
                Span::styled(&project.config.name, style),
                Span::styled(
                    format!("\n  {}", status_text),
                    Style::default().fg(Color::Gray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Projects"))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    f.render_widget(list, left_chunks[1]);

    if app.show_system_processes {
        // System Processes title
        let system_title = Paragraph::new("⚙️ System Processes")
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .alignment(ratatui::layout::Alignment::Center)
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(system_title, left_chunks[2]);

        // System Processes list
        let system_processes = get_system_processes(&app.system);
        let system_items: Vec<ListItem> = system_processes
            .iter()
            .map(|(name, pid, cpu, memory)| {
                let memory_mb = memory / 1024 / 1024;
                let info = format!("PID:{} {:.1}% {}MB", pid, cpu, memory_mb);
                ListItem::new(Line::from(vec![
                    Span::styled(format!("🔹 "), Style::default().fg(Color::Blue)),
                    Span::styled(name.clone(), Style::default().fg(Color::White)),
                    Span::styled(
                        format!("\n  {}", info),
                        Style::default().fg(Color::Gray),
                    ),
                ]))
            })
            .collect();

        let system_list = List::new(system_items)
            .block(Block::default().borders(Borders::ALL).title("Running Processes"))
            .style(Style::default().fg(Color::White));
        f.render_widget(system_list, left_chunks[3]);
    }

    // Instructions for left panel
    let instructions_text = if app.show_system_processes {
        "↑↓ Navigate | SPACE Toggle | D Daemon | P Processes | E Config"
    } else {
        "↑↓ Navigate | SPACE Toggle | D Daemon | P Processes | E Config"
    };
    let instructions = Paragraph::new(instructions_text)
        .style(Style::default().fg(Color::Gray))
        .alignment(ratatui::layout::Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    let instructions_index = if app.show_system_processes { 4 } else { 2 };
    f.render_widget(instructions, left_chunks[instructions_index]);

    // Right panel: Output/Logs
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(3)])
        .split(main_chunks[1]);

    // Title for right panel
    let output_title = if app.selected_index < app.projects.len() {
        let project = &app.projects[app.selected_index];
        format!("📋 {} - {}", project.config.name, project.config.command)
    } else {
        "📋 Output".to_string()
    };

    let output_header = Paragraph::new(output_title)
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(ratatui::layout::Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(output_header, right_chunks[0]);

    // Output content - now scrollable
    let (output_lines, output_height) = if app.selected_index < app.projects.len() {
        let project = &app.projects[app.selected_index];
        let (header_info, process_output) = match &project.status {
            ProjectStatus::Running { pid, start_time, cpu_usage, memory_usage } => {
                let uptime = Local::now() - *start_time;
                let uptime_str = if uptime.num_minutes() > 0 {
                    format!("{} minutes", uptime.num_minutes())
                } else {
                    format!("{} seconds", uptime.num_seconds())
                };
                
                let memory_mb = *memory_usage / 1024 / 1024;
                
                let header = format!(
                    "🟢 Process is running\n\n\
                    PID: {} | CPU: {:.1}% | Memory: {} MB | Uptime: {}\n\
                    Path: {}\n\
                    Command: {}\n\n\
                    {}",
                    pid, cpu_usage, memory_mb, uptime_str,
                    project.config.path.display(),
                    project.config.command,
                    "─".repeat(50)
                );
                
                let process_output = if let Ok(output) = app.process_outputs[app.selected_index].lock() {
                    output.get_all_output()
                } else {
                    "Error reading output".to_string()
                };
                
                (header, process_output)
            }
            ProjectStatus::Daemon { pid, start_time, cpu_usage, memory_usage } => {
                let uptime = Local::now() - *start_time;
                let uptime_str = if uptime.num_minutes() > 0 {
                    format!("{} minutes", uptime.num_minutes())
                } else {
                    format!("{} seconds", uptime.num_seconds())
                };
                
                let memory_mb = *memory_usage / 1024 / 1024;
                
                let header = format!(
                    "🔵 Process is running as DAEMON\n\n\
                    PID: {} | CPU: {:.1}% | Memory: {} MB | Uptime: {}\n\
                    Path: {}\n\
                    Command: {}\n\n\
                    {}",
                    pid, cpu_usage, memory_mb, uptime_str,
                    project.config.path.display(),
                    project.config.command,
                    "─".repeat(50)
                );
                
                let process_output = "Daemon processes run in the background without output capture.\n\
                    They will continue running even if you close the dashboard.\n\
                    Use 'ps aux | grep <command>' to see their output in the terminal.";
                
                (header, process_output.to_string())
            }
            ProjectStatus::Stopped => {
                let header = format!(
                    "🔴 Process is stopped\n\n\
                    Path: {}\n\
                    Command: {}\n\n\
                    Press SPACE to start this project",
                    project.config.path.display(),
                    project.config.command
                );
                (header, String::new())
            }
            ProjectStatus::Error(msg) => {
                let header = format!(
                    "🟡 Error occurred\n\n\
                    Error: {}\n\
                    Path: {}\n\
                    Command: {}\n\n\
                    Check your configuration and try again",
                    msg,
                    project.config.path.display(),
                    project.config.command
                );
                (header, String::new())
            }
        };
        
        let mut all_lines = Vec::new();
        all_lines.extend(header_info.lines().map(|s| s.to_string()));
        if !process_output.is_empty() {
            all_lines.push(String::new());
            all_lines.extend(process_output.lines().map(|s| s.to_string()));
        }
        
        (all_lines, right_chunks[1].height as usize)
    } else {
        (vec!["No project selected".to_string()], right_chunks[1].height as usize)
    };

    // Calculate scroll position
    let max_scroll = if output_lines.len() > output_height.saturating_sub(2) {
        output_lines.len().saturating_sub(output_height.saturating_sub(2))
    } else {
        0
    };

    // Auto-scroll to bottom if enabled
    if app.auto_scroll {
        app.output_scroll = max_scroll;
    } else {
        // Clamp scroll position
        app.output_scroll = app.output_scroll.min(max_scroll);
    }

    // Create scrollable text
    let visible_lines = if output_lines.len() <= output_height.saturating_sub(2) {
        output_lines
    } else {
        let start = app.output_scroll;
        let end = (start + output_height.saturating_sub(2)).min(output_lines.len());
        output_lines[start..end].to_vec()
    };

    let output_text = visible_lines.join("\n");
    let scroll_info = if max_scroll > 0 {
        format!(" ({}%)", (app.output_scroll * 100 / max_scroll.max(1)))
    } else {
        String::new()
    };

    let output_paragraph = Paragraph::new(output_text)
        .style(Style::default().fg(Color::White))
        .block(Block::default()
            .borders(Borders::ALL)
            .title(format!("Output{}", scroll_info))
        )
        .wrap(ratatui::widgets::Wrap { trim: true });
    f.render_widget(output_paragraph, right_chunks[1]);

    // Instructions for right panel
    let auto_scroll_indicator = if app.auto_scroll { "AUTO" } else { "MANUAL" };
    let right_instructions = Paragraph::new(format!(
        "Q Quit | Shift+↑↓ Scroll | PgUp/PgDn | End Bottom | A Toggle Auto ({})",
        auto_scroll_indicator
    ))
        .style(Style::default().fg(Color::Gray))
        .alignment(ratatui::layout::Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(right_instructions, right_chunks[2]);
}