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
use rodio::{Decoder, OutputStream, Sink, Source};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

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
    search_mode: bool,
    search_query: String,
    filtered_songs: Vec<usize>,
    g_pressed: bool,
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

        // Initialize audio system with Rodio 0.20 API
        let (stream, stream_handle, sink) = match OutputStream::try_default() {
            Ok((stream, stream_handle)) => match Sink::try_new(&stream_handle) {
                Ok(sink) => (
                    Some(Box::new(stream) as Box<dyn std::any::Any>),
                    Some(Box::new(stream_handle) as Box<dyn std::any::Any>),
                    Some(Arc::new(Mutex::new(sink))),
                ),
                Err(e) => {
                    eprintln!("Warning: Could not create audio sink: {e}");
                    (
                        Some(Box::new(stream) as Box<dyn std::any::Any>),
                        Some(Box::new(stream_handle) as Box<dyn std::any::Any>),
                        None,
                    )
                }
            },
            Err(e) => {
                eprintln!("Warning: Could not initialize audio output: {e}");
                eprintln!("The application will continue but audio playback may not work.");
                (None, None, None)
            }
        };

        let filtered_songs: Vec<usize> = (0..songs.len()).collect();

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
            search_mode: false,
            search_query: String::new(),
            filtered_songs,
            g_pressed: false,
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

        let is_same_song = self.current_index == index;
        self.current_index = index;
        self.selected_index = index;
        self.list_state.select(Some(self.selected_index));

        // Only reset seek_offset if it's a different song
        if !is_same_song {
            self.seek_offset = Duration::from_secs(0);
        }
        if let Some(ref sink) = self.sink {
            let song = &self.songs[index];
            match create_audio_source(&song.path) {
                Ok(source) => {
                    // Try to get duration from symphonia first, fallback to source
                    let total_duration = get_audio_duration(&song.path).or_else(|| source.total_duration());

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
                    eprintln!("Warning: Could not decode audio file '{}': {e}", song.name);
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

                // Check if sink is empty (which happens after pause in some cases)
                if sink.empty() {
                    // If sink is empty, we need to reload the song from the current position
                    drop(sink);
                    let _ = self.play_song(self.current_index);
                } else {
                    // If sink still has content, just resume playback
                    sink.play();
                    self.is_playing = true;
                    self.playback_start = Some(Instant::now());
                    self.update_terminal_title();
                }
            }
        }
    }

    fn seek(&mut self, offset_seconds: i32) {
        if !self.songs.is_empty() && self.is_playing {
            if let Some(ref sink) = self.sink {
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
                    // Seek forward
                    current_position + seek_duration
                };

                // Try to seek using rodio's try_seek method
                let sink = sink.lock().unwrap();
                match sink.try_seek(new_position) {
                    Ok(()) => {
                        // Seeking succeeded, update our tracking variables
                        self.seek_offset = new_position;
                        self.playback_start = Some(Instant::now());
                    }
                    Err(_) => {
                        // Seeking failed, fall back to restarting from new position
                        drop(sink);
                        self.seek_offset = new_position;
                        let _ = self.play_song(self.current_index);
                    }
                }
            }
        }
    }

    fn fuzzy_search(&mut self, query: &str) {
        if query.is_empty() {
            self.filtered_songs = (0..self.songs.len()).collect();
        } else {
            let query_lower = query.to_lowercase();
            let mut matches: Vec<(usize, f32)> = self
                .songs
                .iter()
                .enumerate()
                .filter_map(|(index, song)| {
                    let song_name_lower = song.name.to_lowercase();
                    let score = Self::fuzzy_match_score(&query_lower, &song_name_lower);
                    if score > 0.0 { Some((index, score)) } else { None }
                })
                .collect();

            matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            self.filtered_songs = matches.into_iter().map(|(index, _)| index).collect();
        }

        if !self.filtered_songs.is_empty() {
            self.selected_index = self.filtered_songs[0];
            self.list_state.select(Some(0));
        }
    }

    fn fuzzy_match_score(query: &str, text: &str) -> f32 {
        if query.is_empty() {
            return 1.0;
        }

        if text.contains(query) {
            let exact_match_bonus = if text == query { 2.0 } else { 1.5 };
            let starts_with_bonus = if text.starts_with(query) { 1.2 } else { 1.0 };
            return exact_match_bonus * starts_with_bonus;
        }

        let mut score = 0.0;
        let query_chars: Vec<char> = query.chars().collect();
        let text_chars: Vec<char> = text.chars().collect();
        let mut query_index = 0;

        for (text_index, text_char) in text_chars.iter().enumerate() {
            if query_index < query_chars.len() && *text_char == query_chars[query_index] {
                score += 1.0 / (text_index as f32 + 1.0);
                query_index += 1;
            }
        }

        if query_index == query_chars.len() {
            score / query_chars.len() as f32
        } else {
            0.0
        }
    }

    fn enter_search_mode(&mut self) {
        self.search_mode = true;
        self.search_query.clear();
        self.fuzzy_search("");
    }

    fn exit_search_mode(&mut self) {
        self.search_mode = false;
        self.search_query.clear();
        self.filtered_songs = (0..self.songs.len()).collect();
        self.list_state.select(Some(self.selected_index));
    }

    fn get_display_songs(&self) -> Vec<(usize, &Song)> {
        if self.search_mode {
            self.filtered_songs.iter().map(|&index| (index, &self.songs[index])).collect()
        } else {
            self.songs.iter().enumerate().collect()
        }
    }

    fn move_selection_in_search(&mut self, direction: i32) {
        if self.filtered_songs.is_empty() {
            return;
        }

        let current_filtered_index = self.filtered_songs.iter().position(|&index| index == self.selected_index).unwrap_or(0);

        let new_filtered_index = if direction > 0 {
            (current_filtered_index + 1) % self.filtered_songs.len()
        } else if direction < 0 {
            if current_filtered_index == 0 {
                self.filtered_songs.len() - 1
            } else {
                current_filtered_index - 1
            }
        } else {
            current_filtered_index
        };

        self.selected_index = self.filtered_songs[new_filtered_index];
        self.list_state.select(Some(new_filtered_index));
    }

    fn jump_to_first(&mut self) {
        if self.songs.is_empty() {
            return;
        }

        if self.search_mode {
            if !self.filtered_songs.is_empty() {
                self.selected_index = self.filtered_songs[0];
                self.list_state.select(Some(0));
            }
        } else {
            self.selected_index = 0;
            self.list_state.select(Some(0));
        }
    }

    fn jump_to_last(&mut self) {
        if self.songs.is_empty() {
            return;
        }

        if self.search_mode {
            if !self.filtered_songs.is_empty() {
                let last_index = self.filtered_songs.len() - 1;
                self.selected_index = self.filtered_songs[last_index];
                self.list_state.select(Some(last_index));
            }
        } else {
            self.selected_index = self.songs.len() - 1;
            self.list_state.select(Some(self.selected_index));
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

fn create_audio_source(path: &PathBuf) -> Result<Box<dyn Source<Item = i16> + Send>, Box<dyn std::error::Error>> {
    // For now, just use rodio's built-in decoder
    // AAC support would require more complex implementation
    let file = std::fs::File::open(path)?;
    let source = Decoder::new(file)?;
    Ok(Box::new(source))
}

fn get_audio_duration(path: &PathBuf) -> Option<Duration> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return None,
    };

    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension() {
        if let Some(ext_str) = extension.to_str() {
            hint.with_extension(ext_str);
        }
    }

    let meta_opts: MetadataOptions = Default::default();
    let fmt_opts: FormatOptions = Default::default();

    match symphonia::default::get_probe().format(&hint, mss, &fmt_opts, &meta_opts) {
        Ok(mut probed) => {
            let format = &mut probed.format;
            let track = match format
                .tracks()
                .iter()
                .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
            {
                Some(track) => track,
                None => return None,
            };

            // Try multiple methods to get duration
            if let (Some(time_base), Some(n_frames)) = (track.codec_params.time_base, track.codec_params.n_frames) {
                let duration_secs = n_frames as f64 * time_base.numer as f64 / time_base.denom as f64;
                return Some(Duration::from_secs_f64(duration_secs));
            }

            // Alternative method for formats like AAC/FLAC/OPUS that might not have n_frames
            if let Some(sample_rate) = track.codec_params.sample_rate {
                // Store codec type to avoid borrowing issues
                let codec_type = track.codec_params.codec;
                
                // Try to read through the entire format to count samples
                let mut packet_count = 0u64;
                let mut sample_count = 0u64;
                
                loop {
                    match format.next_packet() {
                        Ok(_packet) => {
                            packet_count += 1;
                            // Estimate samples per packet based on codec
                            let samples_per_packet = match codec_type {
                                symphonia::core::codecs::CODEC_TYPE_AAC => 1024,
                                symphonia::core::codecs::CODEC_TYPE_FLAC => 4096, // Variable, but reasonable estimate
                                symphonia::core::codecs::CODEC_TYPE_VORBIS => 1024,
                                _ => 1152, // Default for MP3
                            };
                            sample_count += samples_per_packet;
                        }
                        Err(_) => break,
                    }
                    // Limit iteration to prevent infinite loops on corrupted files
                    if packet_count > 1000000 {
                        break;
                    }
                }
                
                if sample_count > 0 {
                    let duration_secs = sample_count as f64 / sample_rate as f64;
                    return Some(Duration::from_secs_f64(duration_secs));
                }
            }

            None
        }
        Err(_) => None,
    }
}

