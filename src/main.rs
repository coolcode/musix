use std::{
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph},
};
use rodio::{Decoder, OutputStreamBuilder, Sink, Source};

#[derive(Clone)]
struct Song {
    name: String,
    path: PathBuf,
}

const HIGHLIGHT_COLOR: Color = Color::Rgb(0, 255, 150);
const PRIMARY_COLOR: Color = Color::LightGreen;

struct Player {
    songs: Vec<Song>,
    current_index: usize,
    selected_index: usize,
    _stream: Option<Box<dyn std::any::Any>>,
    _stream_handle: Option<Box<dyn std::any::Any>>,
    sink: Option<Arc<Mutex<Sink>>>,
    is_playing: bool,
    loop_mode: bool,
    random_mode: bool,
    list_state: ListState,
    playback_start: Option<Instant>,
    song_duration: Option<Duration>,
    seek_offset: Duration,
    show_controls_popup: bool,
}

impl Player {
    fn update_terminal_title(&self) {
        if self.songs.is_empty() {
            return;
        }

        let title = if self.is_playing {
            format!("MUSIX - ♪ {}", self.songs[self.current_index].name)
        } else {
            format!("MUSIX - {} (Paused)", self.songs[self.current_index].name)
        };

        let _ = execute!(io::stdout(), SetTitle(&title));
    }
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let songs = load_mp3_files()?;
        if songs.is_empty() {
            return Err("No MP3 files found".into());
        }

        let mut list_state = ListState::default();
        list_state.select(Some(0));

        // Initialize audio system with Rodio 0.21 API
        let (stream, stream_handle, sink) = match OutputStreamBuilder::open_default_stream() {
            Ok(mut stream_handle) => {
                // Disable logging on drop to prevent CLI/UI interference
                stream_handle.log_on_drop(false);
                let sink = Sink::connect_new(stream_handle.mixer());
                (
                    None, // No separate stream object in 0.21
                    Some(Box::new(stream_handle) as Box<dyn std::any::Any>),
                    Some(Arc::new(Mutex::new(sink))),
                )
            }
            Err(e) => {
                eprintln!("Warning: Could not initialize audio output: {e}");
                eprintln!("The application will continue but audio playback may not work.");
                (None, None, None)
            }
        };

        let player = Player {
            songs,
            current_index: 0,
            selected_index: 0,
            _stream: stream,
            _stream_handle: stream_handle,
            sink,
            is_playing: false,
            loop_mode: true,
            random_mode: false,
            list_state,
            playback_start: None,
            song_duration: None,
            seek_offset: Duration::from_secs(0),
            show_controls_popup: false,
        };

        // Set initial terminal title
        if !player.songs.is_empty() {
            let _ = execute!(io::stdout(), SetTitle(&format!("MUSIX - {}", player.songs[0].name)));
        } else {
            let _ = execute!(io::stdout(), SetTitle("MUSIX"));
        }

