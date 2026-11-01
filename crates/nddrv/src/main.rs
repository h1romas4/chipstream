mod library;
mod playback;

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui_kit::component::ElementExt;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::input::{
    InlineToken, InlineTokenSpan, Input, InputEvent, InputState, InputToken,
};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::scroll::{ScrollableElement, Scrollbar, ScrollbarHandle, ScrollbarMode};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, Theme, ThemeMode, TitleBar, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use library::{Config, DirectoryEntry, ScanProgress, Track};
use playback::{Event, Playback};

const ACCENT_COLOR: u32 = 0x2fb3aa;

struct LibraryScanNotification;

fn error_notification(title: &'static str, message: impl Into<SharedString>) -> Notification {
    Notification::error(message).title(title).autohide(false)
}

fn error_details_notification(
    title: &'static str,
    message: impl Into<SharedString>,
    errors: Vec<String>,
) -> Notification {
    let details: SharedString = errors.join("\n").into();
    error_notification(title, message).content(move |_, _, _| {
        div()
            .id("error-details")
            .max_h(px(180.))
            .overflow_y_scrollbar()
            .child(div().text_sm().child(details.clone()))
            .into_any_element()
    })
}

struct AppView {
    config_path: Option<PathBuf>,
    config: Config,
    tracks: Vec<Track>,
    track_by_path: HashMap<PathBuf, usize>,
    filtered_tracks: Vec<usize>,
    selected_tracks: HashSet<PathBuf>,
    selected_queue: HashSet<usize>,
    track_anchor: Option<PathBuf>,
    queue_anchor: Option<usize>,
    track_focus: FocusHandle,
    queue_focus: FocusHandle,
    playlist_titles: HashMap<PathBuf, String>,
    directories: Vec<DirectoryEntry>,
    visible_directories: Vec<usize>,
    expanded_directories: HashMap<PathBuf, HashSet<PathBuf>>,
    selected_directory: Option<PathBuf>,
    library_scroll: UniformListScrollHandle,
    queue_scroll: UniformListScrollHandle,
    directory_scroll: UniformListScrollHandle,
    panel_state: Entity<ResizableState>,
    keep_side_widths: Option<bool>,
    query: Entity<InputState>,
    available_chips: Vec<&'static str>,
    suggestion_index: usize,
    suggestions_hidden: bool,
    _subscription: Subscription,
    volume: Entity<SliderState>,
    _volume_subscription: Subscription,
    player: Playback,
    playing: Option<usize>,
    paused: bool,
    looped: bool,
    elapsed: u64,
    playing_title: String,
    title_viewport_width: Pixels,
    measured_title: Option<(String, Pixels)>,
    status: String,
    persistence_error: Option<String>,
    pending_errors: Vec<Notification>,
    scanning: bool,
    scan_id: u64,
    scan_progress: Option<Arc<Mutex<ScanProgress>>>,
    displayed_scan_progress: Option<ScanProgress>,
    scan_progress_updated_at: Option<Instant>,
    completed_scan_notification: Option<(usize, Vec<String>)>,
    _poll: Task<()>,
}