fn visit_dir(dir: &PathBuf, songs: &mut Vec<Song>) -> Result<(), Box<dyn std::error::Error>> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                visit_dir(&path, songs)?;
            } else if let Some(extension) = path.extension() {
                let ext_lower = extension.to_str().unwrap_or("").to_lowercase();
                if ext_lower == "mp3" || ext_lower == "aac" || ext_lower == "wav" || ext_lower == "flac" || ext_lower == "opus" {
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
    let display_songs = player.get_display_songs();
    let items: Vec<ListItem> = display_songs
        .iter()
        .enumerate()
        .map(|(_display_index, &(actual_index, song))| {
            let playing_indicator = if actual_index == player.current_index && player.is_playing {
                "♪ "
            } else {
                "  "
            };

            let content = format!("{playing_indicator}{}. {}", actual_index + 1, song.name);

            let style = if actual_index == player.current_index && player.is_playing {
                Style::default().fg(HIGHLIGHT_COLOR).add_modifier(Modifier::BOLD)
            } else if actual_index == player.selected_index {
                Style::default().fg(PRIMARY_COLOR)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(content).style(style)
        })
        .collect();

    let songs_title = if player.search_mode {
        format!("Songs - Search: {}", player.search_query)
    } else {
        "Songs".to_string()
    };

    let songs_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(songs_title)
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
    let song_count = if player.search_mode {
        format!("{}/{}", player.filtered_songs.len(), player.songs.len())
    } else {
        player.songs.len().to_string()
    };

    let status_content = if player.search_mode {
        vec![Line::from(vec![
            Span::raw(format!("  Search Mode | Songs: {} | ", song_count)),
            Span::styled("Esc", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
            Span::raw(": Exit Search | "),
            Span::styled("Enter", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
            Span::raw(": Play  "),
        ])]
    } else {
        vec![Line::from(vec![
            Span::raw(format!("  Mode: {} | Songs: {} | ", mode_text, song_count)),
            Span::styled("/", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
            Span::raw(": Search | "),
            Span::styled("x", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
            Span::raw(": Help  "),
        ])]
    };

    let status = Paragraph::new(status_content).alignment(Alignment::Left).block(
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
                Span::styled(" ↑/↓ or j/k", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Navigate songs"),
            ]),
            Line::from(vec![
                Span::styled(" Space/↵   ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Play/Pause"),
            ]),
            Line::from(vec![
                Span::styled(" ←/→ or h/l", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Play prev/next song"),
            ]),
            Line::from(vec![
                Span::styled(" gg/G      ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Jump to first/last"),
            ]),
            Line::from(vec![
                Span::styled(" /         ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Enter search mode"),
            ]),
            Line::from(vec![
                Span::styled(" n/N       ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Next/prev search"),
            ]),
            Line::from(vec![
                Span::styled(" ,/.       ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Seek ±5 seconds"),
            ]),
            Line::from(vec![
                Span::styled(" r         ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Toggle random mode"),
            ]),
            Line::from(vec![
                Span::styled(" q/Esc     ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Exit application"),
            ]),
            Line::from(vec![
                Span::styled(" x         ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Close this popup"),
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
                // Reset g_pressed state for any key except 'g'
                if key.code != KeyCode::Char('g') || key.modifiers != KeyModifiers::NONE {
                    player.g_pressed = false;
                }

                match key {
                    KeyEvent {
                        code: KeyCode::Esc,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.show_controls_popup {
                            player.show_controls_popup = false;
                        } else if player.search_mode {
                            player.exit_search_mode();
                        } else {
                            break;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('c'),
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    } => break,

                    KeyEvent {
                        code: KeyCode::Up,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.move_selection_in_search(-1);
                        } else {
                            player.move_selection(-1);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('k'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('k');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.move_selection(-1);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Down,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.move_selection_in_search(1);
                        } else {
                            player.move_selection(1);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('j'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('j');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.move_selection(1);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Enter,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        let _ = player.play_or_pause();
                        if player.search_mode {
                            player.exit_search_mode();
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char(' '),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push(' ');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            let _ = player.play_or_pause();
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Left,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if !player.search_mode {
                            player.previous_song()?;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Right,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if !player.search_mode {
                            player.next_song()?;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('h'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('h');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.previous_song()?;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('l'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('l');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.next_song()?;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('n'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('n');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        }
                        // In normal mode, 'n' has no special meaning
                    }

                    KeyEvent {
                        code: KeyCode::Char('N'),
                        modifiers: KeyModifiers::SHIFT,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('N');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        }
                        // In normal mode, 'N' has no special meaning, ignore
                    }

                    KeyEvent {
                        code: KeyCode::Char('g'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('g');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            if player.g_pressed {
                                // Second 'g' - jump to first song
                                player.jump_to_first();
                                player.g_pressed = false;
                            } else {
                                // First 'g' - set flag and wait for second 'g'
                                player.g_pressed = true;
                            }
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('G'),
                        modifiers: KeyModifiers::SHIFT,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('G');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.jump_to_last();
                            player.g_pressed = false; // Reset g_pressed state
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('q'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('q');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            break; // Quit the application
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('r'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('r');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.random_mode = !player.random_mode;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('x'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('x');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.show_controls_popup = !player.show_controls_popup;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('<') | KeyCode::Char(','),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            let c = if key.code == KeyCode::Char('<') { '<' } else { ',' };
                            player.search_query.push(c);
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.seek(-5); // Seek backward 5 seconds
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('>') | KeyCode::Char('.'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            let c = if key.code == KeyCode::Char('>') { '>' } else { '.' };
                            player.search_query.push(c);
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.seek(5); // Seek forward 5 seconds
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('/'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if !player.search_mode {
                            player.enter_search_mode();
                        } else {
                            player.search_query.push('/');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Backspace,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.pop();
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char(c),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push(c);
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        }
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