        Ok(player)
    }

    fn play_song(&mut self, index: usize) -> Result<(), Box<dyn std::error::Error>> {
        if index >= self.songs.len() {
            return Ok(());
        }

        // Check if this is a new song before updating current_index
        let is_new_song = index != self.current_index;
        
        self.current_index = index;
        self.selected_index = index;
        self.list_state.select(Some(self.selected_index));
        
        // Only reset seek_offset if this is a new song
        if is_new_song {
            self.seek_offset = Duration::from_secs(0);
        }

        if let Some(ref sink) = self.sink {
            let song = &self.songs[index];
            match std::fs::File::open(&song.path) {
                Ok(file) => {
                    match Decoder::new(file) {
                        Ok(source) => {
                            // Try to get duration from the source
                            let total_duration = source.total_duration();

                            let sink = sink.lock().unwrap();
                            sink.stop();

                            // If we have a seek offset, we need to skip ahead
                            if self.seek_offset > Duration::from_secs(0) {
                                let skipped_source = source.skip_duration(self.seek_offset);
                                sink.append(skipped_source);
                            } else {
                                sink.append(source);
                            }

                            sink.play();
                            self.is_playing = true;
                            self.playback_start = Some(Instant::now());
                            self.song_duration = total_duration;
                            self.update_terminal_title();
                        }
                        Err(e) => {
                            eprintln!("Warning: Could not decode audio file '{}': {e}", self.songs[index].name);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Warning: Could not open audio file '{}': {e}", self.songs[index].name);
                }
            }
        } else {
            eprintln!("Warning: No audio sink available. Cannot play '{}'", self.songs[index].name);
        }

        Ok(())
    }

    fn play_or_pause(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // If no songs are loaded, do nothing
        if self.songs.is_empty() {
            return Ok(());
        }

        // If no song has ever been played (initial state), play the selected song
        if self.playback_start.is_none() && !self.is_playing {
            self.play_song(self.selected_index)?;
            return Ok(());
        }

        // If selected song is different from current playing song, play the selected song
        if self.selected_index != self.current_index {
            self.play_song(self.selected_index)?;
        } else {
            // If selected song is the same as current playing song, toggle play/pause
            if self.is_playing {
                self.pause_playback();
                self.update_terminal_title();
            } else {
                self.resume_playback();
                self.update_terminal_title();
            }
        }
        Ok(())
    }

    fn next_song(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.songs.is_empty() {
            return Ok(());
        }

        let next_index = if self.random_mode {
            // Simple random selection using timestamp
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as usize;
            let mut indices: Vec<usize> = (0..self.songs.len()).collect();
            indices.retain(|&i| i != self.current_index);
            if indices.is_empty() {
                self.current_index
            } else {
                indices[timestamp % indices.len()]
            }
        } else if self.current_index + 1 >= self.songs.len() {
            if self.loop_mode { 0 } else { self.current_index }
        } else {
            self.current_index + 1
        };

        self.play_song(next_index)
    }

    fn previous_song(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.songs.is_empty() {
            return Ok(());
        }

        let prev_index = if self.random_mode {
            // Simple random selection using timestamp
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as usize;
            let mut indices: Vec<usize> = (0..self.songs.len()).collect();
            indices.retain(|&i| i != self.current_index);
            if indices.is_empty() {
                self.current_index
            } else {
                indices[timestamp % indices.len()]
            }
        } else if self.current_index == 0 {
            if self.loop_mode { self.songs.len() - 1 } else { 0 }
        } else {
            self.current_index - 1
        };

        self.play_song(prev_index)
    }

    fn move_selection(&mut self, direction: i32) {
        if self.songs.is_empty() {
            return;
        }

        let len = self.songs.len();
        if direction > 0 {
            self.selected_index = (self.selected_index + 1) % len;
        } else if direction < 0 {
            self.selected_index = if self.selected_index == 0 { len - 1 } else { self.selected_index - 1 };
        }
        self.list_state.select(Some(self.selected_index));
    }

    fn get_playback_progress(&self) -> (Duration, Option<Duration>) {
        if let Some(start_time) = self.playback_start {
            let elapsed = start_time.elapsed() + self.seek_offset;
            (elapsed, self.song_duration)
        } else {
            (self.seek_offset, self.song_duration)
        }
    }

    fn format_duration(duration: Duration) -> String {
        let total_seconds = duration.as_secs();
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        format!("{minutes:02}:{seconds:02}")
    }

    fn pause_playback(&mut self) {
        if self.is_playing {
            // Store current progress before pausing
            if let Some(start_time) = self.playback_start {
                self.seek_offset += start_time.elapsed();
            }

            if let Some(ref sink) = self.sink {
                let sink = sink.lock().unwrap();
                sink.pause();
            }
            self.is_playing = false;
            self.playback_start = None;
            self.update_terminal_title();
        }
    }

    fn resume_playback(&mut self) {
        if !self.is_playing && !self.songs.is_empty() {
            if let Some(ref sink) = self.sink {
                let sink = sink.lock().unwrap();
                sink.play();
                self.is_playing = true;
                self.playback_start = Some(Instant::now());
                self.update_terminal_title();
            }
        }
    }

    fn seek(&mut self, offset_seconds: i32) {
        if !self.songs.is_empty() && self.is_playing {
            if let Some(ref sink_arc) = self.sink {
                // Get current actual position (including elapsed time since playback start)
                let current_position = if let Some(start_time) = self.playback_start {
                    self.seek_offset + start_time.elapsed()
                } else {
                    self.seek_offset
                };

                let seek_duration = Duration::from_secs(offset_seconds.unsigned_abs().into());
                let new_position = if offset_seconds < 0 {
                    // Seek backward
                    if current_position > seek_duration {
                        current_position - seek_duration
                    } else {
                        Duration::from_secs(0)
                    }
                } else {
                    // Seek forward - also check against song duration if available
                    let max_duration = self.song_duration.unwrap_or(Duration::from_secs(u64::MAX));
                    let proposed_position = current_position + seek_duration;
                    if proposed_position < max_duration {
                        proposed_position
                    } else {
                        max_duration.saturating_sub(Duration::from_secs(1))
                    }
                };

                // Use rodio's try_seek for fast seeking
                let sink = sink_arc.lock().unwrap();
                match sink.try_seek(new_position) {
                    Ok(()) => {
                        // Seeking succeeded, update our tracking variables
                        self.seek_offset = new_position;
                        self.playback_start = Some(Instant::now());
                    }
                    Err(_) => {
                        // try_seek failed, which means this format doesn't support seeking
                        // Just ignore the seek request rather than doing expensive restart
                        eprintln!("Seeking not supported for this audio format");
                    }
                }
            }
        }
    }
}

fn load_mp3_files() -> Result<Vec<Song>, Box<dyn std::error::Error>> {
    let mut songs = Vec::new();

    // Try multiple directories in order of preference
    let potential_dirs = vec![
        {
            // User's Music directory
            let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(format!("{home_dir}/Music"))
        },
        PathBuf::from("./data"),
    ];

    for data_dir in potential_dirs {
        if data_dir.exists() {
            match visit_dir(&data_dir, &mut songs) {
                Ok(_) => {
                    //eprintln!("Loaded {} MP3 files from: {data_dir:?}", songs.len());  // break;
                }
                Err(e) => {
                    eprintln!("Warning: Could not access directory {data_dir:?}: {e}");
                    continue;
                }
            }
        }
    }

    songs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(songs)
}

fn visit_dir(dir: &PathBuf, songs: &mut Vec<Song>) -> Result<(), Box<dyn std::error::Error>> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                visit_dir(&path, songs)?;
            } else if let Some(extension) = path.extension() {
                if extension.to_str().unwrap_or("").to_lowercase() == "mp3" {
                    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Unknown").to_string();

                    songs.push(Song { name, path: path.clone() });
                }
            }
        }
    }
    Ok(())
}