impl AppView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter text or type a chip name"));
        let subscription =
            cx.subscribe_in(&query, window, |view: &mut Self, _, event, window, cx| {
                match event {
                    InputEvent::Change => {
                        view.suggestion_index = 0;
                        view.suggestions_hidden = false;
                        view.update_filter(cx);
                    }
                    InputEvent::Focus => {
                        view.suggestions_hidden = false;
                    }
                    InputEvent::Blur => {}
                    InputEvent::PressEnter { .. } => {
                        if let Some((_, chips)) = view.chip_suggestions(window, cx)
                            && let Some(chip) = chips.get(view.suggestion_index).copied()
                        {
                            view.accept_chip(chip, window, cx);
                        }
                    }
                }
                cx.notify();
            });
        let volume = cx.new(|_| SliderState::new().default_value(100.));
        let volume_subscription = cx.subscribe(&volume, |view: &mut Self, _, event, cx| {
            if let SliderEvent::Change(SliderValue::Single(value)) = event {
                view.player.set_volume(*value / 100.);
                cx.notify();
            }
        });
        let (config_path, config, config_error) = match library::config_path() {
            Ok(path) => match library::load_config(&path) {
                Ok(config) => (Some(path), config, None),
                Err(error) => (None, Config::default(), Some(format!("{error:#}"))),
            },
            Err(error) => (None, Config::default(), Some(format!("{error:#}"))),
        };
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if this
                    .update_in(cx, |view, window, cx| view.poll(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let pending_errors = config_error
            .as_ref()
            .map(|error| error_notification("Configuration error", error.clone()))
            .into_iter()
            .collect();
        let persistence_error = config_error;
        let expanded_directories = HashMap::new();
        Self {
            config_path,
            config,
            tracks: Vec::new(),
            track_by_path: HashMap::new(),
            filtered_tracks: Vec::new(),
            selected_tracks: HashSet::new(),
            selected_queue: HashSet::new(),
            track_anchor: None,
            queue_anchor: None,
            track_focus: cx.focus_handle(),
            queue_focus: cx.focus_handle(),
            playlist_titles: HashMap::new(),
            directories: Vec::new(),
            visible_directories: Vec::new(),
            expanded_directories,
            selected_directory: None,
            library_scroll: UniformListScrollHandle::default(),
            queue_scroll: UniformListScrollHandle::default(),
            directory_scroll: UniformListScrollHandle::default(),
            panel_state: cx.new(|_| ResizableState::default()),
            keep_side_widths: None,
            query,
            available_chips: Vec::new(),
            suggestion_index: 0,
            suggestions_hidden: false,
            _subscription: subscription,
            volume,
            _volume_subscription: volume_subscription,
            player: Playback::new(),
            playing: None,
            paused: false,
            looped: false,
            elapsed: 0,
            playing_title: String::new(),
            title_viewport_width: px(0.),
            measured_title: None,
            status: "Ready".into(),
            persistence_error,
            pending_errors,
            scanning: false,
            scan_id: 0,
            scan_progress: None,
            displayed_scan_progress: None,
            scan_progress_updated_at: None,
            completed_scan_notification: None,
            _poll: poll,
        }
    }

    fn save(&mut self) {
        let error = match self.config_path.as_deref() {
            Some(path) => library::save_config(path, &self.config)
                .err()
                .map(|error| format!("Could not save library: {error:#}")),
            None => Some("Configuration unavailable; changes cannot be saved".into()),
        };
        if error != self.persistence_error
            && let Some(message) = &error
        {
            self.pending_errors
                .push(error_notification("Configuration error", message.clone()));
        }
        self.persistence_error = error;
    }

    fn update_filter(&mut self, cx: &mut Context<Self>) {
        let input = self.query.read(cx);
        let (query, chips) = search_terms(&input.value(), input.tokens());
        self.filtered_tracks = library::filter_indices_with_chips(
            &self.tracks,
            &query,
            self.selected_directory.as_deref(),
            &chips,
        );
        if !self.selected_tracks.is_empty() || self.track_anchor.is_some() {
            let visible: HashSet<_> = self
                .filtered_tracks
                .iter()
                .map(|&index| &self.tracks[index].path)
                .collect();
            self.selected_tracks.retain(|path| visible.contains(path));
            if self
                .track_anchor
                .as_ref()
                .is_some_and(|path| !visible.contains(path))
            {
                self.track_anchor = None;
            }
        }
        self.library_scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    fn chip_suggestions(
        &self,
        window: &Window,
        cx: &App,
    ) -> Option<(Range<usize>, Vec<&'static str>)> {
        let input = self.query.read(cx);
        if !input.focus_handle(cx).is_focused(window) || self.suggestions_hidden {
            return None;
        }
        chip_suggestions(
            &input.value(),
            input.cursor(),
            input.tokens(),
            &self.available_chips,
        )
    }

    fn accept_chip(&mut self, chip: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((range, suggestions)) = self.chip_suggestions(window, cx) else {
            return;
        };
        if !suggestions.contains(&chip) {
            return;
        }
        let result = self.query.update(cx, |input, cx| {
            let result =
                input.replace_range_with_token(range, InlineToken::new(chip, chip), window, cx);
            input.focus(window, cx);
            result
        });
        if let Err(error) = result {
            self.pending_errors.push(error_notification(
                "Could not add chip filter",
                format!("{error:?}"),
            ));
        } else {
            self.suggestions_hidden = true;
            self.update_filter(cx);
        }
        cx.notify();
    }

    fn search_input(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let suggestions = self.chip_suggestions(window, cx).map(|(_, chips)| chips);
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                let Some((_, chips)) = view.chip_suggestions(window, cx) else {
                    return;
                };
                match event.keystroke.key.as_str() {
                    "down" => {
                        view.suggestion_index = (view.suggestion_index + 1).min(chips.len() - 1);
                        cx.stop_propagation();
                    }
                    "up" => {
                        view.suggestion_index = view.suggestion_index.saturating_sub(1);
                        cx.stop_propagation();
                    }
                    "escape" => {
                        view.suggestions_hidden = true;
                        cx.stop_propagation();
                    }
                    _ => return,
                }
                cx.notify();
            }))
            .child(Input::new(&self.query).token(|token, _, _| {
                div().max_w(token.available_width()).pr(px(4.)).child(
                    InputToken::new(token).when(!token.is_selected(), |tag| {
                        tag.bg(rgb(0x71418e)).border_color(rgb(0xb77ad4))
                    }),
                )
            }))
            .when_some(suggestions, |input, chips| {
                input.child(
                    div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .p_1()
                        .rounded_md()
                        .bg(rgb(0x202c38))
                        .border_1()
                        .border_color(rgb(0x485969))
                        .children(chips.into_iter().enumerate().map(|(index, chip)| {
                            div()
                                .id(SharedString::from(format!("chip-suggestion-{chip}")))
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .cursor_pointer()
                                .when(index == self.suggestion_index, |row| row.bg(rgb(0x39495a)))
                                .hover(|style| style.bg(rgb(0x39495a)))
                                .child(chip)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |view, _, window, cx| {
                                        cx.stop_propagation();
                                        view.accept_chip(chip, window, cx);
                                    }),
                                )
                        })),
                )
            })
    }

    fn add_selected_tracks(&mut self, cx: &mut Context<Self>) {
        let paths: Vec<_> = self
            .filtered_tracks
            .iter()
            .map(|&index| &self.tracks[index].path)
            .filter(|path| self.selected_tracks.contains(*path))
            .cloned()
            .collect();
        let count = paths.len();
        self.config.playlist.extend(paths);
        self.status = format!("Added {count} tracks to playlist");
        self.save();
        cx.notify();
    }

    fn remove_queue_selection(&mut self, clear_all: bool, cx: &mut Context<Self>) {
        let removed = if clear_all {
            (0..self.config.playlist.len()).collect()
        } else {
            std::mem::take(&mut self.selected_queue)
        };
        let count = removed.len();
        let (playing, stopped) =
            remove_playlist_entries(&mut self.config.playlist, &removed, self.playing);
        self.selected_queue.clear();
        self.queue_anchor = None;
        let remaining: HashSet<_> = self.config.playlist.iter().collect();
        self.playlist_titles
            .retain(|path, _| remaining.contains(path));
        self.playing = playing;
        if stopped {
            self.player.stop();
            self.paused = false;
            self.elapsed = 0;
        }
        self.status = format!("Removed {count} tracks from playlist");
        self.save();
        cx.notify();
    }

    fn update_directory_tree(&mut self) {
        self.visible_directories =
            library::visible_directories(&self.directories, &self.expanded_directories);
    }

    fn load_playlist_titles(&mut self, cx: &mut Context<Self>) {
        self.playlist_titles.clear();
        let paths: HashSet<_> = self
            .config
            .playlist
            .iter()
            .filter(|path| !self.track_by_path.contains_key(*path))
            .cloned()
            .collect();
        if paths.is_empty() {
            return;
        }
        let scan_id = self.scan_id;
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_spawn(async move {
                    paths
                        .into_iter()
                        .map(|path| {
                            let title = library::read_metadata(&path)
                                .map(|metadata| Track::format_display_title(&path, &metadata));
                            (path, title)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.scan_id != scan_id {
                    return;
                }
                let remaining: HashSet<_> = view.config.playlist.iter().collect();
                let mut errors = Vec::new();
                for (path, result) in results {
                    if !remaining.contains(&path) || view.track_by_path.contains_key(&path) {
                        continue;
                    }
                    match result {
                        Ok(title) => {
                            view.playlist_titles.insert(path, title);
                        }
                        Err(error) => errors.push(format!("{}: {error:#}", path.display())),
                    }
                }
                if !errors.is_empty() {
                    view.pending_errors.push(error_details_notification(
                        "Playlist metadata errors",
                        format!("{} errors", errors.len()),
                        errors,
                    ));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn scan(&mut self, cx: &mut Context<Self>) {
        self.scan_id = self.scan_id.wrapping_add(1);
        let scan_id = self.scan_id;
        let dirs = self.config.directories.clone();
        let progress = Arc::new(Mutex::new(ScanProgress::default()));
        self.scan_progress = Some(progress.clone());
        self.displayed_scan_progress = None;
        self.scan_progress_updated_at = None;
        self.completed_scan_notification = None;
        self.scanning = true;
        self.status = "Scanning directories...".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (result, track_by_path) = cx
                .background_spawn(async move {
                    let result = library::scan_with_progress(&dirs, |value| {
                        *progress.lock().expect("scan progress mutex poisoned") = value;
                    });
                    let track_by_path = result
                        .tracks
                        .iter()
                        .enumerate()
                        .map(|(index, track)| (track.path.clone(), index))
                        .collect();
                    (result, track_by_path)
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.scan_id != scan_id {
                    return;
                }
                view.scanning = false;
                view.tracks = result.tracks;
                view.track_by_path = track_by_path;
                view.available_chips = view
                    .tracks
                    .iter()
                    .flat_map(|track| track.chips.iter().map(|chip| chip.name))
                    .collect();
                view.available_chips.sort_unstable();
                view.available_chips.dedup();
                view.directories = result.directories;
                for directory in &view.directories {
                    if directory.depth == 0 {
                        view.expanded_directories
                            .entry(directory.root.clone())
                            .or_insert_with(|| HashSet::from([directory.path.clone()]));
                    }
                }
                if view.selected_directory.as_ref().is_some_and(|selected| {
                    !view.directories.iter().any(|entry| &entry.path == selected)
                }) {
                    view.selected_directory = None;
                }
                view.update_directory_tree();
                view.update_filter(cx);
                view.status = format!("{} tracks found", view.tracks.len());
                view.scan_progress = None;
                view.completed_scan_notification = Some((view.tracks.len(), result.errors));
                view.load_playlist_titles(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn add_directory(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let selection = cx
                .background_spawn(async { rfd::FileDialog::new().pick_folder() })
                .await;
            let Some(path) = selection else {
                return;
            };
            let _ = this.update(cx, |view, cx| match path.canonicalize() {
                Ok(path) if !view.config.directories.contains(&path) => {
                    view.config.directories.push(path);
                    view.save();
                    view.scan(cx);
                }
                Ok(_) => {
                    view.status = "Directory is already registered".into();
                    cx.notify();
                }
                Err(error) => {
                    view.pending_errors.push(error_notification(
                        "Cannot open directory",
                        error.to_string(),
                    ));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn play(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(path) = self.config.playlist.get(index).cloned() {
            self.status = format!("Playing: {}", path.display());
            self.playing = Some(index);
            self.paused = false;
            self.elapsed = 0;
            let indexed = self.track_by_path.get(&path).copied();
            self.playing_title = indexed
                .map(|index| self.tracks[index].display_title())
                .unwrap_or_else(|| Track::format_display_title(&path, &Default::default()));
            self.player.play(path.clone(), self.looped);
            if indexed.is_none() {
                let generation = self.player.generation();
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_spawn(async move {
                            library::read_metadata(&path)
                                .map(|metadata| Track::format_display_title(&path, &metadata))
                        })
                        .await;
                    let _ = this.update(cx, |view, cx| {
                        if view.playing.is_some() && view.player.is_current(generation) {
                            match result {
                                Ok(title) => view.playing_title = title,
                                Err(error) => view.pending_errors.push(error_notification(
                                    "Metadata error",
                                    format!("{error:#}"),
                                )),
                            }
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            self.queue_scroll
                .scroll_to_item(index, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((count, errors)) = self.completed_scan_notification.take() {
            let notification = if errors.is_empty() {
                Notification::success(format!("{count} tracks found"))
                    .title("Library scan complete")
            } else {
                let summary = format!("{count} tracks found; {} errors", errors.len());
                error_details_notification("Library scan completed with errors", summary, errors)
            };
            window.push_notification(notification.id::<LibraryScanNotification>(), cx);
        } else if let Some(progress) = &self.scan_progress {
            let progress = *progress.lock().expect("scan progress mutex poisoned");
            if self.displayed_scan_progress != Some(progress)
                && self
                    .scan_progress_updated_at
                    .is_none_or(|updated| updated.elapsed() >= Duration::from_secs(1))
            {
                self.displayed_scan_progress = Some(progress);
                self.scan_progress_updated_at = Some(Instant::now());
                window.push_notification(
                    Notification::info(format!(
                        "{} files scanned; {} errors",
                        progress.files, progress.errors
                    ))
                    .title("Scanning library")
                    .id::<LibraryScanNotification>()
                    .autohide(false),
                    cx,
                );
            }
        }
        if self.playing.is_some() {
            let elapsed = self.player.elapsed_seconds();
            if elapsed != self.elapsed {
                self.elapsed = elapsed;
                cx.notify();
            }
        }
        while let Ok(event) = self.player.events.try_recv() {
            match event {
                Event::Finished(generation) if self.player.is_current(generation) => {
                    if let Some(next) = self
                        .playing
                        .map(|index| index + 1)
                        .filter(|index| *index < self.config.playlist.len())
                    {
                        self.play(next, cx);
                    } else {
                        self.playing = None;
                        self.paused = false;
                        self.elapsed = 0;
                        self.status = "Playback finished".into();
                    }
                    cx.notify();
                }
                Event::Error(generation, error) if self.player.is_current(generation) => {
                    self.playing = None;
                    self.paused = false;
                    self.elapsed = 0;
                    self.status = "Stopped".into();
                    self.pending_errors
                        .push(error_notification("Playback error", error));
                    cx.notify();
                }
                _ => {}
            }
        }
        for error in std::mem::take(&mut self.pending_errors) {
            window.push_notification(error, cx);
        }
    }

    fn now_playing_title(&self) -> String {
        if self.playing.is_some() {
            self.playing_title.clone()
        } else {
            "No track playing".into()
        }
    }

    fn transport(&self, cx: &mut Context<Self>) -> Div {
        let active = self.playing.is_some();
        let play_label = if !active {
            "Play"
        } else if self.paused {
            "Resume"
        } else {
            "Pause"
        };
        let play_icon = if active && !self.paused {
            assets::IconName::Pause
        } else {
            assets::IconName::Play
        };
        let state = if !active {
            "Stopped"
        } else if self.paused {
            "Paused"
        } else {
            "Playing"
        };
        let accent = ButtonCustomVariant::new(cx)
            .color(rgb(ACCENT_COLOR).into())
            .foreground(rgb(0xffffff).into())
            .hover(rgb(0x269c94).into())
            .active(rgb(0x208b84).into());
        let previous = adjacent_track(self.playing, self.config.playlist.len(), false);
        let next = adjacent_track(self.playing, self.config.playlist.len(), true);
        let title = self.now_playing_title();
        let title_width = self
            .measured_title
            .as_ref()
            .filter(|(measured, _)| measured == &title)
            .map(|(_, width)| *width)
            .unwrap_or(px(0.));
        let distance = (title_width - self.title_viewport_width).as_f32().max(0.);
        let view = cx.weak_entity();
        let title_for_measure = title.clone();
        let title_element = div()
            .relative()
            .flex_shrink_0()
            .whitespace_nowrap()
            .child(title.clone())
            .on_prepaint(move |bounds, window, cx| {
                let width = bounds.size.width;
                let changed = view.upgrade().is_some_and(|view| {
                    view.read(cx)
                        .measured_title
                        .as_ref()
                        .is_none_or(|(text, old)| text != &title_for_measure || *old != width)
                });
                if changed {
                    window.defer(cx, move |_, cx| {
                        let _ = view.update(cx, |view, cx| {
                            if view.now_playing_title() == title_for_measure {
                                view.measured_title = Some((title_for_measure, width));
                                cx.notify();
                            }
                        });
                    });
                }
            });
        let title_element = if distance > 0. && self.title_viewport_width > px(0.) {
            let travel_seconds = distance / 30.;
            let duration = Duration::from_secs_f32(2. * travel_seconds + 2.);
            title_element
                .with_animation(
                    SharedString::from(format!("now-playing-{title}")),
                    Animation::new(duration).repeat(),
                    move |element, progress| {
                        let elapsed = progress * duration.as_secs_f32();
                        let offset = if elapsed < 1. {
                            0.
                        } else if elapsed < 1. + travel_seconds {
                            (elapsed - 1.) * 30.
                        } else if elapsed < 2. + travel_seconds {
                            distance
                        } else {
                            (distance - (elapsed - 2. - travel_seconds) * 30.).max(0.)
                        };
                        element.left(px(-offset))
                    },
                )
                .into_any_element()
        } else {
            title_element.into_any_element()
        };
        let view = cx.weak_entity();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(rgb(0x30414d))
            .bg(rgb(0x1b2933))
            .child(
                div()
                    .id("now-playing-title")
                    .relative()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .overflow_hidden()
                    .text_sm()
                    .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
                    .on_prepaint(move |bounds, window, cx| {
                        let width = bounds.size.width;
                        if view
                            .upgrade()
                            .is_some_and(|view| view.read(cx).title_viewport_width != width)
                        {
                            window.defer(cx, move |_, cx| {
                                let _ = view.update(cx, |view, cx| {
                                    if view.title_viewport_width != width {
                                        view.title_viewport_width = width;
                                        cx.notify();
                                    }
                                });
                            });
                        }
                    })
                    .child(title_element),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{}:{:02}", self.elapsed / 60, self.elapsed % 60)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(if active && !self.paused {
                                rgb(0x81d88a)
                            } else {
                                rgb(0xa6b6c3)
                            })
                            .child(state),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("transport-previous")
                            .icon(assets::IconName::SkipBack)
                            .tooltip("Previous track")
                            .accessibility_label("Previous track")
                            .disabled(previous.is_none())
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if let Some(index) = previous {
                                    view.play(index, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("transport-play")
                            .icon(play_icon)
                            .tooltip(play_label)
                            .accessibility_label(play_label)
                            .custom(accent)
                            .disabled(!active && self.config.playlist.is_empty())
                            .on_click(cx.listener(|view, _, _, cx| {
                                if view.playing.is_some() {
                                    view.paused = !view.paused;
                                    view.player.set_paused(view.paused);
                                    view.status = if view.paused {
                                        "Paused".into()
                                    } else {
                                        "Playing".into()
                                    };
                                    cx.notify();
                                } else if !view.config.playlist.is_empty() {
                                    view.play(0, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("transport-next")
                            .icon(assets::IconName::SkipForward)
                            .tooltip("Next track")
                            .accessibility_label("Next track")
                            .disabled(next.is_none())
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if let Some(index) = next {
                                    view.play(index, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("transport-stop")
                            .icon(assets::IconName::Square)
                            .tooltip("Stop")
                            .accessibility_label("Stop")
                            .custom(accent)
                            .disabled(!active)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.player.stop();
                                view.playing = None;
                                view.paused = false;
                                view.elapsed = 0;
                                view.status = "Stopped".into();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("transport-loop")
                            .icon(assets::IconName::Repeat2)
                            .tooltip(if self.looped { "Loop on" } else { "Loop off" })
                            .accessibility_label("Follow song loop points")
                            .toggled(self.looped)
                            .when(self.looped, |button| button.custom(accent))
                            .disabled(active)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.looped = !view.looped;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_1()
                            .min_w(px(95.))
                            .child(gpui_kit::component::Icon::from(assets::IconName::Volume2))
                            .child(
                                Slider::new(&self.volume)
                                    .accessibility_label("Playback volume")
                                    .bg(rgb(ACCENT_COLOR))
                                    .w_full(),
                            ),
                    ),
            )
    }

    fn directory_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        range
            .map(|index| {
                let entry = &self.directories[self.visible_directories[index]];
                let path = entry.path.clone();
                let selected = self.selected_directory.as_ref() == Some(&path);
                let expanded = self
                    .expanded_directories
                    .get(&entry.root)
                    .is_some_and(|paths| paths.contains(&path));
                let name = path
                    .file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .into_owned();
                let tooltip = path.display().to_string();
                let root = entry.root.clone();
                div()
                    .id(SharedString::from(format!("directory-row-{index}")))
                    .w_full()
                    .h(px(32.))
                    .pl(px(4. + (entry.depth.min(12) * 14) as f32))
                    .pr_1()
                    .flex()
                    .items_center()
                    .rounded_sm()
                    .when(selected, |row| row.bg(rgb(0x304755)))
                    .hover(|style| style.bg(rgb(0x293a47)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.selected_directory = Some(path.clone());
                        view.update_filter(cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .id(SharedString::from(format!("directory-expand-{index}")))
                            .w(px(20.))
                            .flex_shrink_0()
                            .text_sm()
                            .when(entry.has_children, |arrow| {
                                let path = entry.path.clone();
                                let root = entry.root.clone();
                                arrow
                                    .cursor_pointer()
                                    .child(if expanded { "▾" } else { "▸" })
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        cx.stop_propagation();
                                        let expanded = view
                                            .expanded_directories
                                            .entry(root.clone())
                                            .or_default();
                                        if !expanded.remove(&path) {
                                            expanded.insert(path.clone());
                                        }
                                        view.update_directory_tree();
                                        cx.notify();
                                    }))
                            }),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("directory-label-{index}")))
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_sm()
                            .child(name)
                            .tooltip(move |window, cx| {
                                Tooltip::new(tooltip.clone()).build(window, cx)
                            }),
                    )
                    .when(entry.depth == 0, |row| {
                        row.child(
                            Button::new(SharedString::from(format!("directory-remove-{index}")))
                                .label("×")
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    cx.stop_propagation();
                                    view.config
                                        .directories
                                        .retain(|directory| directory != &root);
                                    view.expanded_directories.remove(&root);
                                    view.directories.retain(|directory| directory.root != root);
                                    if view.selected_directory.as_ref().is_some_and(|selected| {
                                        !view
                                            .directories
                                            .iter()
                                            .any(|entry| &entry.path == selected)
                                    }) {
                                        view.selected_directory = None;
                                    }
                                    view.update_directory_tree();
                                    view.update_filter(cx);
                                    view.save();
                                    view.scan(cx);
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    }

    fn library_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        range
            .map(|index| {
                let track = &self.tracks[self.filtered_tracks[index]];
                let path = track.path.clone();
                let click_path = path.clone();
                let menu_path = path.clone();
                let selected = self.selected_tracks.contains(&path);
                let view = cx.weak_entity();
                let title = track.display_title();
                let artist = track.metadata.artist.as_deref().unwrap_or_default();
                let chip_list = track
                    .chips
                    .iter()
                    .map(|chip| chip.label())
                    .collect::<Vec<_>>()
                    .join(", ");
                div()
                    .id(SharedString::from(format!("track-row-{index}")))
                    .w_full()
                    .h(px(68.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .p_2()
                    .border_b_1()
                    .border_color(rgb(0x303b45))
                    .when(selected, |row| row.bg(rgb(0x304755)))
                    .hover(|style| style.bg(rgb(0x293a47)))
                    .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
                        if event.click_count() == 2 {
                            view.config.playlist.push(click_path.clone());
                            view.status = "Added to playlist".into();
                            view.save();
                        } else {
                            let modifiers = event.modifiers();
                            let toggle = modifiers.control || modifiers.platform;
                            let anchor = view
                                .track_anchor
                                .as_ref()
                                .filter(|_| modifiers.shift)
                                .and_then(|anchor| {
                                    view.filtered_tracks
                                        .iter()
                                        .position(|&track| &view.tracks[track].path == anchor)
                                });
                            if let Some(anchor) = anchor {
                                select_range(
                                    &mut view.selected_tracks,
                                    inclusive_range(anchor, index).map(|row| {
                                        view.tracks[view.filtered_tracks[row]].path.clone()
                                    }),
                                    toggle,
                                );
                            } else {
                                select_item(&mut view.selected_tracks, click_path.clone(), toggle);
                                view.track_anchor = Some(click_path.clone());
                            }
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .child(title),
                                    )
                                    .when(!track.chips.is_empty(), |row| {
                                        row.child(
                                            div()
                                                .id(SharedString::from(format!("chips-{index}")))
                                                .flex()
                                                .flex_shrink_0()
                                                .max_w(relative(0.65))
                                                .overflow_hidden()
                                                .items_center()
                                                .gap_1()
                                                .tooltip(move |window, cx| {
                                                    Tooltip::new(chip_list.clone())
                                                        .build(window, cx)
                                                })
                                                .children(track.chips.iter().map(|chip| {
                                                    div()
                                                        .flex_shrink_0()
                                                        .px_2()
                                                        .rounded_md()
                                                        .bg(if chip.ymfm {
                                                            rgb(ACCENT_COLOR)
                                                        } else {
                                                            rgb(0x71418e)
                                                        })
                                                        .text_color(if chip.ymfm {
                                                            rgb(0x102e2d)
                                                        } else {
                                                            rgb(0xffedff)
                                                        })
                                                        .text_xs()
                                                        .whitespace_nowrap()
                                                        .child(chip.label())
                                                })),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .text_color(rgb(0xa6b6c3))
                                    .child(artist.to_owned()),
                            ),
                    )
                    .context_menu(move |menu, _, cx| {
                        let _ = view.update(cx, |view, cx| {
                            if select_for_context(&mut view.selected_tracks, menu_path.clone()) {
                                view.track_anchor = Some(menu_path.clone());
                            }
                            cx.notify();
                        });
                        let view = view.clone();
                        menu.item(PopupMenuItem::new("Add selected to playlist").on_click(
                            move |_, _, cx| {
                                let _ = view.update(cx, |view, cx| view.add_selected_tracks(cx));
                            },
                        ))
                    })
                    .into_any_element()
            })
            .collect()
    }

    fn select_all_tracks(&mut self) {
        select_range(
            &mut self.selected_tracks,
            self.filtered_tracks
                .iter()
                .map(|&index| self.tracks[index].path.clone()),
            false,
        );
        if self.track_anchor.is_none() {
            self.track_anchor = self
                .filtered_tracks
                .first()
                .map(|&index| self.tracks[index].path.clone());
        }
    }

    fn select_all_queue(&mut self) {
        select_range(
            &mut self.selected_queue,
            0..self.config.playlist.len(),
            false,
        );
        if self.queue_anchor.is_none() && !self.config.playlist.is_empty() {
            self.queue_anchor = Some(0);
        }
    }

    fn queue_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        range
            .map(|index| {
                let path = &self.config.playlist[index];
                let title = self
                    .track_by_path
                    .get(path)
                    .map(|&track_index| self.tracks[track_index].display_title())
                    .or_else(|| self.playlist_titles.get(path).cloned())
                    .unwrap_or_else(|| {
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    });
                let marker = if self.playing == Some(index) {
                    "▶ "
                } else {
                    ""
                };
                let selected = self.selected_queue.contains(&index);
                let view = cx.weak_entity();
                div()
                    .id(SharedString::from(format!("queue-row-{index}")))
                    .w_full()
                    .h(px(48.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .p_2()
                    .when(selected, |row| row.bg(rgb(0x304755)))
                    .hover(|style| style.bg(rgb(0x293a47)))
                    .on_click(cx.listener(move |view, event: &ClickEvent, _, cx| {
                        if event.click_count() == 2 {
                            view.play(index, cx);
                        } else {
                            let modifiers = event.modifiers();
                            let toggle = modifiers.control || modifiers.platform;
                            if modifiers.shift
                                && let Some(anchor) = view.queue_anchor
                            {
                                select_range(
                                    &mut view.selected_queue,
                                    inclusive_range(anchor, index),
                                    toggle,
                                );
                            } else {
                                select_item(&mut view.selected_queue, index, toggle);
                                view.queue_anchor = Some(index);
                            }
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!("{marker}{title}")),
                    )
                    .context_menu(move |menu, _, cx| {
                        let _ = view.update(cx, |view, cx| {
                            if select_for_context(&mut view.selected_queue, index) {
                                view.queue_anchor = Some(index);
                            }
                            cx.notify();
                        });
                        let selected_view = view.clone();
                        let all_view = view.clone();
                        menu.item(PopupMenuItem::new("Remove selected").on_click(
                            move |_, _, cx| {
                                let _ = selected_view
                                    .update(cx, |view, cx| view.remove_queue_selection(false, cx));
                            },
                        ))
                        .separator()
                        .item(
                            PopupMenuItem::new("Clear playlist").on_click(move |_, _, cx| {
                                let _ = all_view
                                    .update(cx, |view, cx| view.remove_queue_selection(true, cx));
                            }),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    }
}

fn search_terms(text: &str, tokens: &[InlineTokenSpan]) -> (String, Vec<String>) {
    if tokens.is_empty() {
        return (text.to_owned(), Vec::new());
    }
    let mut plain = String::new();
    let mut chips = Vec::new();
    let mut start = 0;
    for token in tokens {
        let range = token.range();
        plain.push_str(&text[start..range.start]);
        plain.push(' ');
        chips.push(token.token().id().to_string());
        start = range.end;
    }
    plain.push_str(&text[start..]);
    (
        plain.split_whitespace().collect::<Vec<_>>().join(" "),
        chips,
    )
}

fn chip_suggestions(
    text: &str,
    cursor: usize,
    tokens: &[InlineTokenSpan],
    chips: &[&'static str],
) -> Option<(Range<usize>, Vec<&'static str>)> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    let mut start = 0;
    for token in tokens {
        let range = token.range();
        if range.start < cursor && cursor < range.end {
            return None;
        }
        if range.end <= cursor {
            start = range.end;
        }
    }
    let segment = &text[start..cursor];
    let starts = std::iter::once(0).chain(
        segment
            .char_indices()
            .filter_map(|(index, ch)| ch.is_whitespace().then_some(index + ch.len_utf8())),
    );
    for offset in starts {
        let fragment = &segment[offset..];
        if fragment.is_empty() || fragment.chars().last().is_some_and(char::is_whitespace) {
            continue;
        }
        let fragment = fragment.to_lowercase();
        let matches: Vec<_> = chips
            .iter()
            .copied()
            .filter(|chip| {
                chip.to_lowercase().starts_with(&fragment)
                    && !tokens
                        .iter()
                        .any(|token| token.token().id().as_ref() == *chip)
            })
            .take(8)
            .collect();
        if !matches.is_empty() {
            let end = text[cursor..]
                .char_indices()
                .find_map(|(index, ch)| ch.is_whitespace().then_some(cursor + index))
                .unwrap_or(text.len());
            let end = tokens
                .iter()
                .map(InlineTokenSpan::range)
                .filter(|range| range.start >= cursor)
                .map(|range| range.start)
                .fold(end, usize::min);
            return Some((start + offset..end, matches));
        }
    }
    None
}

fn select_item<T: Eq + Hash>(selection: &mut HashSet<T>, item: T, toggle: bool) {
    if toggle {
        if !selection.remove(&item) {
            selection.insert(item);
        }
    } else {
        selection.clear();
        selection.insert(item);
    }
}

fn select_for_context<T: Eq + Hash>(selection: &mut HashSet<T>, item: T) -> bool {
    if !selection.contains(&item) {
        select_item(selection, item, false);
        true
    } else {
        false
    }
}

fn inclusive_range(a: usize, b: usize) -> std::ops::RangeInclusive<usize> {
    a.min(b)..=a.max(b)
}

fn is_select_all(keystroke: &Keystroke) -> bool {
    let modifiers = keystroke.modifiers;
    keystroke.key == "a"
        && (modifiers.control || modifiers.platform)
        && !modifiers.alt
        && !modifiers.shift
        && !modifiers.function
}

fn select_range<T: Eq + Hash>(
    selection: &mut HashSet<T>,
    items: impl IntoIterator<Item = T>,
    additive: bool,
) {
    if !additive {
        selection.clear();
    }
    selection.extend(items);
}

fn remove_playlist_entries(
    playlist: &mut Vec<PathBuf>,
    removed: &HashSet<usize>,
    playing: Option<usize>,
) -> (Option<usize>, bool) {
    let stopped = playing.is_some_and(|index| removed.contains(&index));
    let playing = playing
        .filter(|_| !stopped)
        .map(|index| index - removed.iter().filter(|&&removed| removed < index).count());
    let mut index = 0;
    playlist.retain(|_| {
        let keep = !removed.contains(&index);
        index += 1;
        keep
    });
    (playing, stopped)
}

fn adjacent_track(current: Option<usize>, len: usize, forward: bool) -> Option<usize> {
    let current = current.filter(|&index| index < len)?;
    if forward {
        current.checked_add(1).filter(|&index| index < len)
    } else {
        current.checked_sub(1)
    }
}

fn scrollbar(id: &'static str, handle: &(impl ScrollbarHandle + Clone)) -> impl IntoElement {
    div()
        .id(id)
        .relative()
        .w(Scrollbar::width())
        .h_full()
        .flex_shrink_0()
        .child(
            Scrollbar::vertical(handle)
                .id(id)
                .mode(ScrollbarMode::Always)
                .viewport_from_layout(),
        )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use gpui_kit::Keystroke;
    use gpui_kit::component::input::{InlineToken, InputContent};

    use super::{
        adjacent_track, chip_suggestions, inclusive_range, is_select_all, remove_playlist_entries,
        search_terms, select_for_context, select_item, select_range,
    };

    #[test]
    fn chip_suggestions_match_at_the_cursor_and_skip_existing_tokens() {
        let chips = ["Sega PCM", "YM2151", "YM2612"];
        assert_eq!(
            chip_suggestions("game ym", 7, &[], &chips),
            Some((5..7, vec!["YM2151", "YM2612"]))
        );
        assert_eq!(
            chip_suggestions("game Sega P", 11, &[], &chips),
            Some((5..11, vec!["Sega PCM"]))
        );
        assert_eq!(
            chip_suggestions("ym2612", 2, &[], &chips),
            Some((0..6, vec!["YM2151", "YM2612"]))
        );
        let content = InputContent::new("YM2151ym")
            .with_token(0..6, InlineToken::new("YM2151", "YM2151"))
            .unwrap();
        assert_eq!(
            chip_suggestions(content.text(), 8, content.tokens(), &chips),
            Some((6..8, vec!["YM2612"]))
        );
        assert_eq!(
            chip_suggestions(content.text(), 3, content.tokens(), &chips),
            None
        );
        assert_eq!(chip_suggestions("音", 1, &[], &chips), None);
    }

    #[test]
    fn search_terms_separate_tokens_from_plain_text() {
        let content = InputContent::new("game YM2151YM2612 song")
            .with_token(5..11, InlineToken::new("YM2151", "YM2151"))
            .unwrap()
            .with_token(11..17, InlineToken::new("YM2612", "YM2612"))
            .unwrap();
        assert_eq!(
            search_terms(content.text(), content.tokens()),
            ("game song".into(), vec!["YM2151".into(), "YM2612".into()])
        );
        assert_eq!(
            search_terms("two  spaces", &[]),
            ("two  spaces".into(), vec![])
        );
    }

    #[test]
    fn selection_toggles_and_context_menu_preserves_selected_items() {
        let mut selected = HashSet::new();
        select_item(&mut selected, 1, false);
        select_item(&mut selected, 2, true);
        select_for_context(&mut selected, 1);
        assert_eq!(selected, HashSet::from([1, 2]));
        select_item(&mut selected, 1, true);
        assert_eq!(selected, HashSet::from([2]));
        select_for_context(&mut selected, 3);
        assert_eq!(selected, HashSet::from([3]));
        select_item(&mut selected, 4, false);
        assert_eq!(selected, HashSet::from([4]));
    }

    #[test]
    fn select_all_shortcut_accepts_control_and_command_without_other_modifiers() {
        for key in ["ctrl-a", "cmd-a"] {
            assert!(is_select_all(&Keystroke::parse(key).unwrap()));
        }
        for key in ["a", "ctrl-shift-a", "ctrl-alt-a", "ctrl-b"] {
            assert!(!is_select_all(&Keystroke::parse(key).unwrap()));
        }
    }

    #[test]
    fn select_all_replaces_selection_with_visible_tracks_or_all_playlist_indices() {
        let tracks = ["a", "hidden", "b", "c"];
        let visible = [3, 0, 2];
        let mut selected = HashSet::from(["hidden"]);
        select_range(&mut selected, visible.iter().map(|&row| tracks[row]), false);
        assert_eq!(selected, HashSet::from(["a", "b", "c"]));
        select_range(&mut selected, std::iter::empty(), false);
        assert!(selected.is_empty());

        let playlist = ["a", "b", "a"];
        let mut queue = HashSet::new();
        select_range(&mut queue, 0..playlist.len(), false);
        assert_eq!(queue, HashSet::from([0, 1, 2]));
    }

    #[test]
    fn range_selection_is_inclusive_reversible_and_optionally_additive() {
        let mut selected = HashSet::from([9]);
        select_range(&mut selected, inclusive_range(3, 1), false);
        assert_eq!(selected, HashSet::from([1, 2, 3]));
        select_range(&mut selected, inclusive_range(3, 4), false);
        assert_eq!(selected, HashSet::from([3, 4]));
        select_range(&mut selected, inclusive_range(0, 1), true);
        assert_eq!(selected, HashSet::from([0, 1, 3, 4]));
        select_range(&mut selected, inclusive_range(2, 2), false);
        assert_eq!(selected, HashSet::from([2]));
    }

    #[test]
    fn range_selection_uses_visible_order_and_duplicate_queue_indices() {
        let tracks = ["a", "hidden", "b", "c"];
        let visible = [3, 0, 2];
        let mut selected = HashSet::new();
        select_range(
            &mut selected,
            inclusive_range(0, 1).map(|row| tracks[visible[row]]),
            false,
        );
        assert_eq!(selected, HashSet::from(["c", "a"]));

        let playlist = ["a", "b", "a"];
        let mut rows = HashSet::new();
        select_range(&mut rows, inclusive_range(1, 2), false);
        assert_eq!(rows, HashSet::from([1, 2]));
        assert_eq!(rows.iter().filter(|&&row| playlist[row] == "a").count(), 1);
    }

    #[test]
    fn removal_distinguishes_duplicates_and_updates_playing_index() {
        let mut playlist: Vec<PathBuf> = ["a", "b", "a", "c", "d"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        let (playing, stopped) =
            remove_playlist_entries(&mut playlist, &HashSet::from([0, 3]), Some(2));
        assert_eq!(playlist, ["b", "a", "d"].map(PathBuf::from));
        assert_eq!(playing, Some(1));
        assert!(!stopped);

        let (playing, stopped) =
            remove_playlist_entries(&mut playlist, &HashSet::from([0, 1]), playing);
        assert_eq!(playlist, [PathBuf::from("d")]);
        assert_eq!(playing, None);
        assert!(stopped);

        let (playing, stopped) = remove_playlist_entries(&mut playlist, &HashSet::from([0]), None);
        assert!(playlist.is_empty());
        assert_eq!(playing, None);
        assert!(!stopped);
    }

    #[test]
    fn adjacent_track_does_not_wrap_or_navigate_when_stopped() {
        assert_eq!(adjacent_track(None, 3, true), None);
        assert_eq!(adjacent_track(Some(0), 3, false), None);
        assert_eq!(adjacent_track(Some(0), 3, true), Some(1));
        assert_eq!(adjacent_track(Some(1), 3, false), Some(0));
        assert_eq!(adjacent_track(Some(1), 3, true), Some(2));
        assert_eq!(adjacent_track(Some(2), 3, true), None);
        assert_eq!(adjacent_track(Some(3), 3, false), None);
        assert_eq!(adjacent_track(Some(0), 0, true), None);
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_title_bar = !cfg!(target_os = "linux")
            || matches!(window.window_decorations(), Decorations::Client { .. });
        let playlist_width = self.config.playlist_width.unwrap_or_else(|| {
            ((window.viewport_size().width.as_f32() - 52. - self.config.library_width) / 2.)
                .clamp(library::MIN_PANEL_WIDTH, library::MAX_PANEL_WIDTH)
        });
        let keep_side_widths = window.viewport_size().width.as_f32()
            >= 52. + 220. + self.config.library_width + playlist_width;
        if self
            .keep_side_widths
            .is_some_and(|previous| previous != keep_side_widths)
        {
            // Reapply saved sizes when switching between constrained and fixed layouts.
            self.panel_state.update(cx, |state, _| state.clear());
        }
        self.keep_side_widths = Some(keep_side_widths);
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x111820))
            .text_color(rgb(0xe4edf2))
            .when(show_title_bar, |view| {
                view.child(
                    TitleBar::new()
                        .bg(rgb(0x1b2530))
                        .border_color(rgb(0x243441))
                        .text_color(rgb(0xe4edf2))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("nddrv"),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(52.))
                            .h_full()
                            .bg(rgb(0x1b2530))
                            .p_2()
                            .child(Button::new("library-icon").icon(assets::IconName::FolderOpen)),
                    )
                    .child(
                        div().flex_1().min_w_0().h_full().child(
                            h_resizable("main-panels")
                                .with_state(&self.panel_state)
                                .on_resize({
                                    let view = cx.weak_entity();
                                    move |state, _, cx| {
                                        let sizes = state.read(cx).sizes();
                                        if let [library, _, playlist] = sizes.as_slice() {
                                            let (library, playlist) = (
                                                library.as_f32().clamp(
                                                    library::MIN_PANEL_WIDTH,
                                                    library::MAX_PANEL_WIDTH,
                                                ),
                                                playlist.as_f32().clamp(
                                                    library::MIN_PANEL_WIDTH,
                                                    library::MAX_PANEL_WIDTH,
                                                ),
                                            );
                                            let _ = view.update(cx, |view, cx| {
                                                view.config.library_width = library;
                                                view.config.playlist_width = Some(playlist);
                                                view.save();
                                                cx.notify();
                                            });
                                        }
                                    }
                                })
                                .child(
                                    resizable_panel()
                                        .size(px(self.config.library_width))
                                        .when(keep_side_widths, |panel| panel.fixed())
                                        .size_range(
                                            px(library::MIN_PANEL_WIDTH)
                                                ..px(library::MAX_PANEL_WIDTH),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .w_full()
                                                .h_full()
                                                .p_3()
                                                .gap_2()
                                                .bg(rgb(0x17212b))
                                                .child(div().text_lg().child("LIBRARY"))
                                                .child(
                                                    div()
                                                        .id("all-tracks")
                                                        .px_2()
                                                        .py_1()
                                                        .rounded_sm()
                                                        .text_sm()
                                                        .when(
                                                            self.selected_directory.is_none(),
                                                            |row| row.bg(rgb(0x304755)),
                                                        )
                                                        .hover(|style| style.bg(rgb(0x293a47)))
                                                        .child("All tracks")
                                                        .on_click(cx.listener(|view, _, _, cx| {
                                                            view.selected_directory = None;
                                                            view.update_filter(cx);
                                                            cx.notify();
                                                        })),
                                                )
                                                .child(
                                                    div()
                                                        .flex()
                                                        .flex_1()
                                                        .min_h_0()
                                                        .child(
                                                            uniform_list(
                                                                "directories",
                                                                self.visible_directories.len(),
                                                                cx.processor(
                                                                    |view, range, _, cx| {
                                                                        view.directory_rows(
                                                                            range, cx,
                                                                        )
                                                                    },
                                                                ),
                                                            )
                                                            .flex_1()
                                                            .min_w_0()
                                                            .h_full()
                                                            .track_scroll(&self.directory_scroll),
                                                        )
                                                        .child(scrollbar(
                                                            "directory-scrollbar",
                                                            &self.directory_scroll,
                                                        )),
                                                )
                                                .child(
                                                    Button::new("add-folder")
                                                        .label("+ Add directory")
                                                        .on_click(cx.listener(|view, _, _, cx| {
                                                            view.add_directory(cx)
                                                        })),
                                                )
                                                .child(
                                                    Button::new("rescan")
                                                        .label(if self.scanning {
                                                            "Scanning..."
                                                        } else {
                                                            "Rescan"
                                                        })
                                                        .on_click(cx.listener(|view, _, _, cx| {
                                                            view.scan(cx)
                                                        })),
                                                ),
                                        ),
                                )
                                .child(
                                    resizable_panel().size_range(px(220.)..Pixels::MAX).child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .flex_1()
                                            .min_w_0()
                                            .h_full()
                                            .p_3()
                                            .gap_2()
                                            .child(div().text_lg().child("MUSIC"))
                                            .child(self.search_input(window, cx))
                                            .child(
                                                div()
                                                    .id("music-list-container")
                                                    .track_focus(&self.track_focus)
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(|view, _, window, cx| {
                                                            view.track_focus.focus(window, cx);
                                                        }),
                                                    )
                                                    .on_key_down(cx.listener(
                                                        |view, event: &KeyDownEvent, _, cx| {
                                                            if is_select_all(&event.keystroke) {
                                                                view.select_all_tracks();
                                                                cx.stop_propagation();
                                                                cx.notify();
                                                            }
                                                        },
                                                    ))
                                                    .flex()
                                                    .flex_1()
                                                    .min_h_0()
                                                    .child(
                                                        uniform_list(
                                                            "track-list",
                                                            self.filtered_tracks.len(),
                                                            cx.processor(|view, range, _, cx| {
                                                                view.library_rows(range, cx)
                                                            }),
                                                        )
                                                        .track_scroll(&self.library_scroll)
                                                        .flex_1()
                                                        .min_w_0()
                                                        .h_full(),
                                                    )
                                                    .child(scrollbar(
                                                        "library-scrollbar",
                                                        &self.library_scroll,
                                                    )),
                                            ),
                                    ),
                                )
                                .child(
                                    resizable_panel()
                                        .size(px(playlist_width))
                                        .when(keep_side_widths, |panel| panel.fixed())
                                        .size_range(
                                            px(library::MIN_PANEL_WIDTH)
                                                ..px(library::MAX_PANEL_WIDTH),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .w_full()
                                                .h_full()
                                                .p_3()
                                                .gap_2()
                                                .bg(rgb(0x17212b))
                                                .child(div().text_lg().child("PLAYLIST"))
                                                .child(self.transport(cx))
                                                .child(
                                                    div()
                                                        .id("playlist-list-container")
                                                        .track_focus(&self.queue_focus)
                                                        .on_mouse_down(
                                                            MouseButton::Left,
                                                            cx.listener(|view, _, window, cx| {
                                                                view.queue_focus.focus(window, cx);
                                                            }),
                                                        )
                                                        .on_key_down(cx.listener(
                                                            |view, event: &KeyDownEvent, _, cx| {
                                                                if is_select_all(&event.keystroke) {
                                                                    view.select_all_queue();
                                                                    cx.stop_propagation();
                                                                    cx.notify();
                                                                }
                                                            },
                                                        ))
                                                        .flex()
                                                        .flex_1()
                                                        .min_h_0()
                                                        .child(
                                                            uniform_list(
                                                                "queue-list",
                                                                self.config.playlist.len(),
                                                                cx.processor(
                                                                    |view, range, _, cx| {
                                                                        view.queue_rows(range, cx)
                                                                    },
                                                                ),
                                                            )
                                                            .track_scroll(&self.queue_scroll)
                                                            .flex_1()
                                                            .min_w_0()
                                                            .h_full(),
                                                        )
                                                        .child(scrollbar(
                                                            "queue-scrollbar",
                                                            &self.queue_scroll,
                                                        )),
                                                ),
                                        ),
                                ),
                        ),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .bg(rgb(0x243441))
                    .text_sm()
                    .child(self.status.clone()),
            )
    }
}

fn main() {
    application().with_assets(assets::AllAssets).run(|cx| {
        init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        Theme::global_mut(cx).notification.placement = Anchor::BottomRight;
        let options = WindowOptions {
            window_decorations: Some(WindowDecorations::Client),
            ..TitleBar::window_options()
        };
        open_window(options, cx, |window, cx| {
            window.set_window_title("nddrv");
            cx.new(|cx| {
                let mut app = AppView::new(window, cx);
                if !app.config.directories.is_empty() {
                    app.scan(cx);
                } else {
                    app.load_playlist_titles(cx);
                }
                app
            })
        })
        .expect("failed to open nddrv window");
    });
}
