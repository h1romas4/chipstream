mod device;
mod hardware;
mod library;
mod patchbay;
mod playback;
mod routing;

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chipstream::metadata::TrackMetadata;
use gpui_kit::component::ElementExt;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::input::{
    InlineToken, InlineTokenSpan, Input, InputEvent, InputState, InputToken,
};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::scroll::{ScrollableElement, Scrollbar, ScrollbarHandle, ScrollbarMode};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, IndexPath, Theme, ThemeMode, TitleBar, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use library::{ChipTag, Config, DirectoryEntry, ScanProgress, Track};
use playback::{Event, Playback, VolumeMode};

const ACCENT_COLOR: u32 = 0x2fb3aa;
const APP_TITLE: &str = "NDDRV";

fn chip_colors(ymfm: bool) -> (u32, u32) {
    if ymfm {
        (ACCENT_COLOR, 0x102e2d)
    } else {
        (0x71418e, 0xffedff)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Library,
    Devices,
}

enum DeviceStatus {
    Connecting,
    Connected(Vec<ndsif::CapabilityEntry>),
    Disconnecting,
}

#[derive(Clone)]
struct ProfileChoice {
    id: Option<u64>,
    name: String,
}

impl SelectItem for ProfileChoice {
    type Value = Option<u64>;

    fn title(&self) -> SharedString {
        self.name.clone().into()
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

fn profile_choices(config: &routing::RoutingConfig) -> Vec<ProfileChoice> {
    std::iter::once(ProfileChoice {
        id: None,
        name: "Emulation (chipstream)".into(),
    })
    .chain(config.profiles.iter().map(|profile| ProfileChoice {
        id: Some(profile.id),
        name: profile.name.clone(),
    }))
    .collect()
}

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
    page: Page,
    device_service: device::DeviceService,
    device_status: HashMap<u64, DeviceStatus>,
    device_port: Entity<SelectState<SearchableVec<String>>>,
    scanning_ports: bool,
    profile_select: Entity<SelectState<SearchableVec<ProfileChoice>>>,
    profile_name: Entity<InputState>,
    selected_source: Option<(routing::HardwareChip, routing::ChipInstance)>,
    _profile_subscription: Subscription,
    tracks: Vec<Track>,
    track_by_path: HashMap<PathBuf, usize>,
    filtered_tracks: Vec<usize>,
    selected_tracks: HashSet<PathBuf>,
    selected_queue: HashSet<usize>,
    track_anchor: Option<PathBuf>,
    track_selection_order: Vec<PathBuf>,
    queue_anchor: Option<usize>,
    track_focus: FocusHandle,
    queue_focus: FocusHandle,
    playlist_metadata: HashMap<PathBuf, TrackMetadata>,
    details_path: Option<PathBuf>,
    details_artwork: Option<PathBuf>,
    details_artwork_loading: bool,
    details_request: u64,
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
    playing_hardware: bool,
    displayed_volume_mode: VolumeMode,
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
        let device_port = cx.new(|cx| {
            SelectState::new(SearchableVec::new(Vec::<String>::new()), None, window, cx)
                .searchable(true)
        });
        let volume_subscription = cx.subscribe(&volume, |view: &mut Self, _, event, cx| {
            if let SliderEvent::Change(SliderValue::Single(value)) = event {
                if let Err(error) = view.player.set_volume(*value / 100.) {
                    view.pending_errors
                        .push(error_notification("Volume error", error.to_string()));
                }
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
        let choices = profile_choices(&config.routing);
        let profile_index = choices
            .iter()
            .position(|choice| choice.id == config.routing.selected_profile);
        let profile_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(choices),
                profile_index.map(IndexPath::new),
                window,
                cx,
            )
        });
        let profile_subscription =
            cx.subscribe(&profile_select, |view: &mut Self, _, event, cx| {
                if let SelectEvent::Confirm(Some(id)) = event
                    && view.config.routing.selected_profile != *id
                {
                    view.config.routing.selected_profile = *id;
                    view.selected_source = None;
                    view.save();
                    cx.notify();
                }
            });
        let profile_name = cx.new(|cx| InputState::new(window, cx).placeholder("New profile name"));
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
            page: Page::Library,
            device_service: device::DeviceService::new(),
            device_status: HashMap::new(),
            device_port,
            scanning_ports: false,
            profile_select,
            profile_name,
            selected_source: None,
            _profile_subscription: profile_subscription,
            tracks: Vec::new(),
            track_by_path: HashMap::new(),
            filtered_tracks: Vec::new(),
            selected_tracks: HashSet::new(),
            selected_queue: HashSet::new(),
            track_anchor: None,
            track_selection_order: Vec::new(),
            queue_anchor: None,
            track_focus: cx.focus_handle(),
            queue_focus: cx.focus_handle(),
            playlist_metadata: HashMap::new(),
            details_path: None,
            details_artwork: None,
            details_artwork_loading: false,
            details_request: 0,
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
            playing_hardware: false,
            displayed_volume_mode: VolumeMode::Master,
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
            self.track_selection_order
                .retain(|path| self.selected_tracks.contains(path));
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
                let ymfm = ChipTag::is_ymfm_name(token.token().id().as_ref());
                let (background, foreground) = chip_colors(ymfm);
                div()
                    .max_w(token.available_width())
                    .pr(px(4.))
                    .child(InputToken::new(token).when(!token.is_selected(), |tag| {
                        tag.bg(rgb(background))
                            .text_color(rgb(foreground))
                            .border_color(rgb(if ymfm { background } else { 0xb77ad4 }))
                    }))
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
        self.playlist_metadata
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

    fn load_playlist_metadata(&mut self, cx: &mut Context<Self>) {
        self.playlist_metadata.clear();
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
                            let metadata = library::read_metadata(&path);
                            (path, metadata)
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
                        Ok(metadata) => {
                            view.playlist_metadata.insert(path, metadata);
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
                view.load_playlist_metadata(cx);
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
        let plan = match self.config.routing.playback_plan() {
            Ok(plan) => plan,
            Err(error) => {
                self.pending_errors.push(error_notification(
                    "Routing configuration error",
                    format!("{error:#}"),
                ));
                cx.notify();
                return;
            }
        };
        if let Some(path) = self.config.playlist.get(index).cloned() {
            self.status = format!("Playing: {}", path.display());
            self.playing = Some(index);
            self.playing_hardware = plan.is_some();
            self.paused = false;
            self.elapsed = 0;
            let indexed = self.track_by_path.get(&path).copied();
            self.playing_title = indexed
                .map(|index| self.tracks[index].display_title())
                .unwrap_or_else(|| Track::format_display_title(&path, &Default::default()));
            if let Some(plan) = plan {
                self.player.play_hardware(
                    path.clone(),
                    self.looped,
                    self.device_service.playback_handle(),
                    plan,
                );
            } else {
                self.player.play(path.clone(), self.looped);
            }
            self.displayed_volume_mode = self.player.volume_mode();
            if indexed.is_none() {
                let generation = self.player.generation();
                cx.spawn(async move |this, cx| {
                    let load_path = path.clone();
                    let result = cx
                        .background_spawn(async move { library::read_metadata(&load_path) })
                        .await;
                    let _ = this.update(cx, |view, cx| {
                        if view.playing.is_some() && view.player.is_current(generation) {
                            match result {
                                Ok(metadata) => {
                                    view.playing_title =
                                        Track::format_display_title(&path, &metadata);
                                    view.playlist_metadata.insert(path.clone(), metadata);
                                }
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
        while let Ok(event) = self.device_service.events.try_recv() {
            match event {
                device::DeviceEvent::Connected(id, model, capabilities) => {
                    let name = unique_device_name(&model, id, &self.config.routing.devices);
                    if let Some(device) = self
                        .config
                        .routing
                        .devices
                        .iter_mut()
                        .find(|device| device.id == id)
                    {
                        device.name = name;
                        device.baud_rate = device::BAUD_RATE;
                        self.save();
                    }
                    self.device_status
                        .insert(id, DeviceStatus::Connected(capabilities));
                }
                device::DeviceEvent::Disconnected(id) => {
                    self.device_status.remove(&id);
                }
                device::DeviceEvent::Error(id, error) => {
                    self.device_status.remove(&id);
                    let name = self
                        .config
                        .routing
                        .devices
                        .iter()
                        .find(|device| device.id == id)
                        .map_or_else(
                            || format!("Device {id}"),
                            |device| {
                                if device.name.is_empty() {
                                    device.port.clone()
                                } else {
                                    device.name.clone()
                                }
                            },
                        );
                    self.pending_errors.push(error_notification(
                        "NanoDrive connection error",
                        format!("{name}: {error}"),
                    ));
                }
                device::DeviceEvent::Busy(id, error, capabilities) => {
                    self.device_status
                        .insert(id, DeviceStatus::Connected(capabilities));
                    self.pending_errors
                        .push(error_notification("NanoDrive connection error", error));
                }
                device::DeviceEvent::Ports(result) => {
                    self.scanning_ports = false;
                    match result {
                        Ok(ports) => self.device_port.update(cx, |state, cx| {
                            state.set_items(SearchableVec::new(ports), window, cx);
                        }),
                        Err(error) => self
                            .pending_errors
                            .push(error_notification("Serial port scan error", error)),
                    }
                }
            }
            cx.notify();
        }
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
            let mode = self.player.volume_mode();
            if mode != self.displayed_volume_mode {
                self.displayed_volume_mode = mode;
                cx.notify();
            }
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
        self.refresh_details(cx);
        for error in std::mem::take(&mut self.pending_errors) {
            window.push_notification(error, cx);
        }
    }

    fn refresh_details(&mut self, cx: &mut Context<Self>) {
        let target = details_target(
            self.playing,
            &self.config.playlist,
            &self.track_selection_order,
        )
        .cloned();
        if target == self.details_path {
            return;
        }
        self.details_request = self.details_request.wrapping_add(1);
        let request = self.details_request;
        self.details_path = target.clone();
        self.details_artwork = None;
        self.details_artwork_loading = target.is_some();
        cx.notify();
        let Some(path) = target else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let search_path = path.clone();
            let result = cx
                .background_spawn(async move { library::find_artwork(&search_path) })
                .await;
            let _ = this.update(cx, |view, cx| {
                if view.details_request != request || view.details_path.as_ref() != Some(&path) {
                    return;
                }
                view.details_artwork_loading = false;
                match result {
                    Ok(artwork) => view.details_artwork = artwork,
                    Err(error) => view.pending_errors.push(error_notification(
                        "Artwork error",
                        format!("{}: {error:#}", path.display()),
                    )),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn track_details(&self) -> impl IntoElement {
        let path = self.details_path.as_ref();
        let metadata = path.and_then(|path| {
            self.track_by_path
                .get(path)
                .map(|&index| &self.tracks[index].metadata)
                .or_else(|| self.playlist_metadata.get(path))
        });
        let title = path.map(|path| {
            metadata
                .and_then(|metadata| metadata.title.clone())
                .unwrap_or_else(|| {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
        });
        let mut fields = Vec::new();
        if let Some(title) = title {
            fields.push(("Track", title));
        }
        if let Some(metadata) = metadata {
            for (name, value) in [
                ("Game", &metadata.album),
                ("Composer", &metadata.artist),
                ("Notes", &metadata.comment),
            ] {
                if let Some(value) = value.as_ref().filter(|value| !value.trim().is_empty()) {
                    fields.push((name, value.clone()));
                }
            }
        }
        let artwork = self.details_artwork.clone();
        let loading = self.details_artwork_loading;
        div()
            .flex()
            .flex_col()
            .h(relative(0.28))
            .min_h(px(80.))
            .max_h(px(190.))
            .flex_shrink_0()
            .min_w_0()
            .gap_1()
            .border_t_1()
            .border_color(rgb(0x303b45))
            .pt_2()
            .child(div().text_sm().child("TRACK INFO"))
            .child(
                div()
                    .id("track-details")
                    .overflow_y_scrollbar()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex()
                            .gap(px(16.))
                            .min_w_0()
                            .when(path.is_some(), |row| {
                                row.child(
                                    div()
                                        .w(px(96.))
                                        .h(px(96.))
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .bg(rgb(0x243441))
                                        .when_some(artwork, |node, artwork| {
                                            node.child(
                                                img(artwork).w_full().h_full().with_fallback(
                                                    || {
                                                        div()
                                                            .text_xs()
                                                            .child("Image error")
                                                            .into_any_element()
                                                    },
                                                ),
                                            )
                                        })
                                        .when(self.details_artwork.is_none(), |node| {
                                            node.child(
                                                div().text_xs().text_color(rgb(0xa6b6c3)).child(
                                                    if loading { "Loading..." } else { "No image" },
                                                ),
                                            )
                                        }),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .when(path.is_none(), |node| {
                                        node.child(
                                            div()
                                                .text_sm()
                                                .text_color(rgb(0xa6b6c3))
                                                .child("Select a track"),
                                        )
                                    })
                                    .children(fields.into_iter().map(|(name, value)| {
                                        div()
                                            .flex()
                                            .flex_col()
                                            .min_w_0()
                                            .text_sm()
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(0xa6b6c3))
                                                    .child(name),
                                            )
                                            .child(div().whitespace_normal().child(value))
                                    })),
                            ),
                    ),
            )
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
        let volume_mode = if active {
            self.displayed_volume_mode
        } else {
            VolumeMode::Master
        };
        let volume_label = match volume_mode {
            VolumeMode::Master => "Master volume",
            VolumeMode::SoftwareOnly => "Software volume (NanoDrive fixed)",
            VolumeMode::Preparing => "Checking NanoDrive volume",
        };
        let play_label = if active && self.playing_hardware {
            "Pause unavailable during NanoDrive playback"
        } else if !active {
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
                            .disabled(
                                (!active && self.config.playlist.is_empty())
                                    || (active && self.playing_hardware),
                            )
                            .on_click(cx.listener(|view, _, _, cx| {
                                if view.playing.is_some() && !view.playing_hardware {
                                    view.paused = !view.paused;
                                    view.player.set_paused(view.paused);
                                    view.status = if view.paused {
                                        "Paused".into()
                                    } else {
                                        "Playing".into()
                                    };
                                    cx.notify();
                                } else if view.playing.is_none() && !view.config.playlist.is_empty()
                                {
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
                            .tooltip(if self.looped {
                                "Loop indefinitely"
                            } else {
                                "Play twice, then fade out for 2 seconds"
                            })
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
                            .child(div().text_xs().child(match volume_mode {
                                VolumeMode::Master => "Master",
                                VolumeMode::SoftwareOnly => "Software (ND fixed)",
                                VolumeMode::Preparing => "Checking ND",
                            }))
                            .child(
                                Slider::new(&self.volume)
                                    .accessibility_label(volume_label)
                                    .bg(rgb(ACCENT_COLOR))
                                    .w_full(),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_sm().child("Routing"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Select::new(&self.profile_select).w_full().disabled(active)),
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

    fn refresh_ports(&mut self, cx: &mut Context<Self>) {
        if self.scanning_ports {
            return;
        }
        match self.device_service.scan_ports() {
            Ok(()) => self.scanning_ports = true,
            Err(error) => self.pending_errors.push(error_notification(
                "Serial port scan error",
                format!("{error:#}"),
            )),
        }
        cx.notify();
    }

    fn add_device(&mut self, cx: &mut Context<Self>) {
        let port = self.device_port.read(cx).selected_value().cloned();
        let result = (|| -> anyhow::Result<routing::Device> {
            let port = port.ok_or_else(|| anyhow::anyhow!("Select a serial port"))?;
            let id = self
                .config
                .routing
                .devices
                .iter()
                .map(|device| device.id)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("No more device IDs are available"))?;
            let device = routing::Device {
                id,
                name: String::new(),
                port,
                baud_rate: device::BAUD_RATE,
            };
            let mut routing = self.config.routing.clone();
            routing.devices.push(device.clone());
            routing.validate()?;
            Ok(device)
        })();
        match result {
            Ok(device) => {
                self.config.routing.devices.push(device);
                self.save();
            }
            Err(error) => self.pending_errors.push(error_notification(
                "Could not add NanoDrive",
                format!("{error:#}"),
            )),
        }
        cx.notify();
    }

    fn connect_device(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(device) = self
            .config
            .routing
            .devices
            .iter()
            .find(|device| device.id == id)
        else {
            self.pending_errors.push(error_notification(
                "NanoDrive error",
                "Device no longer exists",
            ));
            cx.notify();
            return;
        };
        let result = if matches!(
            self.device_status.get(&id),
            Some(DeviceStatus::Connected(_))
        ) {
            self.device_service
                .disconnect(id)
                .map(|_| DeviceStatus::Disconnecting)
        } else {
            self.device_service
                .connect(device.clone())
                .map(|_| DeviceStatus::Connecting)
        };
        match result {
            Ok(status) => {
                self.device_status.insert(id, status);
            }
            Err(error) => self.pending_errors.push(error_notification(
                "NanoDrive connection error",
                format!("{error:#}"),
            )),
        }
        cx.notify();
    }

    fn remove_device(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.device_status.contains_key(&id) {
            self.pending_errors.push(error_notification(
                "Cannot remove NanoDrive",
                "Disconnect the device first.",
            ));
        } else if self.config.routing.profiles.iter().any(|profile| {
            profile
                .connections
                .iter()
                .any(|route| route.device_id == id)
        }) {
            self.pending_errors.push(error_notification(
                "Cannot remove NanoDrive",
                "The device is used by a saved routing profile.",
            ));
        } else {
            self.config.routing.devices.retain(|device| device.id != id);
            self.save();
        }
        cx.notify();
    }

    fn sync_profiles(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let choices = profile_choices(&self.config.routing);
        let selected = self.config.routing.selected_profile;
        self.profile_select.update(cx, |state, cx| {
            state.set_items(SearchableVec::new(choices), window, cx);
            state.set_selected_value(&selected, window, cx);
        });
    }

    fn add_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.profile_name.read(cx).value();
        match self.config.routing.add_profile(&name) {
            Ok(_) => {
                self.selected_source = None;
                self.sync_profiles(window, cx);
                self.save();
            }
            Err(error) => self.pending_errors.push(error_notification(
                "Could not add routing profile",
                format!("{error:#}"),
            )),
        }
        cx.notify();
    }

    fn remove_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.config.routing.selected_profile.take() {
            self.config
                .routing
                .profiles
                .retain(|profile| profile.id != id);
            self.selected_source = None;
            self.sync_profiles(window, cx);
            self.save();
        }
        cx.notify();
    }

    fn assign_source(
        &mut self,
        target: Option<(u64, routing::ChipInstance)>,
        cx: &mut Context<Self>,
    ) {
        let Some((chip, source)) = self.selected_source else {
            self.pending_errors.push(error_notification(
                "Routing error",
                "Select a source first.",
            ));
            cx.notify();
            return;
        };
        if let Some((id, instance)) = target {
            let supported = matches!(self.device_status.get(&id),
                Some(DeviceStatus::Connected(entries)) if chip.advertised(instance, entries));
            if !supported {
                self.pending_errors.push(error_notification(
                    "Routing error",
                    "The device is disconnected or does not advertise this chip instance.",
                ));
                cx.notify();
                return;
            }
        }
        match self.config.routing.set_route(chip, source, target) {
            Ok(()) => self.save(),
            Err(error) => self
                .pending_errors
                .push(error_notification("Routing error", format!("{error:#}"))),
        }
        cx.notify();
    }

    fn profile_view(&self, cx: &mut Context<Self>) -> Div {
        let editable = self.config.routing.selected_profile.is_some() && self.playing.is_none();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_lg().child("ROUTING PROFILES"))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .items_center()
                    .child(
                        div().w(px(220.)).child(
                            Select::new(&self.profile_select)
                                .w_full()
                                .disabled(self.playing.is_some()),
                        ),
                    )
                    .child(div().w(px(180.)).child(Input::new(&self.profile_name)))
                    .child(
                        Button::new("add-profile")
                            .label("New profile")
                            .disabled(self.playing.is_some())
                            .on_click(
                                cx.listener(|view, _, window, cx| view.add_profile(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("delete-profile")
                            .label("Delete profile")
                            .disabled(!editable)
                            .on_click(
                                cx.listener(|view, _, window, cx| view.remove_profile(window, cx)),
                            ),
                    ),
            )
    }

    fn routing_view(&self, cx: &mut Context<Self>) -> Div {
        let selected_profile = self.config.routing.selected_profile.and_then(|id| {
            self.config
                .routing
                .profiles
                .iter()
                .find(|profile| profile.id == id)
        });
        let editable = selected_profile.is_some() && self.playing.is_none();
        let graph = patchbay::build(
            &self.config.routing.devices,
            &self.device_status,
            selected_profile,
        );
        let mut panels = vec![
            div()
                .absolute()
                .left(px(patchbay::SOURCE_X))
                .top(px(0.))
                .w(px(patchbay::SOURCE_WIDTH))
                .h(px(graph.emulator_height))
                .rounded_md()
                .border_1()
                .border_color(rgb(0x30414d))
                .bg(rgb(0x1b2933))
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("SEQUENCER"),
                )
                .into_any_element(),
            div()
                .absolute()
                .left(px(patchbay::TARGET_X))
                .top(px(0.))
                .w(px(patchbay::TARGET_WIDTH))
                .h(px(graph.emulator_height))
                .rounded_md()
                .border_1()
                .border_color(rgb(0x30414d))
                .bg(rgb(0x1b2933))
                .child(
                    div()
                        .px_3()
                        .py_2()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("chipstream"),
                )
                .into_any_element(),
        ];
        let mut nodes = Vec::new();
        for (index, source) in graph.sources.iter().copied().enumerate() {
            let y = patchbay::row_y(index);
            let hardware = match source {
                patchbay::Source::Hardware(chip, instance) => Some((chip, instance)),
                patchbay::Source::Software(_, _) => None,
            };
            let selected = hardware.is_some() && hardware == self.selected_source;
            let mut source_node = div()
                .id(SharedString::from(format!("source-{index}")))
                .absolute()
                .left(px(patchbay::SOURCE_X + 8.))
                .top(px(y))
                .w(px(patchbay::SOURCE_WIDTH - 4.))
                .h(px(patchbay::ROW))
                .flex()
                .items_center()
                .justify_between()
                .rounded_sm()
                .when(selected, |row| row.bg(rgb(0x304755)))
                .child(div().text_sm().child(source.label()))
                .child(div().size(px(8.)).rounded_full().bg(rgb(if selected {
                    0x7de0d6
                } else {
                    0x6a8e9c
                })));
            if editable && let Some((chip, instance)) = hardware {
                source_node = source_node
                    .cursor_pointer()
                    .hover(|row| row.bg(rgb(0x293a47)))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.selected_source = Some((chip, instance));
                        cx.notify();
                    }));
            }
            nodes.push(source_node.into_any_element());
            let mut target_node = div()
                .id(SharedString::from(format!("emulator-{index}")))
                .absolute()
                .left(px(patchbay::TARGET_X - 4.))
                .top(px(y))
                .w(px(patchbay::TARGET_WIDTH))
                .h(px(patchbay::ROW))
                .flex()
                .items_center()
                .gap_2()
                .rounded_sm()
                .child(div().size(px(8.)).rounded_full().bg(rgb(0x6a8e9c)))
                .child(div().text_sm().child(source.label()));
            if editable && selected {
                target_node = target_node
                    .bg(rgb(0x244047))
                    .cursor_pointer()
                    .hover(|row| row.bg(rgb(0x293a47)))
                    .on_click(cx.listener(|view, _, _, cx| view.assign_source(None, cx)));
            }
            nodes.push(target_node.into_any_element());
        }
        for device in &graph.devices {
            let title = format!(
                "{} · {}",
                device.name,
                if device.connected {
                    "Connected"
                } else {
                    "Unavailable"
                }
            );
            panels.push(
                div()
                    .absolute()
                    .left(px(patchbay::DEVICE_X))
                    .top(px(device.top))
                    .w(px(patchbay::DEVICE_WIDTH))
                    .h(px(device.height))
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x30414d))
                    .bg(rgb(0x1b2933))
                    .child(
                        div()
                            .px_3()
                            .py_2()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_ellipsis()
                            .child(title),
                    )
                    .when(device.endpoints.is_empty(), |panel| {
                        panel.child(
                            div()
                                .px_3()
                                .text_sm()
                                .text_color(rgb(0xa6b6c3))
                                .child("No available targets"),
                        )
                    })
                    .into_any_element(),
            );
            for endpoint in &device.endpoints {
                let id = device.id;
                let chip = endpoint.chip;
                let instance = endpoint.instance;
                let compatible = editable
                    && endpoint.available
                    && self
                        .selected_source
                        .is_some_and(|(source, _)| source == chip);
                let label = format!(
                    "{} #{}{}",
                    chip.name(),
                    instance.number() + 1,
                    if endpoint.available {
                        ""
                    } else {
                        " (unavailable)"
                    }
                );
                let mut node = div()
                    .id(SharedString::from(format!(
                        "target-{id}-{}-{}",
                        chip.name(),
                        instance.number()
                    )))
                    .absolute()
                    .left(px(patchbay::DEVICE_X - 4.))
                    .top(px(endpoint.y))
                    .w(px(patchbay::DEVICE_WIDTH))
                    .h(px(patchbay::ROW))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_sm()
                    .child(
                        div()
                            .size(px(8.))
                            .rounded_full()
                            .bg(rgb(if endpoint.available {
                                0x6a8e9c
                            } else {
                                0xf2a968
                            })),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(if endpoint.available {
                                0xe4edf2
                            } else {
                                0xf2a968
                            }))
                            .child(label),
                    );
                if compatible {
                    node = node
                        .bg(rgb(0x244047))
                        .cursor_pointer()
                        .hover(|row| row.bg(rgb(0x293a47)))
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.assign_source(Some((id, instance)), cx)
                        }));
                }
                nodes.push(node.into_any_element());
            }
        }
        let cables = graph.cables;
        div().flex().flex_col().gap_3()
            .child(div().text_lg().child("ROUTING CONNECTIONS"))
            .child(div().text_sm().text_color(rgb(0xa6b6c3)).child(
                "Select a sequencer source, then a matching chipstream or connected NanoDrive target. Dashed orange cables are unavailable. Hardware timing is best-effort."))
            .when(!editable, |view| view.child(div().text_sm().text_color(rgb(0xf2a968)).child(
                if self.playing.is_some() { "Stop playback to edit connections." } else { "Create or select a profile to edit connections." }
            )))
            .child(div().id("patchbay-scroll").w_full().h(px(graph.height)).overflow_x_scrollbar()
                .child(div().relative().w(px(patchbay::WIDTH)).h(px(graph.height))
                    .child(canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            for cable in &cables {
                                let start = point(bounds.origin.x + px(patchbay::SOURCE_X + patchbay::SOURCE_WIDTH),
                                    bounds.origin.y + px(cable.source_y));
                                let end = point(bounds.origin.x + px(cable.target_x),
                                    bounds.origin.y + px(cable.target_y));
                                let mut path = PathBuilder::stroke(px(if cable.hardware { 2.5 } else { 1.5 }));
                                if !cable.available { path = path.dash_array(&[px(6.), px(4.)]); }
                                path.move_to(start);
                                if cable.hardware {
                                    let under_panel = point(
                                        bounds.origin.x + px(patchbay::TARGET_X + 40.),
                                        start.y - px(patchbay::ROW / 2.),
                                    );
                                    path.cubic_bezier_to(under_panel, point(start.x + px(70.), start.y),
                                        point(under_panel.x - px(70.), under_panel.y));
                                    path.cubic_bezier_to(end, point(under_panel.x + px(100.), under_panel.y),
                                        point(end.x - px(70.), end.y));
                                } else {
                                    let control_offset = (end.x - start.x) * 0.45;
                                    path.cubic_bezier_to(end, point(start.x + control_offset, start.y),
                                        point(end.x - control_offset, end.y));
                                }
                                match path.build() {
                                    Ok(path) => window.paint_path(path, rgb(if !cable.available { 0xf2a968 }
                                        else if cable.hardware { ACCENT_COLOR } else { 0x466370 })),
                                    Err(error) => eprintln!("Failed to draw routing cable: {error}"),
                                }
                            }
                        }
                    ).absolute().top_0().left_0().w(px(patchbay::WIDTH)).h(px(graph.height)))
                    .children(panels)
                    .children(nodes)))
    }

    fn devices_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().id("device-list").size_full().overflow_y_scrollbar()
          .child(div()
            .flex().flex_col().p_4().gap_3()
            .child(self.profile_view(cx))
            .child(div().mt_2().text_lg().child("NANODRIVE DEVICES"))
            .child(div().text_sm().text_color(rgb(0xa6b6c3))
                .child("Connect devices to inspect their advertised chips. Hardware audio is output by the NanoDrive, not mixed into software audio."))
            .child(div().flex().gap_2().items_center()
                .child(div().w(px(280.)).flex_shrink_0().child(
                    Select::new(&self.device_port).placeholder("Select serial port").w_full()
                ))
                .child(Button::new("scan-ports").label(if self.scanning_ports { "Searching..." } else { "Search ports" })
                    .disabled(self.scanning_ports)
                    .on_click(cx.listener(|view, _, _, cx| view.refresh_ports(cx))))
                .child(Button::new("add-device").label("Add device").on_click(cx.listener(|view, _, _, cx| view.add_device(cx)))))
            .child(div().flex().flex_wrap().gap_3().children(
                        self.config.routing.devices.iter().map(|device| {
                            let id = device.id;
                            let status = self.device_status.get(&id);
                            let busy = matches!(status, Some(DeviceStatus::Connecting | DeviceStatus::Disconnecting));
                            let connected = matches!(status, Some(DeviceStatus::Connected(_)));
                            let status_text = match status {
                                None => "Disconnected",
                                Some(DeviceStatus::Connecting) => "Connecting...",
                                Some(DeviceStatus::Connected(_)) => "Connected",
                                Some(DeviceStatus::Disconnecting) => "Disconnecting...",
                            };
                            let capabilities = match status {
                                Some(DeviceStatus::Connected(entries)) if entries.is_empty() =>
                                    vec!["No chips advertised".into()],
                                Some(DeviceStatus::Connected(entries)) => capability_labels(entries),
                                _ => Vec::new(),
                            };
                            div().w(px(300.)).p_3().rounded_md().border_1()
                                .border_color(rgb(0x30414d)).bg(rgb(0x1b2933))
                                .flex().flex_col().gap_2()
                                .child(div().font_weight(FontWeight::SEMIBOLD).child(if device.name.is_empty() { "Unknown device".to_owned() } else { device.name.clone() }))
                                .child(div().text_sm().child(format!("{} · {} baud", device.port, device::BAUD_RATE)))
                                .child(div().text_sm().text_color(if connected { rgb(0x81d88a) } else { rgb(0xa6b6c3) }).child(status_text))
                                .when(connected, |node| node.child(
                                    div().flex().flex_col().gap_1().children(
                                        capabilities.into_iter().map(|label| div().text_sm().child(label))
                                    )
                                ))
                                .child(div().flex().gap_2()
                                    .child(Button::new(("connect-device", id as usize))
                                        .label(if connected { "Disconnect" } else { "Connect" })
                                        .disabled(busy)
                                        .on_click(cx.listener(move |view, _, _, cx| view.connect_device(id, cx))))
                                    .child(Button::new(("remove-device", id as usize))
                                        .label("Remove").disabled(status.is_some())
                                        .on_click(cx.listener(move |view, _, _, cx| view.remove_device(id, cx))))
                                )
                        })
                    ))
            .child(self.routing_view(cx)))
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
                            view.update_track_selection_order(Some(&click_path));
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
                                                    let (background, foreground) =
                                                        chip_colors(chip.ymfm);
                                                    div()
                                                        .flex_shrink_0()
                                                        .px_2()
                                                        .rounded_md()
                                                        .bg(rgb(background))
                                                        .text_color(rgb(foreground))
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
                            view.update_track_selection_order(Some(&menu_path));
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
        self.update_track_selection_order(None);
    }

    fn update_track_selection_order(&mut self, touched: Option<&PathBuf>) {
        update_selection_order(
            &mut self.track_selection_order,
            &self.selected_tracks,
            self.filtered_tracks
                .iter()
                .map(|&index| &self.tracks[index].path),
            touched,
        );
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
                let metadata = self
                    .track_by_path
                    .get(path)
                    .map(|&track_index| &self.tracks[track_index].metadata)
                    .or_else(|| self.playlist_metadata.get(path));
                let title = metadata
                    .map(|metadata| Track::format_display_title(path, metadata))
                    .unwrap_or_else(|| {
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    });
                let artist = metadata
                    .and_then(|metadata| metadata.artist.as_deref())
                    .unwrap_or_default()
                    .to_owned();
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
                    .h(px(68.))
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
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(format!("{marker}{title}")),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .text_color(rgb(0xa6b6c3))
                                    .child(artist),
                            ),
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

fn unique_device_name(model: &str, id: u64, devices: &[routing::Device]) -> String {
    let used = |name: &str| {
        devices
            .iter()
            .any(|device| device.id != id && device.name == name)
    };
    if let Some(device) = devices.iter().find(|device| device.id == id) {
        let same_model = device.name == model
            || device
                .name
                .strip_prefix(model)
                .and_then(|suffix| suffix.strip_prefix(" #"))
                .and_then(|number| number.parse::<u64>().ok())
                .is_some_and(|number| number >= 2);
        if same_model && !used(&device.name) {
            return device.name.clone();
        }
    }
    if !used(model) {
        return model.to_owned();
    }
    for number in 2u64.. {
        let name = format!("{model} #{number}");
        if !used(&name) {
            return name;
        }
    }
    unreachable!("a finite list of devices cannot exhaust all names")
}

fn details_target<'a>(
    playing: Option<usize>,
    playlist: &'a [PathBuf],
    selected: &'a [PathBuf],
) -> Option<&'a PathBuf> {
    playing
        .and_then(|index| playlist.get(index))
        .or_else(|| selected.last())
}

fn update_selection_order<'a>(
    order: &mut Vec<PathBuf>,
    selected: &HashSet<PathBuf>,
    visible: impl IntoIterator<Item = &'a PathBuf>,
    touched: Option<&PathBuf>,
) {
    order.retain(|path| selected.contains(path));
    let mut seen: HashSet<_> = order.iter().cloned().collect();
    if seen.len() != selected.len() {
        if selected.len() == seen.len() + 1
            && let Some(path) = touched.filter(|path| selected.contains(*path))
            && seen.insert(path.clone())
        {
            order.push(path.clone());
        } else {
            for path in visible {
                if selected.contains(path) && seen.insert(path.clone()) {
                    order.push(path.clone());
                }
            }
        }
    }
    if let Some(path) = touched.filter(|path| selected.contains(*path)) {
        order.retain(|entry| entry != path);
        order.push(path.clone());
    }
}

fn capability_labels(entries: &[ndsif::CapabilityEntry]) -> Vec<String> {
    let mut labels: Vec<_> = entries
        .iter()
        .map(|entry| {
            let name = match entry.write_id {
                0x30 | 0x50 => "SN76489",
                0x51 => "YM2413",
                0x52 => "YM2612",
                0x54 => "YM2151",
                0x55 => "YM2203",
                0x56 => "YM2608",
                0x58 => "YM2610/B",
                0x5a => "YM3812",
                0x5b => "YM3526",
                0x5c => "Y8950",
                0x5e => "YMF262",
                0xd0 => "YMF278B",
                0xb7 => "OKIM6258",
                other => {
                    return format!(
                        "Unknown target 0x{other:02x} #{}",
                        u16::from(entry.instance) + 1
                    );
                }
            };
            format!("{name} #{}", u16::from(entry.instance) + 1)
        })
        .collect();
    labels.sort();
    labels.dedup();
    labels
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
        ACCENT_COLOR, adjacent_track, capability_labels, chip_colors, chip_suggestions,
        details_target, inclusive_range, is_select_all, profile_choices, remove_playlist_entries,
        search_terms, select_for_context, select_item, select_range, unique_device_name,
        update_selection_order,
    };

    #[test]
    fn default_profile_uses_chipstream_emulation_label() {
        let choices = profile_choices(&crate::routing::RoutingConfig::default());
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].id, None);
        assert_eq!(choices[0].name, "Emulation (chipstream)");
    }

    #[test]
    fn track_details_follow_playback_then_last_selected_music_track() {
        let playlist = [PathBuf::from("first.vgm"), PathBuf::from("second.vgm")];
        let visible = [
            PathBuf::from("a.vgm"),
            PathBuf::from("b.vgm"),
            PathBuf::from("c.vgm"),
        ];
        let mut selected = HashSet::from([visible[0].clone(), visible[1].clone()]);
        let mut order = Vec::new();
        update_selection_order(&mut order, &selected, &visible, Some(&visible[0]));
        assert_eq!(details_target(None, &playlist, &order), Some(&visible[0]));
        assert_eq!(
            details_target(Some(1), &playlist, &order),
            Some(&playlist[1])
        );
        selected.remove(&visible[0]);
        update_selection_order(&mut order, &selected, &visible, Some(&visible[0]));
        assert_eq!(details_target(None, &playlist, &order), Some(&visible[1]));
        selected.insert(visible[2].clone());
        update_selection_order(&mut order, &selected, &visible, Some(&visible[2]));
        assert_eq!(details_target(None, &playlist, &order), Some(&visible[2]));
        selected.clear();
        update_selection_order(&mut order, &selected, &visible, None);
        assert_eq!(details_target(None, &playlist, &order), None);
        assert_eq!(
            details_target(Some(0), &playlist, &order),
            Some(&playlist[0])
        );
    }

    #[test]
    fn device_names_are_unique_and_stable_across_reconnections() {
        let device = |id, name: &str| crate::routing::Device {
            id,
            name: name.into(),
            port: format!("port{id}"),
            baud_rate: 115_200,
        };
        let devices = vec![
            device(1, "NanoDrive8"),
            device(2, "NanoDrive8 #2"),
            device(3, ""),
        ];
        assert_eq!(
            unique_device_name("NanoDrive8", 3, &devices),
            "NanoDrive8 #3"
        );
        assert_eq!(
            unique_device_name("NanoDrive8", 2, &devices),
            "NanoDrive8 #2"
        );
        assert_eq!(unique_device_name("NanoDrive6", 2, &devices), "NanoDrive6");
        assert_eq!(
            unique_device_name("NanoDrive8", 3, &devices[1..]),
            "NanoDrive8"
        );
    }

    #[test]
    fn capability_labels_deduplicate_pages_and_keep_unknown_targets() {
        let entry = ndsif::CapabilityEntry {
            write_id: 0x54,
            instance: 0,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Unknown,
            fixed_clock_hz: 0,
        };
        let entries = [
            entry,
            ndsif::CapabilityEntry { page: 1, ..entry },
            ndsif::CapabilityEntry {
                instance: 1,
                ..entry
            },
            ndsif::CapabilityEntry {
                write_id: 0xff,
                instance: 255,
                ..entry
            },
        ];
        assert_eq!(
            capability_labels(&entries),
            ["Unknown target 0xff #256", "YM2151 #1", "YM2151 #2"]
        );
    }

    #[test]
    fn chip_tag_colors_match_list_and_search() {
        assert_eq!(chip_colors(true), (ACCENT_COLOR, 0x102e2d));
        assert_eq!(chip_colors(false), (0x71418e, 0xffedff));
    }

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
                                .child(APP_TITLE),
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
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                Button::new("library-icon")
                                    .icon(assets::IconName::FolderOpen)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.page = Page::Library;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("devices-icon")
                                    .icon(assets::IconName::Cable)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.page = Page::Devices;
                                        view.refresh_ports(cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .when(self.page == Page::Library, |container| {
                                container.child(
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
                                                )
                                                .child(self.track_details()),
                                        ),
                                ),
                        )
                            })
                            .when(self.page == Page::Devices, |container| {
                                container.child(self.devices_view(cx))
                            }),
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
            window.set_window_title(APP_TITLE);
            cx.new(|cx| {
                let mut app = AppView::new(window, cx);
                if !app.config.directories.is_empty() {
                    app.scan(cx);
                } else {
                    app.load_playlist_metadata(cx);
                }
                app
            })
        })
        .expect("failed to open nddrv window");
    });
}