fn ui(f: &mut Frame, player: &Player) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Min(8),    // Song list
            Constraint::Length(3), // Progress bar
            Constraint::Length(3), // Status
        ])
        .split(f.area());

    // Title
    let title = Paragraph::new("MUSIX")
        .style(Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(PRIMARY_COLOR)));
    f.render_widget(title, chunks[0]);

    // Song list
    let items: Vec<ListItem> = player
        .songs
        .iter()
        .enumerate()
        .map(|(i, song)| {
            let playing_indicator = if i == player.current_index && player.is_playing { "♪ " } else { "  " };

            let content = format!("{playing_indicator}{}. {}", i + 1, song.name);

            let style = if i == player.current_index && player.is_playing {
                Style::default().fg(HIGHLIGHT_COLOR).add_modifier(Modifier::BOLD)
            } else if i == player.selected_index {
                Style::default().fg(PRIMARY_COLOR)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(content).style(style)
        })
        .collect();

    let songs_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Songs")
                .border_style(Style::default().fg(PRIMARY_COLOR)),
        )
        .highlight_style(Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD))
        .scroll_padding(1);

    f.render_stateful_widget(songs_list, chunks[1], &mut player.list_state.clone());

    // Progress bar
    let (elapsed, total) = player.get_playback_progress();
    let progress_ratio = if let Some(duration) = total {
        if duration.as_secs() > 0 {
            (elapsed.as_secs() as f64 / duration.as_secs() as f64).min(1.0)
        } else {
            0.0
        }
    } else {
        0.0
    };

    let progress_label_text = if let Some(duration) = total {
        format!(" {}/{} ", Player::format_duration(elapsed), Player::format_duration(duration))
    } else {
        format!(" {} ", Player::format_duration(elapsed))
    };

    let progress_bar_style = Style::default().fg(PRIMARY_COLOR).bg(Color::default());
    let progress_label = Span::styled(progress_label_text, progress_bar_style);

    let progress_bar = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Progress")
                .border_style(Style::default().fg(PRIMARY_COLOR)),
        )
        .gauge_style(progress_bar_style)
        .ratio(progress_ratio)
        .label(progress_label);
    f.render_widget(progress_bar, chunks[2]);

    // Status
    let mode_text = if player.random_mode { "RANDOM" } else { "NORMAL" };

    let status = Paragraph::new(vec![Line::from(vec![
        Span::raw(format!("  Mode: {} | Songs: {} | ", mode_text, player.songs.len())),
        Span::styled("X", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
        Span::raw(": Help  "),
    ])])
    .alignment(Alignment::Left)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Status")
            .border_style(Style::default().fg(PRIMARY_COLOR)),
    );
    f.render_widget(status, chunks[3]);

    // Controls popup
    if player.show_controls_popup {
        let popup_area = centered_rect(60, 60, f.area());
        f.render_widget(ratatui::widgets::Clear, popup_area);

        let controls_popup = Paragraph::new(vec![
            Line::from(""),
            Line::from(vec![Span::styled("CONTROLS", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD))]).alignment(Alignment::Center),
            Line::from(""),
            Line::from(vec![
                Span::styled(" ↑/↓", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Navigate songs"),
            ]),
            Line::from(vec![
                Span::styled(" ␣/↵", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Play Pause"),
            ]),
            Line::from(vec![
                Span::styled(" ←/→", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Play prev/next song"),
            ]),
            Line::from(vec![
                Span::styled(" ,/.", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Seek ±5 seconds"),
            ]),
            Line::from(vec![
                Span::styled(" R  ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Toggle random mode"),
            ]),
            Line::from(vec![
                Span::styled(" X  ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Close this popup"),
            ]),
            Line::from(vec![
                Span::styled(" Esc", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Exit application"),
            ]),
        ])
        .alignment(Alignment::Left)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Help")
                .border_style(Style::default().fg(PRIMARY_COLOR)),
        );
        f.render_widget(controls_popup, popup_area);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: ratatui::prelude::Rect) -> ratatui::prelude::Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn run_player() -> Result<(), Box<dyn std::error::Error>> {
    let mut player = match Player::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Player initialization failed: {e}");
            eprintln!("Error details: {e:?}");
            std::process::exit(1);
        }
    };

    if player.songs.is_empty() {
        println!("No MP3 files found in any accessible directory.");
        println!("MUSIX searched for MP3 files in:");
        println!("  - ~/Music (user's music directory)");
        println!("  - ./data (current directory)");
        println!();
        println!("To test MUSIX, you can:");
        println!("Copy MP3 files to ./data directory");
        return Ok(());
    }

    match enable_raw_mode() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Failed to enable raw mode: {e}");
            return Err(e.into());
        }
    }

    let mut stdout = io::stdout();
    match execute!(stdout, EnterAlternateScreen) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Failed to enter alternate screen: {e}");
            return Err(e.into());
        }
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Failed to create terminal: {e}");
            return Err(e.into());
        }
    };

    let result = main_loop(&mut terminal, &mut player);

    // Clean shutdown of audio to prevent warning messages
    if let Some(ref sink) = player.sink {
        let sink = sink.lock().unwrap();
        sink.stop();
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Reset terminal title
    let _ = execute!(io::stdout(), SetTitle("Terminal"));

    result
}

fn main_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, player: &mut Player) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        terminal.draw(|f| ui(f, player))?;

        if let Ok(true) = event::poll(Duration::from_millis(100)) {
            if let Ok(Event::Key(key)) = event::read() {
                match key {
                    KeyEvent {
                        code: KeyCode::Esc,
                        modifiers: KeyModifiers::NONE,
                        ..
                    }
                    | KeyEvent {
                        code: KeyCode::Char('c'),
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    } => break,

                    KeyEvent {
                        code: KeyCode::Up,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        player.move_selection(-1);
                    }

                    KeyEvent {
                        code: KeyCode::Down,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        player.move_selection(1);
                    }

                    KeyEvent {
                        code: KeyCode::Enter | KeyCode::Char(' '),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        let _ = player.play_or_pause();
                    }

                    KeyEvent {
                        code: KeyCode::Left,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        player.previous_song()?;
                    }

                    KeyEvent {
                        code: KeyCode::Right,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        player.next_song()?;
                    }

                    KeyEvent {
                        code: KeyCode::Char('r'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        player.random_mode = !player.random_mode;
                    }

                    KeyEvent {
                        code: KeyCode::Char('x'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        player.show_controls_popup = !player.show_controls_popup;
                    }

                    KeyEvent {
                        code: KeyCode::Char('<') | KeyCode::Char(','),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        player.seek(-5); // Seek backward 5 seconds
                    }

                    KeyEvent {
                        code: KeyCode::Char('>') | KeyCode::Char('.'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        player.seek(5); // Seek forward 5 seconds
                    }

                    _ => {}
                }
            }
        }

        // Check if current song finished and auto-play next
        if player.is_playing {
            if let Some(ref sink) = player.sink {
                let sink = sink.lock().unwrap();
                if sink.empty() {
                    drop(sink);
                    player.is_playing = false;
                    player.playback_start = None;
                    player.seek_offset = Duration::from_secs(0);
                    player.next_song()?;
                }
            }
        }
    }

    Ok(())
}

fn main() {
    if let Err(e) = run_player() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(Player::format_duration(Duration::from_secs(0)), "00:00");
        assert_eq!(Player::format_duration(Duration::from_secs(30)), "00:30");
        assert_eq!(Player::format_duration(Duration::from_secs(60)), "01:00");
        assert_eq!(Player::format_duration(Duration::from_secs(125)), "02:05");
    }
}
