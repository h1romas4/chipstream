mod artwork;
mod device;
mod fm_mute;
mod hardware;
mod library;
mod patchbay;
mod playback;
mod routing;
mod update;
mod visualizer;

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chipstream::metadata::TrackMetadata;
use chipstream::sequencer::Chip;
use gpui_kit::component::ElementExt;
use gpui_kit::component::Icon;
use gpui_kit::component::bubble::{Bubble, BubbleContent};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::input::{
    InlineToken, InlineTokenSpan, Input, InputEvent, InputState, InputToken,
};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::resizable::{ResizableState, h_resizable, resizable_panel};
use gpui_kit::component::scroll::{ScrollableElement, Scrollbar, ScrollbarHandle, ScrollbarMode};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, IndexPath, Sizable, Theme, ThemeMode, TitleBar, WindowExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use library::{ChipTag, Config, DirectoryEntry, ScanProgress, Track};
use playback::{Event, GainPart, GainSource, IdlePreview, Playback, VolumeMode};

const ACCENT_COLOR: u32 = 0x2fb3aa;
const APP_TITLE: &str = "NDDRV";
const INITIAL_WINDOW_SIZE: Size<Pixels> = size(px(1600.), px(900.));

fn initial_window_bounds(visible: Bounds<Pixels>) -> Bounds<Pixels> {
    let window_size = INITIAL_WINDOW_SIZE.min(&visible.size);
    let origin = point(
        visible.origin.x + (visible.size.width - window_size.width) / 2.,
        visible.origin.y + (visible.size.height - window_size.height) / 2.,
    );
    Bounds::new(origin, window_size)
}

fn clipboard_track_path(path: &Path) -> Result<&str, &'static str> {
    if !path.is_absolute() {
        return Err("The track path is not absolute.");
    }
    path.to_str()
        .ok_or("The track path cannot be represented as text.")
}

fn chip_colors(ymfm: bool) -> (u32, u32) {
    if ymfm {
        (ACCENT_COLOR, 0x102e2d)
    } else {
        (0x71418e, 0xffedff)
    }
}

fn chip_tag(chip: &ChipTag, dimmed: bool) -> Div {
    let (background, foreground) = if dimmed {
        if chip.ymfm {
            (0x244744, 0x9abdb8)
        } else {
            (0x403047, 0xc1adca)
        }
    } else {
        chip_colors(chip.ymfm)
    };
    div()
        .flex_shrink_0()
        .px_2()
        .rounded_md()
        .bg(rgb(background))
        .text_color(rgb(foreground))
        .text_xs()
        .whitespace_nowrap()
        .child(chip.label())
}

fn music_format(path: &Path) -> &'static str {
    library::track_format(path)
}

fn ends_folder_group(path: &Path, next: Option<&Path>) -> bool {
    next.is_some_and(|next| path.parent() != next.parent())
}

fn keep_preview_sources(
    playing: bool,
    transitioning: bool,
    current: Option<&[GainSource]>,
    playback: Option<&[GainSource]>,
) -> bool {
    playing && transitioning && current.is_some() && playback.is_none()
}

fn track_title(
    id: SharedString,
    path: &Path,
    metadata: &TrackMetadata,
    dimmed: bool,
    tooltip: String,
) -> Stateful<Div> {
    let (game, title) = Track::display_title_parts(path, metadata);
    div()
        .id(id)
        .flex()
        .items_center()
        .min_w_0()
        .overflow_hidden()
        .font_weight(FontWeight::SEMIBOLD)
        .when(dimmed, |label| label.text_color(rgb(0x82919b)))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .when_some(game, |row, game| {
            row.child(
                div()
                    .min_w_0()
                    .max_w(relative(1.0))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(game.to_owned()),
            )
            .child(div().flex_shrink_0().child(" / "))
        })
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(title),
        )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Library,
    Piano,
    Devices,
}

#[derive(Clone, Copy)]
enum DetailsSelection {
    Music,
    Playlist,
}

fn selection_status(selection: DetailsSelection, music: usize, playlist: usize) -> Option<String> {
    let (label, count) = match selection {
        DetailsSelection::Music => ("Music", music),
        DetailsSelection::Playlist => ("Playlist", playlist),
    };
    (count > 1).then(|| format!("{label}: {count} selected"))
}

enum DeviceStatus {
    Connecting,
    Connected(Vec<ndsif::CapabilityEntry>),
    Disconnecting,
}

fn connection_action(status: Option<&DeviceStatus>) -> (assets::IconName, &'static str) {
    match status {
        Some(DeviceStatus::Connecting) => (assets::IconName::Plug, "Connecting..."),
        Some(DeviceStatus::Connected(_)) => (assets::IconName::Unplug, "Disconnect"),
        Some(DeviceStatus::Disconnecting) => (assets::IconName::Unplug, "Disconnecting..."),
        None => (assets::IconName::Plug, "Connect"),
    }
}

fn can_remove_device(
    id: u64,
    devices: &[routing::Device],
    statuses: &HashMap<u64, DeviceStatus>,
) -> bool {
    devices.len() > 1
        && devices.last().is_some_and(|device| device.id != id)
        && devices.iter().any(|device| device.id == id)
        && !statuses.contains_key(&id)
}

#[derive(Clone, Copy)]
enum ScanOrigin {
    Background,
    Add,
    Rescan,
}

impl ScanOrigin {
    fn reports_errors(self) -> bool {
        matches!(self, Self::Add | Self::Rescan)
    }
}

fn reported_scan_errors(origin: ScanOrigin, errors: Vec<String>) -> Vec<String> {
    if origin.reports_errors() {
        errors
    } else {
        Vec::new()
    }
}

fn scan_progress_text(origin: ScanOrigin, progress: ScanProgress) -> String {
    if origin.reports_errors() {
        format!(
            "{} files scanned; {} errors",
            progress.files, progress.errors
        )
    } else {
        format!("{} files scanned", progress.files)
    }
}

struct DevicePort {
    select: Entity<SelectState<SearchableVec<String>>>,
    _subscription: Subscription,
}

fn device_port(
    id: u64,
    port: &str,
    ports: &[String],
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> DevicePort {
    let mut items = ports.to_vec();
    if !port.is_empty() && !items.iter().any(|item| item == port) {
        items.push(port.to_owned());
    }
    let selected = items.iter().position(|item| item == port);
    let select = cx.new(|cx| {
        SelectState::new(
            SearchableVec::new(items),
            selected.map(IndexPath::new),
            window,
            cx,
        )
        .searchable(true)
    });
    let subscription = cx.subscribe_in(
        &select,
        window,
        move |view: &mut AppView, select, event, window, cx| {
            if let SelectEvent::Confirm(Some(port)) = event {
                let previous = view
                    .config
                    .routing
                    .devices
                    .iter()
                    .find(|device| device.id == id)
                    .map(|device| device.port.clone());
                if view
                    .config
                    .routing
                    .devices
                    .iter()
                    .any(|device| device.id != id && device.port == *port)
                {
                    view.pending_errors.push(error_notification(
                        "NanoDrive port error",
                        "That serial port is already assigned to another box.",
                    ));
                    select.update(cx, |state, cx| match previous {
                        Some(previous) if !previous.is_empty() => {
                            state.set_selected_value(&previous, window, cx)
                        }
                        _ => state.set_selected_index(None, window, cx),
                    });
                } else if let Some(device) = view
                    .config
                    .routing
                    .devices
                    .iter_mut()
                    .find(|device| device.id == id)
                {
                    if device.port != *port {
                        device.port = port.clone();
                        view.save();
                    }
                }
            }
            cx.notify();
        },
    );
    DevicePort {
        select,
        _subscription: subscription,
    }
}

struct LibraryScanNotification;

struct ScanProgressView {
    origin: ScanOrigin,
    progress: ScanProgress,
}

impl Render for ScanProgressView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let message = scan_progress_text(self.origin, self.progress);
        let copy_text = notification_copy_text("Scanning library", &message, None).into();
        div()
            .flex()
            .flex_col()
            .child(div().text_sm().child(message))
            .child(
                div().mt_2().child(
                    Progress::new("library-scan-progress")
                        .loading(true)
                        .color(rgb(ACCENT_COLOR))
                        .accessibility_label("Library scan in progress"),
                ),
            )
            .child(notification_copy_row(copy_text))
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ScanProgressAction {
    Show,
    Update,
    None,
}

fn scan_progress_action(
    shown: bool,
    previous: Option<ScanProgress>,
    current: ScanProgress,
    elapsed: Option<Duration>,
) -> ScanProgressAction {
    if !shown {
        ScanProgressAction::Show
    } else if previous != Some(current)
        && elapsed.is_none_or(|elapsed| elapsed >= Duration::from_secs(1))
    {
        ScanProgressAction::Update
    } else {
        ScanProgressAction::None
    }
}

struct GainControl {
    slider: Entity<SliderState>,
    _subscription: Subscription,
}

fn error_notification(title: &'static str, message: impl Into<SharedString>) -> Notification {
    error_notification_with_details(title, message.into(), None)
}

fn error_details_notification(
    title: &'static str,
    message: impl Into<SharedString>,
    errors: Vec<String>,
) -> Notification {
    error_notification_with_details(title, message.into(), Some(errors.join("\n")))
}

fn notification_copy_text(title: &str, message: &str, details: Option<&str>) -> String {
    let mut text = format!("{title}\n{message}");
    if let Some(details) = details.filter(|details| !details.is_empty()) {
        text.push('\n');
        text.push_str(details);
    }
    text
}

fn notification_copy_row(text: SharedString) -> Div {
    div().flex().justify_end().child(
        Button::new("copy-notification")
            .icon(assets::IconName::Copy)
            .ghost()
            .xsmall()
            .tooltip("Copy notification")
            .accessibility_label("Copy notification")
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
            }),
    )
}

fn error_notification_with_details(
    title: &'static str,
    message: SharedString,
    details: Option<String>,
) -> Notification {
    let copy_text: SharedString =
        notification_copy_text(title, &message, details.as_deref()).into();
    let details: Option<SharedString> = details.map(Into::into);
    Notification::error(message)
        .title(title)
        .autohide(false)
        .content(move |_, _, _| {
            div()
                .flex()
                .flex_col()
                .when_some(details.clone(), |content, details| {
                    content.child(
                        div()
                            .id("error-details")
                            .max_h(px(180.))
                            .overflow_y_scrollbar()
                            .child(div().text_sm().child(details)),
                    )
                })
                .child(notification_copy_row(copy_text.clone()))
                .into_any_element()
        })
}

struct AppView {
    config_path: Option<PathBuf>,
    config: Config,
    page: Page,
    roll_mode: visualizer::RollMode,
    roll_hidden_channels: HashSet<(u8, u8)>,
    device_service: device::DeviceService,
    device_status: HashMap<u64, DeviceStatus>,
    device_models: HashMap<u64, String>,
    device_ports: HashMap<u64, DevicePort>,
    available_ports: Vec<String>,
    scanning_ports: bool,
    routing: routing::SessionRouting,
    selected_source: Option<(routing::HardwareChip, routing::ChipInstance)>,
    tracks: Vec<Track>,
    track_by_path: HashMap<PathBuf, usize>,
    filtered_tracks: Vec<usize>,
    selected_tracks: HashSet<PathBuf>,
    selected_queue: HashSet<usize>,
    track_anchor: Option<PathBuf>,
    track_selection_order: Vec<PathBuf>,
    queue_anchor: Option<usize>,
    queue_selection_order: Vec<usize>,
    details_selection: DetailsSelection,
    track_focus: FocusHandle,
    queue_focus: FocusHandle,
    playlist_tracks: HashMap<PathBuf, Track>,
    details_path: Option<PathBuf>,
    details_artwork: Option<Arc<RenderImage>>,
    details_artwork_loading: bool,
    details_artwork_error: bool,
    details_artwork_size: u32,
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
    gain_controls: HashMap<(Chip, GainPart), GainControl>,
    gain_sources: Option<Vec<GainSource>>,
    gain_sources_failed: bool,
    player: Playback,
    idle_target: Option<IdlePreviewTarget>,
    idle_preview: Option<IdlePreview>,
    idle_preview_error: Option<String>,
    transition_preview: Option<IdlePreview>,
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
    update_available: Option<semver::Version>,
    persistence_error: Option<String>,
    pending_errors: Vec<Notification>,
    scanning: bool,
    scan_id: u64,
    scan_progress: Option<(Arc<Mutex<ScanProgress>>, ScanOrigin)>,
    scan_progress_view: Entity<ScanProgressView>,
    scan_notification_shown: bool,
    displayed_scan_progress: Option<ScanProgress>,
    scan_progress_updated_at: Option<Instant>,
    completed_scan_notification: Option<(usize, Vec<String>)>,
    _poll: Task<()>,
}

#[derive(Clone, PartialEq, Eq)]
struct IdlePreviewTarget {
    path: PathBuf,
    plan: Option<routing::PlaybackPlan>,
    looped: bool,
    settings: library::PlaybackSettings,
}

impl AppView {
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let loop_get = view.clone();
            let loop_set = view.clone();
            let fade_get = view.clone();
            let fade_set = view.clone();
            let loop_field = SettingField::number_input(
                NumberFieldOptions {
                    min: 1.,
                    max: 99.,
                    step: 1.,
                },
                move |cx| {
                    loop_get
                        .upgrade()
                        .map_or(2., |view| view.read(cx).config.playback.loop_count as f64)
                },
                move |value, cx| {
                    let _ = loop_set.update(cx, |view, cx| {
                        view.config.playback.loop_count = value.round() as u32;
                        view.save();
                        view.sync_idle_preview(cx);
                        cx.notify();
                    });
                },
            )
            .default_value(2.);
            let fade_field = SettingField::number_input(
                NumberFieldOptions {
                    min: 0.,
                    max: 600.,
                    step: 1.,
                },
                move |cx| {
                    fade_get.upgrade().map_or(2., |view| {
                        view.read(cx).config.playback.fadeout_seconds as f64
                    })
                },
                move |value, cx| {
                    let _ = fade_set.update(cx, |view, cx| {
                        view.config.playback.fadeout_seconds = value.round() as u32;
                        view.save();
                        view.sync_idle_preview(cx);
                        cx.notify();
                    });
                },
            )
            .default_value(2.);
            dialog.title("Settings").width(px(760.)).centered().child(
                div().h(px(400.)).w_full().child(
                    Settings::new("nddrv-settings")
                        .sidebar_width(px(180.))
                        .sidebar_size_range(px(150.)..px(240.))
                        .page(
                            SettingPage::new("Playback")
                                .icon(assets::IconName::Play)
                                .group(SettingGroup::new().items([
                                    SettingItem::new("Play count", loop_field),
                                    SettingItem::new("Fade out (seconds)", fade_field),
                                ])),
                        ),
                ),
            )
        });
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter text, chip, format or NanoDrive")
        });
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
        let scan_progress_view = cx.new(|_| ScanProgressView {
            origin: ScanOrigin::Background,
            progress: ScanProgress::default(),
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
        let (config_path, mut config, config_error) = match library::config_path() {
            Ok(path) => match library::load_config(&path) {
                Ok(config) => (Some(path), config, None),
                Err(error) => (None, Config::default(), Some(format!("{error:#}"))),
            },
            Err(error) => (None, Config::default(), Some(format!("{error:#}"))),
        };
        if config.routing.devices.is_empty() {
            config.routing.devices.push(routing::Device {
                id: 1,
                name: String::new(),
                port: String::new(),
                baud_rate: device::BAUD_RATE,
            });
        }
        let device_ports = config
            .routing
            .devices
            .iter()
            .map(|device| {
                (
                    device.id,
                    device_port(device.id, &device.port, &[], window, cx),
                )
            })
            .collect();
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
            roll_mode: visualizer::RollMode::default(),
            roll_hidden_channels: HashSet::new(),
            device_service: device::DeviceService::new(),
            device_status: HashMap::new(),
            device_models: HashMap::new(),
            device_ports,
            available_ports: Vec::new(),
            scanning_ports: false,
            routing: routing::SessionRouting::default(),
            selected_source: None,
            tracks: Vec::new(),
            track_by_path: HashMap::new(),
            filtered_tracks: Vec::new(),
            selected_tracks: HashSet::new(),
            selected_queue: HashSet::new(),
            track_anchor: None,
            track_selection_order: Vec::new(),
            queue_anchor: None,
            queue_selection_order: Vec::new(),
            details_selection: DetailsSelection::Music,
            track_focus: cx.focus_handle(),
            queue_focus: cx.focus_handle(),
            playlist_tracks: HashMap::new(),
            details_path: None,
            details_artwork: None,
            details_artwork_loading: false,
            details_artwork_error: false,
            details_artwork_size: 0,
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
            gain_controls: HashMap::new(),
            gain_sources: None,
            gain_sources_failed: false,
            player: Playback::new(),
            idle_target: None,
            idle_preview: None,
            idle_preview_error: None,
            transition_preview: None,
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
            update_available: None,
            persistence_error,
            pending_errors,
            scanning: false,
            scan_id: 0,
            scan_progress: None,
            scan_progress_view,
            scan_notification_shown: false,
            displayed_scan_progress: None,
            scan_progress_updated_at: None,
            completed_scan_notification: None,
            _poll: poll,
        }
    }

    fn check_for_updates(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async { update::check() }).await;
            if let Ok(Some(version)) = result {
                let _ = this.update(cx, |view, cx| {
                    view.update_available = Some(version);
                    cx.notify();
                });
            }
        })
        .detach();
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
        self.filtered_tracks = library::filter_indices_for_music(
            &self.tracks,
            &query,
            self.selected_directory.as_deref(),
            &chips,
            &nanodrive_chips(&self.device_status),
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
            has_connected_nanodrive(&self.device_status),
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
                "Could not add filter",
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
                let id = token.token().id();
                let ymfm = ChipTag::is_ymfm_name(id.as_ref());
                let format = matches!(id.as_ref(), "MDX" | "VGM" | "NanoDrive");
                let (background, foreground) = if format {
                    (0x4a606d, 0xe7f1f3)
                } else {
                    chip_colors(ymfm)
                };
                div()
                    .max_w(token.available_width())
                    .pr(px(4.))
                    .child(InputToken::new(token).when(!token.is_selected(), |tag| {
                        tag.bg(rgb(background))
                            .text_color(rgb(foreground))
                            .border_color(rgb(if format || ymfm { background } else { 0xb77ad4 }))
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
        self.queue_selection_order.clear();
        self.queue_anchor = None;
        let remaining: HashSet<_> = self.config.playlist.iter().collect();
        self.playlist_tracks
            .retain(|path, _| remaining.contains(path));
        self.playing = playing;
        if stopped {
            self.player.stop();
            self.paused = false;
            self.elapsed = 0;
        }
        if self.playing.is_none() && removed.contains(&0) {
            self.idle_preview = None;
            self.idle_target = None;
            self.idle_preview_error = None;
            self.gain_sources = None;
        }
        self.status = format!("Removed {count} tracks from playlist");
        self.save();
        cx.notify();
    }

    fn update_directory_tree(&mut self) {
        self.visible_directories =
            library::visible_directories(&self.directories, &self.expanded_directories);
    }

    fn load_playlist_metadata(&mut self, origin: ScanOrigin, cx: &mut Context<Self>) {
        self.playlist_tracks.clear();
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
                            let metadata = library::read_track_info(&path);
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
                        Ok(track) => {
                            view.playlist_tracks.insert(path, track);
                        }
                        Err(error) => errors.push(format!("{}: {error:#}", path.display())),
                    }
                }
                let errors = reported_scan_errors(origin, errors);
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

    fn scan(&mut self, origin: ScanOrigin, cx: &mut Context<Self>) {
        self.scan_id = self.scan_id.wrapping_add(1);
        let scan_id = self.scan_id;
        let dirs = self.config.directories.clone();
        let progress = Arc::new(Mutex::new(ScanProgress::default()));
        self.scan_progress = Some((progress.clone(), origin));
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
                view.completed_scan_notification = Some((
                    view.tracks.len(),
                    reported_scan_errors(origin, result.errors),
                ));
                view.load_playlist_metadata(origin, cx);
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
                    view.scan(ScanOrigin::Add, cx);
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
        if self.device_status.values().any(|status| {
            matches!(
                status,
                DeviceStatus::Connecting | DeviceStatus::Disconnecting
            )
        }) {
            self.pending_errors.push(error_notification(
                "NanoDrive connection in progress",
                "Wait for the connection operation to finish before playback.",
            ));
            cx.notify();
            return;
        }
        let plan = self.routing.playback_plan(&self.config.routing.devices);
        if let Some(path) = self.config.playlist.get(index).cloned() {
            self.player.set_settings(self.config.playback);
            let use_transition =
                self.playing.is_none() && index == 0 && self.current_idle_preview().is_some();
            let preview_mute = if self.playing.is_none()
                && index == 0
                && self
                    .idle_target
                    .as_ref()
                    .is_some_and(|target| target.path == path)
            {
                self.idle_preview.as_ref().map(IdlePreview::fm_mute)
            } else {
                None
            };
            self.status = "Playing".into();
            self.playing = Some(index);
            if !use_transition {
                self.gain_sources = None;
            }
            self.gain_sources_failed = false;
            self.playing_hardware = plan.is_some();
            self.paused = false;
            self.elapsed = 0;
            let indexed = self.track_by_path.get(&path).copied();
            self.playing_title = indexed
                .map(|index| self.tracks[index].display_title())
                .or_else(|| self.playlist_tracks.get(&path).map(Track::display_title))
                .unwrap_or_else(|| Track::format_display_title(&path, &Default::default()));
            if let Some(plan) = plan {
                self.player.play_hardware(
                    path.clone(),
                    self.looped,
                    self.device_service.playback_handle(),
                    plan,
                    preview_mute,
                );
            } else {
                self.player.play(path.clone(), self.looped, preview_mute);
            }
            self.transition_preview = if use_transition {
                self.idle_preview.take()
            } else {
                self.idle_preview = None;
                None
            };
            self.idle_target = None;
            self.idle_preview_error = None;
            self.displayed_volume_mode = self.player.volume_mode();
            if indexed.is_none() {
                let generation = self.player.generation();
                cx.spawn(async move |this, cx| {
                    let load_path = path.clone();
                    let result = cx
                        .background_spawn(async move { library::read_track_info(&load_path) })
                        .await;
                    let _ = this.update(cx, |view, cx| {
                        if view.playing.is_some() && view.player.is_current(generation) {
                            match result {
                                Ok(track) => {
                                    view.playing_title =
                                        Track::format_display_title(&path, &track.metadata);
                                    view.playlist_tracks.insert(path.clone(), track);
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

    fn idle_preview_target(&self) -> Option<IdlePreviewTarget> {
        self.config.playlist.first().map(|path| IdlePreviewTarget {
            path: path.clone(),
            plan: self.routing.playback_plan(&self.config.routing.devices),
            looped: self.looped,
            settings: self.config.playback,
        })
    }

    fn current_idle_preview(&self) -> Option<&IdlePreview> {
        (self.playing.is_none()
            && self.idle_target.is_some()
            && self.idle_target == self.idle_preview_target())
        .then_some(self.idle_preview.as_ref())
        .flatten()
    }

    fn piano_fm_mute(&self) -> &fm_mute::FmMute {
        self.current_idle_preview()
            .map_or_else(|| self.player.fm_mute(), IdlePreview::fm_mute)
    }

    fn piano_sources(&self) -> Option<&Vec<GainSource>> {
        if self.playing.is_some() || self.current_idle_preview().is_some() {
            self.gain_sources.as_ref()
        } else {
            None
        }
    }

    fn sync_idle_preview(&mut self, cx: &mut Context<Self>) {
        if self.playing.is_some() || self.page != Page::Piano {
            return;
        }
        let target = self.idle_preview_target();
        if self.idle_target == target && (target.is_some() || self.gain_sources.is_none()) {
            return;
        }
        let previous = self.idle_preview.take();
        let same_track = self
            .idle_target
            .as_ref()
            .zip(target.as_ref())
            .is_some_and(|(old, new)| old.path == new.path);
        self.idle_target = target;
        self.idle_preview_error = None;
        self.gain_sources = None;
        self.gain_sources_failed = false;
        if let Some(target) = &self.idle_target {
            let result = IdlePreview::start_with_settings(
                target.path.clone(),
                target.looped,
                target.plan.as_ref(),
                target.settings,
            );
            match result {
                Ok(preview) => {
                    if same_track && let Some(previous) = &previous {
                        preview.fm_mute().copy_selections_from(previous.fm_mute());
                    }
                    self.idle_preview = Some(preview);
                }
                Err(error) => self.idle_preview_error = Some(format!("{error:#}")),
            }
        }
        cx.notify();
    }

    fn piano_channel_panel(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut rows = Vec::new();
        if let Some(sources) = self.piano_sources() {
            for source in sources {
                let Some((index, total)) = visualizer::tracked_chip(&source.chip) else {
                    continue;
                };
                let fm_count = fm_mute::fm_channels(&source.chip).unwrap_or(0);
                let chip_id = visualizer::source_color_index(index, source.instance as u8);
                let title = format!(
                    "{} #{}{}",
                    Track::chip_name(&source.chip),
                    source.instance as u8 + 1,
                    if source.hardware { " (NanoDrive)" } else { "" }
                );
                rows.push(div().text_xs().text_color(rgb(0xb9e9e7)).child(title));
                for channel in 0..total {
                    let paired =
                        self.piano_fm_mute()
                            .paired_channel(&source.chip, source.instance, channel);
                    if paired.is_some_and(|other| other < channel) {
                        continue;
                    }
                    let mut colors = vec![channel];
                    if let Some(other) = paired {
                        colors.push(other);
                    }
                    let mut swatches = Vec::new();
                    for color_channel in colors {
                        let hidden = self
                            .roll_hidden_channels
                            .contains(&(chip_id, color_channel));
                        let color = visualizer::channel_color(chip_id, color_channel);
                        swatches.push(
                            Button::new(format!("piano-visible-{chip_id}-{color_channel}"))
                                .w(px(25.))
                                .h(px(25.))
                                .p_0()
                                .tooltip(if hidden { "Show notes" } else { "Hide notes" })
                                .accessibility_label(format!(
                                    "{} instance {} channel {} notes",
                                    Track::chip_name(&source.chip),
                                    source.instance as u8 + 1,
                                    color_channel + 1
                                ))
                                .toggled(!hidden)
                                .child(div().size(px(16.)).rounded_none().bg(rgba(if hidden {
                                    (color & 0xffffff00) | 0x55
                                } else {
                                    color
                                })))
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    if !view.roll_hidden_channels.remove(&(chip_id, color_channel))
                                    {
                                        view.roll_hidden_channels.insert((chip_id, color_channel));
                                    }
                                    cx.notify();
                                })),
                        );
                    }
                    let label = if source.chip == Chip::Sn76489 {
                        format!("Tone {}", channel + 1)
                    } else if channel >= fm_count {
                        format!("SSG {}", channel - fm_count + 1)
                    } else if let Some(other) = paired {
                        format!("FM {}+{}", channel + 1, other + 1)
                    } else {
                        format!("FM {}", channel + 1)
                    };
                    let mut row = div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .children(swatches)
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(rgb(0x82a8bb))
                                .child(label),
                        );
                    if channel < fm_count {
                        let chip = source.chip.clone();
                        let instance = source.instance;
                        let muted = self.piano_fm_mute().is_muted(&chip, instance, channel);
                        let soloed = self.piano_fm_mute().is_soloed(&chip, instance, channel);
                        let rhythm = self
                            .piano_fm_mute()
                            .rhythm_channel(&chip, instance, channel);
                        let solo_chip = chip.clone();
                        let channel_name = if let Some(other) = paired {
                            format!("FM {} and {}", channel + 1, other + 1)
                        } else {
                            format!("FM {}", channel + 1)
                        };
                        row = row
                            .child(
                                Button::new(format!("piano-mute-{chip_id}-{channel}"))
                                    .icon(assets::IconName::VolumeX)
                                    .w(px(25.))
                                    .h(px(25.))
                                    .p_0()
                                    .tooltip(if rhythm {
                                        "Rhythm mode is not controlled by FM mute or solo"
                                    } else {
                                        "Toggle FM mute (effective on the next key-on)"
                                    })
                                    .accessibility_label(format!(
                                        "{} instance {} {} mute",
                                        Track::chip_name(&source.chip),
                                        instance as u8 + 1,
                                        channel_name
                                    ))
                                    .when(muted, |button| button.primary())
                                    .toggled(muted)
                                    .disabled(rhythm)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        let state = view.piano_fm_mute();
                                        let muted = state.is_muted(&chip, instance, channel);
                                        if !state.set_muted(&chip, instance, channel, !muted) {
                                            view.pending_errors.push(error_notification(
                                                "FM mute error",
                                                "Invalid FM channel",
                                            ));
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new(format!("piano-solo-{chip_id}-{channel}"))
                                    .icon(assets::IconName::Headphones)
                                    .w(px(25.))
                                    .h(px(25.))
                                    .p_0()
                                    .tooltip(if rhythm {
                                        "Rhythm mode is not controlled by FM mute or solo"
                                    } else {
                                        "Toggle FM solo (effective on the next key-on)"
                                    })
                                    .accessibility_label(format!(
                                        "{} instance {} {} solo",
                                        Track::chip_name(&source.chip),
                                        instance as u8 + 1,
                                        channel_name
                                    ))
                                    .when(soloed, |button| button.primary())
                                    .toggled(soloed)
                                    .disabled(rhythm)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        let state = view.piano_fm_mute();
                                        let soloed = state.is_soloed(&solo_chip, instance, channel);
                                        if !state.set_soloed(&solo_chip, instance, channel, !soloed)
                                        {
                                            view.pending_errors.push(error_notification(
                                                "FM solo error",
                                                "Invalid FM channel",
                                            ));
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(rhythm, |row| {
                                row.child(div().text_xs().text_color(rgb(0xd0a76e)).child("Rhythm"))
                            });
                    }
                    rows.push(row);
                }
            }
        }
        let empty = if self.playing.is_none() && self.config.playlist.is_empty() {
            "Add a track to view channels"
        } else if self.idle_preview_error.is_some() {
            "Channel preview unavailable"
        } else if self.piano_sources().is_none() {
            "Preparing channel controls..."
        } else {
            "No tracked channels"
        };
        let height = if rows.is_empty() {
            76.
        } else {
            rows.len() as f32 * 29. + 44.
        };
        div()
            .id("piano-channels")
            .w_full()
            .h(px(height))
            .flex_shrink(1.)
            .min_h_0()
            .overflow_hidden()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(0x284452))
            .bg(rgba(0x0b1623ed))
            .child(
                div()
                    .id("piano-channels-scroll")
                    .size_full()
                    .overflow_y_scrollbar()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(0x64eadc)).child("CHANNELS"))
                    .when(rows.is_empty(), |panel| {
                        panel.child(div().text_xs().text_color(rgb(0x82a8bb)).child(empty))
                    })
                    .children(rows),
            )
    }

    fn piano_gain_panel(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        if let Some(sources) = self.piano_sources() {
            for source in sources {
                if !seen.insert(source.chip.clone()) {
                    continue;
                }
                let software = sources
                    .iter()
                    .any(|other| other.chip == source.chip && !other.hardware);
                let hardware = sources
                    .iter()
                    .any(|other| other.chip == source.chip && other.hardware);
                let name = Track::chip_name(&source.chip);
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .text_xs()
                        .text_color(rgb(0xb9e9e7))
                        .child(name)
                        .when(hardware, |row| {
                            row.child(div().text_color(rgb(0xd0a76e)).child(if software {
                                "NanoDrive fixed"
                            } else {
                                "NanoDrive"
                            }))
                        }),
                );
                if !software {
                    continue;
                }
                let mut parts = vec![(GainPart::Chip, "Chip")];
                if matches!(source.chip, Chip::Ym2203 | Chip::Ym2608 | Chip::Ym2610b) {
                    let main = if source.chip == Chip::Ym2203 {
                        "FM"
                    } else {
                        "FM / ADPCM"
                    };
                    parts.extend([(GainPart::Fm, main), (GainPart::Ssg, "SSG")]);
                }
                for (part, label) in parts {
                    if let Some(control) = self.gain_controls.get(&(source.chip.clone(), part)) {
                        let value = match control.slider.read(cx).value() {
                            SliderValue::Single(value) => value,
                            SliderValue::Range(_, _) => {
                                unreachable!("chip gain is a single slider")
                            }
                        };
                        rows.push(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(78.))
                                        .text_xs()
                                        .text_color(rgb(0x82a8bb))
                                        .child(label),
                                )
                                .child(
                                    Slider::new(&control.slider)
                                        .accessibility_label(format!("{name} {label} gain"))
                                        .bg(rgb(ACCENT_COLOR))
                                        .flex_1(),
                                )
                                .child(
                                    div()
                                        .w(px(38.))
                                        .text_right()
                                        .text_xs()
                                        .text_color(rgb(0xd3eeee))
                                        .child(format!("{value:.0}%")),
                                ),
                        );
                    }
                }
            }
        }
        let empty = if self.playing.is_none() && self.config.playlist.is_empty() {
            "Add a track to adjust chip gains"
        } else if self.idle_preview_error.is_some() {
            "Chip preview unavailable"
        } else if self.piano_sources().is_none() {
            "Preparing chip controls..."
        } else {
            "No playable chip outputs"
        };
        let height = (rows.len() as f32 * 31. + 72.).min(400.);
        div()
            .id("piano-chip-gains")
            .w_full()
            .h(px(height))
            .max_h(relative(0.45))
            .flex_shrink_0()
            .overflow_hidden()
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(0x284452))
            .bg(rgba(0x0b1623ed))
            .child(
                div()
                    .id("piano-chip-gains-scroll")
                    .size_full()
                    .overflow_y_scrollbar()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(0x64eadc)).child("CHIP GAIN"))
                    .when(rows.is_empty(), |panel| {
                        panel.child(div().text_xs().text_color(rgb(0x82a8bb)).child(empty))
                    })
                    .children(rows),
            )
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Ok(event) = self.device_service.events.try_recv() {
            match event {
                device::DeviceEvent::Connected(id, model, capabilities) => {
                    if let Some(device) = self
                        .config
                        .routing
                        .devices
                        .iter_mut()
                        .find(|device| device.id == id)
                    {
                        device.baud_rate = device::BAUD_RATE;
                    }
                    self.device_models.insert(id, model);
                    renumber_connected_devices(
                        &mut self.config.routing.devices,
                        &self.device_models,
                    );
                    self.save();
                    self.device_status
                        .insert(id, DeviceStatus::Connected(capabilities));
                    self.refresh_routing(false);
                    self.suggestion_index = 0;
                    self.update_filter(cx);
                    if self
                        .config
                        .routing
                        .devices
                        .last()
                        .is_some_and(|device| device.id == id)
                    {
                        self.add_empty_device(window, cx);
                    }
                }
                device::DeviceEvent::Disconnected(id) => {
                    self.device_status.remove(&id);
                    self.device_models.remove(&id);
                    if renumber_connected_devices(
                        &mut self.config.routing.devices,
                        &self.device_models,
                    ) {
                        self.save();
                    }
                    self.refresh_routing(true);
                    self.suggestion_index = 0;
                    self.update_filter(cx);
                }
                device::DeviceEvent::Error(id, error) => {
                    let was_connected = matches!(
                        self.device_status.get(&id),
                        Some(DeviceStatus::Connected(_) | DeviceStatus::Disconnecting)
                    );
                    self.device_status.remove(&id);
                    self.device_models.remove(&id);
                    if renumber_connected_devices(
                        &mut self.config.routing.devices,
                        &self.device_models,
                    ) {
                        self.save();
                    }
                    self.refresh_routing(was_connected);
                    self.suggestion_index = 0;
                    self.update_filter(cx);
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
                    self.suggestion_index = 0;
                    self.update_filter(cx);
                    self.pending_errors
                        .push(error_notification("NanoDrive connection error", error));
                }
                device::DeviceEvent::Ports(result) => {
                    self.scanning_ports = false;
                    match result {
                        Ok(ports) => {
                            self.available_ports = ports;
                            for port in self.device_ports.values() {
                                port.select.update(cx, |state, cx| {
                                    let selected = state.selected_value().cloned();
                                    let mut items = self.available_ports.clone();
                                    if let Some(selected) = &selected
                                        && !items.contains(selected)
                                    {
                                        items.push(selected.clone());
                                    }
                                    state.set_items(SearchableVec::new(items), window, cx);
                                    if let Some(selected) = selected {
                                        state.set_selected_value(&selected, window, cx);
                                    }
                                });
                            }
                        }
                        Err(error) => self
                            .pending_errors
                            .push(error_notification("Serial port scan error", error)),
                    }
                }
            }
            cx.notify();
        }
        if let Some((count, errors)) = self.completed_scan_notification.take() {
            self.scan_notification_shown = false;
            let notification = if errors.is_empty() {
                let message = format!("{count} tracks found");
                let copy_text: SharedString =
                    notification_copy_text("Library scan complete", &message, None).into();
                Notification::success(message)
                    .title("Library scan complete")
                    .content(move |_, _, _| {
                        notification_copy_row(copy_text.clone()).into_any_element()
                    })
            } else {
                let summary = format!("{count} tracks found; {} errors", errors.len());
                error_details_notification("Library scan completed with errors", summary, errors)
            };
            window.push_notification(notification.id::<LibraryScanNotification>(), cx);
        } else if let Some((progress, origin)) = &self.scan_progress {
            let progress = *progress.lock().expect("scan progress mutex poisoned");
            let action = scan_progress_action(
                self.scan_notification_shown,
                self.displayed_scan_progress,
                progress,
                self.scan_progress_updated_at
                    .map(|updated| updated.elapsed()),
            );
            if action != ScanProgressAction::None {
                self.displayed_scan_progress = Some(progress);
                self.scan_progress_updated_at = Some(Instant::now());
                self.scan_progress_view.update(cx, |view, cx| {
                    view.progress = progress;
                    view.origin = *origin;
                    cx.notify();
                });
            }
            if action == ScanProgressAction::Show {
                self.scan_notification_shown = true;
                let progress_view = self.scan_progress_view.clone();
                window.push_notification(
                    Notification::new()
                        .with_type(NotificationType::Info)
                        .title("Scanning library")
                        .id::<LibraryScanNotification>()
                        .autohide(false)
                        .content(move |_, _, _| progress_view.clone().into_any_element()),
                    cx,
                );
            }
        }
        self.sync_idle_preview(cx);
        let sources = if self.playing.is_some() {
            Some(self.player.gain_sources())
        } else {
            self.current_idle_preview().map(IdlePreview::gain_sources)
        };
        if let Some(sources) = sources {
            match sources {
                Ok(sources)
                    if keep_preview_sources(
                        self.playing.is_some(),
                        self.transition_preview.is_some(),
                        self.gain_sources.as_deref(),
                        sources.as_deref(),
                    ) => {}
                Ok(sources) if self.gain_sources != sources => {
                    self.gain_sources_failed = false;
                    if let Some(sources) = &sources {
                        for source in sources.iter().filter(|source| !source.hardware) {
                            let mut parts = vec![GainPart::Chip];
                            if matches!(source.chip, Chip::Ym2203 | Chip::Ym2608 | Chip::Ym2610b) {
                                parts.extend([GainPart::Fm, GainPart::Ssg]);
                            }
                            for part in parts {
                                let key = (source.chip.clone(), part);
                                if self.gain_controls.contains_key(&key) {
                                    continue;
                                }
                                let slider =
                                    cx.new(|_| SliderState::new().max(100.).default_value(100.));
                                let chip = source.chip.clone();
                                let subscription =
                                    cx.subscribe(&slider, move |view: &mut Self, _, event, cx| {
                                        if let SliderEvent::Change(SliderValue::Single(value)) =
                                            event
                                        {
                                            if let Err(error) = view.player.set_chip_gain(
                                                chip.clone(),
                                                part,
                                                *value / 100.,
                                            ) {
                                                view.pending_errors.push(error_notification(
                                                    "Chip gain error",
                                                    error.to_string(),
                                                ));
                                            }
                                            cx.notify();
                                        }
                                    });
                                self.gain_controls.insert(
                                    key,
                                    GainControl {
                                        slider,
                                        _subscription: subscription,
                                    },
                                );
                            }
                        }
                    }
                    self.gain_sources = sources;
                    cx.notify();
                }
                Ok(_) => self.gain_sources_failed = false,
                Err(error) if !self.gain_sources_failed => {
                    self.gain_sources_failed = true;
                    self.pending_errors
                        .push(error_notification("Chip gain error", error.to_string()));
                }
                Err(_) => {}
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
        if self.transition_preview.is_some() {
            let finished = if self.playing.is_none() {
                true
            } else {
                match self.player.visual_snapshot(visualizer::RollMode::Incoming) {
                    Ok(snapshot) => {
                        snapshot.rate != 0
                            && !snapshot.preparing
                            && self
                                .player
                                .gain_sources()
                                .is_ok_and(|sources| sources.is_some())
                    }
                    Err(_) => true,
                }
            };
            if finished {
                self.transition_preview = None;
                cx.notify();
            }
        }
        self.refresh_details(window, cx);
        for error in std::mem::take(&mut self.pending_errors) {
            window.push_notification(error, cx);
        }
    }

    fn refresh_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = details_target(
            self.playing,
            &self.config.playlist,
            &self.track_selection_order,
            &self.queue_selection_order,
            self.details_selection,
        )
        .cloned();
        let artwork_size = artwork::target_size(window.scale_factor());
        if target == self.details_path && artwork_size == self.details_artwork_size {
            return;
        }
        self.details_request = self.details_request.wrapping_add(1);
        let request = self.details_request;
        if target != self.details_path {
            if let Some(image) = self.details_artwork.take() {
                cx.drop_image(image, Some(window));
            }
            self.details_path = target.clone();
        }
        self.details_artwork_size = artwork_size;
        self.details_artwork_error = false;
        self.details_artwork_loading = target.is_some();
        cx.notify();
        let Some(path) = target else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let search_path = path.clone();
            let search = cx
                .background_spawn(async move { library::find_artwork(&search_path) })
                .await;
            let result = match search {
                Ok(Some(artwork_path)) => {
                    let load = this.update(cx, |view, cx| {
                        if view.details_request != request {
                            return None;
                        }
                        let future: std::pin::Pin<
                            Box<dyn std::future::Future<Output = _> + Send + 'static>,
                        > = Box::pin(ImgResourceLoader::load(artwork_path.into(), cx));
                        Some(future)
                    });
                    match load {
                        Ok(Some(load)) => {
                            let decoded = cx.background_spawn(load).await;
                            match decoded {
                                Ok(image) => cx
                                    .background_spawn(async move {
                                        artwork::resize(image, artwork_size)
                                    })
                                    .await
                                    .map(Some)
                                    .map_err(|error| (false, error.to_string())),
                                Err(error) => Err((false, error.to_string())),
                            }
                        }
                        _ => return,
                    }
                }
                Ok(None) => Ok(None),
                Err(error) => Err((true, format!("{error:#}"))),
            };
            let _ = this.update(cx, |view, cx| {
                if view.details_request != request || view.details_path.as_ref() != Some(&path) {
                    return;
                }
                view.details_artwork_loading = false;
                match result {
                    Ok(artwork) => {
                        if let Some(old) = std::mem::replace(&mut view.details_artwork, artwork) {
                            cx.drop_image(old, None);
                        }
                    }
                    Err((directory_error, error)) => {
                        view.details_artwork_error = true;
                        if directory_error || view.details_artwork.is_some() {
                            view.pending_errors.push(error_notification(
                                "Artwork error",
                                format!("{}: {error}", path.display()),
                            ));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn track_details(&self) -> impl IntoElement {
        let path = self.details_path.as_ref();
        let track = path.and_then(|path| {
            self.track_by_path
                .get(path)
                .map(|&index| &self.tracks[index])
                .or_else(|| self.playlist_tracks.get(path))
        });
        let metadata = track.map(|track| &track.metadata);
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
            .h(relative(0.4))
            .min_h(px(80.))
            .max_h(px(260.))
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
                            .flex_wrap()
                            .items_start()
                            .gap(px(16.))
                            .min_w_0()
                            .when(path.is_some(), |row| {
                                row.child(
                                    div()
                                        .w(px(artwork::ARTWORK_SIZE))
                                        .max_w(relative(1.))
                                        .aspect_square()
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .bg(rgb(0x243441))
                                        .when_some(artwork, |node, artwork| {
                                            node.child(
                                                img(artwork)
                                                    .id("track-artwork")
                                                    .w_full()
                                                    .h_full()
                                                    .with_fallback(|| {
                                                        div()
                                                            .text_xs()
                                                            .child("Image error")
                                                            .into_any_element()
                                                    }),
                                            )
                                        })
                                        .when(self.details_artwork.is_none(), |node| {
                                            node.child(
                                                div().text_xs().text_color(rgb(0xa6b6c3)).child(
                                                    if loading {
                                                        "Loading..."
                                                    } else if self.details_artwork_error {
                                                        "Image error"
                                                    } else {
                                                        "No image"
                                                    },
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
                                    .min_w(px(120.))
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
                                    }))
                                    .when_some(
                                        track.filter(|track| !track.chips.is_empty()),
                                        |node, track| {
                                            node.child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .min_w_0()
                                                    .gap_1()
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(rgb(0xa6b6c3))
                                                            .child("Chips"),
                                                    )
                                                    .child(
                                                        div().flex().flex_wrap().gap_1().children(
                                                            track
                                                                .chips
                                                                .iter()
                                                                .map(|chip| chip_tag(chip, false)),
                                                        ),
                                                    ),
                                            )
                                        },
                                    ),
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

    fn piano_title(&self) -> String {
        if self.playing.is_some() {
            return self.now_playing_title();
        }
        let Some(path) = self.config.playlist.first() else {
            return "No track playing".into();
        };
        self.track_by_path
            .get(path)
            .map(|&index| self.tracks[index].display_title())
            .or_else(|| self.playlist_tracks.get(path).map(Track::display_title))
            .unwrap_or_else(|| Track::format_display_title(path, &Default::default()))
    }

    fn playback_buttons(&self, cx: &mut Context<Self>) -> Div {
        let active = self.playing.is_some();
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
        let accent = ButtonCustomVariant::new(cx)
            .color(rgb(ACCENT_COLOR).into())
            .foreground(rgb(0xffffff).into())
            .hover(rgb(0x269c94).into())
            .active(rgb(0x208b84).into());
        let previous = adjacent_track(self.playing, self.config.playlist.len(), false);
        let next = adjacent_track(self.playing, self.config.playlist.len(), true);
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
                        } else if view.playing.is_none() && !view.config.playlist.is_empty() {
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
                        "Loop indefinitely".to_string()
                    } else {
                        format!(
                            "Play {} times, then fade out for {} seconds",
                            self.config.playback.loop_count, self.config.playback.fadeout_seconds
                        )
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
        let state = if !active {
            "Stopped"
        } else if self.paused {
            "Paused"
        } else {
            "Playing"
        };
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
                    .text_base()
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
                            .text_2xl()
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
                    .child(self.playback_buttons(cx))
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
                        div().flex().flex_wrap().gap_1().children(
                            self.config
                                .routing
                                .devices
                                .iter()
                                .filter(|device| {
                                    matches!(
                                        self.device_status.get(&device.id),
                                        Some(DeviceStatus::Connected(_))
                                    )
                                })
                                .map(|device| {
                                    div()
                                        .px_2()
                                        .py_1()
                                        .rounded_sm()
                                        .bg(rgb(0x39434b))
                                        .text_sm()
                                        .text_color(rgb(0xd0d8dd))
                                        .child(device.name.clone())
                                }),
                        ),
                    )
                    .when(
                        !self
                            .device_status
                            .values()
                            .any(|status| matches!(status, DeviceStatus::Connected(_))),
                        |row| {
                            row.child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .bg(rgb(0x39434b))
                                    .text_sm()
                                    .text_color(rgb(0xd0d8dd))
                                    .child("Emulation"),
                            )
                        },
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
                                    view.remove_directory(&root, cx);
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    }

    fn remove_directory(&mut self, root: &PathBuf, cx: &mut Context<Self>) {
        self.config
            .directories
            .retain(|directory| directory != root);
        self.expanded_directories.remove(root);
        self.directories.retain(|directory| &directory.root != root);
        if self
            .selected_directory
            .as_ref()
            .is_some_and(|selected| !self.directories.iter().any(|entry| &entry.path == selected))
        {
            self.selected_directory = None;
        }
        self.update_directory_tree();
        self.update_filter(cx);
        self.save();
        self.scan(ScanOrigin::Background, cx);
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

    fn available_devices(&self) -> HashMap<u64, Vec<ndsif::CapabilityEntry>> {
        self.device_status
            .iter()
            .filter_map(|(&id, status)| match status {
                DeviceStatus::Connected(entries) => Some((id, entries.clone())),
                _ => None,
            })
            .collect()
    }

    fn refresh_routing(&mut self, reset: bool) {
        let available = self.available_devices();
        if reset {
            self.routing.reset(&self.config.routing.devices, &available);
        } else {
            self.routing
                .refresh(&self.config.routing.devices, &available);
        }
        self.selected_source = None;
    }

    fn add_empty_device(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self
            .config
            .routing
            .devices
            .iter()
            .map(|device| device.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
        else {
            self.pending_errors.push(error_notification(
                "Could not add NanoDrive",
                "No more device IDs are available",
            ));
            return;
        };
        self.config.routing.devices.push(routing::Device {
            id,
            name: String::new(),
            port: String::new(),
            baud_rate: device::BAUD_RATE,
        });
        self.device_ports
            .insert(id, device_port(id, "", &self.available_ports, window, cx));
        self.save();
    }

    fn connect_device(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.playing.is_some() {
            self.pending_errors.push(error_notification(
                "NanoDrive error",
                "Stop playback before changing devices.",
            ));
            cx.notify();
            return;
        }
        let Some(device) = self
            .config
            .routing
            .devices
            .iter()
            .find(|device| device.id == id)
            .cloned()
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
            let Some(port) = self
                .device_ports
                .get(&id)
                .and_then(|state| state.select.read(cx).selected_value().cloned())
            else {
                self.pending_errors.push(error_notification(
                    "NanoDrive connection error",
                    "Select a serial port.",
                ));
                cx.notify();
                return;
            };
            if self
                .config
                .routing
                .devices
                .iter()
                .any(|other| other.id != id && other.port == port)
            {
                self.pending_errors.push(error_notification(
                    "NanoDrive connection error",
                    "That serial port is already assigned to another box.",
                ));
                cx.notify();
                return;
            }
            let mut device = device;
            device.port = port;
            if let Some(saved) = self
                .config
                .routing
                .devices
                .iter_mut()
                .find(|saved| saved.id == id)
            {
                *saved = device.clone();
            }
            self.save();
            self.device_service
                .connect(device)
                .map(|_| DeviceStatus::Connecting)
        };
        match result {
            Ok(status) => {
                self.device_status.insert(id, status);
                self.suggestion_index = 0;
                self.update_filter(cx);
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
        } else if !can_remove_device(id, &self.config.routing.devices, &self.device_status) {
            self.pending_errors.push(error_notification(
                "Cannot remove NanoDrive",
                "The last NanoDrive box must be kept for new connections.",
            ));
        } else {
            self.config.routing.devices.retain(|device| device.id != id);
            self.device_ports.remove(&id);
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
        let available = self.available_devices();
        match self.routing.set_route(
            chip,
            source,
            target,
            &self.config.routing.devices,
            &available,
        ) {
            Ok(()) => self.selected_source = None,
            Err(error) => self
                .pending_errors
                .push(error_notification("Routing error", format!("{error:#}"))),
        }
        cx.notify();
    }

    fn routing_view(&self, cx: &mut Context<Self>) -> Div {
        let editable = self.playing.is_none()
            && !self.device_status.values().any(|status| {
                matches!(
                    status,
                    DeviceStatus::Connecting | DeviceStatus::Disconnecting
                )
            });
        let graph = patchbay::build(
            &self.config.routing.devices,
            &self.device_status,
            self.routing.connections(),
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
                        .child("Sequencer"),
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
                        .child("Emulation"),
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
                .left(px(patchbay::SOURCE_X - 4.))
                .top(px(y))
                .w(px(patchbay::SOURCE_WIDTH + 8.))
                .h(px(patchbay::ROW))
                .flex()
                .items_center()
                .justify_between()
                .rounded_sm()
                .when(selected, |row| row.bg(rgb(0x304755)))
                .child(div().size(px(8.)).rounded_full().bg(rgb(if selected {
                    0x7de0d6
                } else {
                    0x6a8e9c
                })))
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
            let silent = matches!(
                source,
                patchbay::Source::Hardware(routing::HardwareChip::Ay8910, _)
            );
            let target_label = if silent {
                format!("{} (silent)", source.label())
            } else {
                source.label()
            };
            let mut target_node = div()
                .id(SharedString::from(format!("emulator-{index}")))
                .absolute()
                .left(px(patchbay::TARGET_X + 8.))
                .top(px(y))
                .w(px(patchbay::TARGET_WIDTH - 4.))
                .h(px(patchbay::ROW))
                .flex()
                .items_center()
                .justify_between()
                .rounded_sm()
                .child(
                    div()
                        .text_sm()
                        .when(silent, |label| label.text_color(rgb(0x9aabb5)))
                        .child(target_label),
                )
                .child(div().size(px(8.)).rounded_full().bg(rgb(0x6a8e9c)))
                .when(silent, |node| {
                    node.tooltip(|window, cx| {
                        Tooltip::new("AY8910 has no software emulation").build(window, cx)
                    })
                });
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
            let id = device.id;
            let status = self.device_status.get(&id);
            let busy = matches!(
                status,
                Some(DeviceStatus::Connecting | DeviceStatus::Disconnecting)
            );
            let can_remove =
                can_remove_device(id, &self.config.routing.devices, &self.device_status);
            let port = &self
                .device_ports
                .get(&id)
                .expect("each device has a port selector")
                .select;
            let selected_port = port.read(cx).selected_value().is_some();
            let (connection_icon, connection_label) = connection_action(status);
            let status_label = match status {
                Some(DeviceStatus::Connecting) => "Connecting...",
                Some(DeviceStatus::Connected(_)) => "Connected",
                Some(DeviceStatus::Disconnecting) => "Disconnecting...",
                None => "Disconnected",
            };
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
                            .p_2()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_ellipsis()
                                            .child(device.name.clone()),
                                    )
                                    .when(can_remove, |row| {
                                        row.child(
                                            Button::new(("remove-device", id as usize))
                                                .label("×")
                                                .disabled(status.is_some() || !editable)
                                                .on_click(cx.listener(move |view, _, _, cx| {
                                                    view.remove_device(id, cx)
                                                })),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .mt_1()
                                    .gap_1()
                                    .child(
                                        div().flex_1().min_w_0().child(
                                            Select::new(port)
                                                .placeholder("Select serial port")
                                                .w_full()
                                                .disabled(device.connected || busy || !editable),
                                        ),
                                    )
                                    .child(
                                        Button::new(("scan-ports", id as usize))
                                            .icon(assets::IconName::RefreshCw)
                                            .w(px(28.))
                                            .h(px(28.))
                                            .p_0()
                                            .tooltip(if self.scanning_ports {
                                                "Searching ports..."
                                            } else {
                                                "Search ports"
                                            })
                                            .accessibility_label("Search ports")
                                            .disabled(self.scanning_ports)
                                            .on_click(
                                                cx.listener(|view, _, _, cx| {
                                                    view.refresh_ports(cx)
                                                }),
                                            ),
                                    )
                                    .child(
                                        Button::new(("connect-device", id as usize))
                                            .icon(connection_icon)
                                            .w(px(28.))
                                            .h(px(28.))
                                            .p_0()
                                            .tooltip(connection_label)
                                            .accessibility_label(connection_label)
                                            .disabled(
                                                busy || !editable
                                                    || (!device.connected && !selected_port),
                                            )
                                            .on_click(cx.listener(move |view, _, _, cx| {
                                                view.connect_device(id, cx)
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(if device.connected {
                                        0x81d88a
                                    } else {
                                        0xa6b6c3
                                    }))
                                    .child(status_label),
                            ),
                    )
                    .into_any_element(),
            );
            for (index, label) in device.extra_labels.iter().enumerate() {
                nodes.push(
                    div()
                        .absolute()
                        .left(px(patchbay::DEVICE_X + 8.))
                        .top(px(device.top
                            + patchbay::DEVICE_HEADER
                            + patchbay::ROW * (device.endpoints.len() + index) as f32))
                        .w(px(patchbay::DEVICE_WIDTH - 16.))
                        .h(px(patchbay::ROW))
                        .text_sm()
                        .text_color(rgb(0xf2a968))
                        .child(label.clone())
                        .into_any_element(),
                );
            }
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
            .child(div().flex().items_center().gap_2()
                .child(div().text_lg().child("ROUTING CONNECTIONS"))
                .when(self.config.routing.devices.last().is_some_and(|device| matches!(self.device_status.get(&device.id), Some(DeviceStatus::Connected(_)))), |row| {
                    row.child(Button::new("add-device-box").label("Add NanoDrive")
                        .disabled(!editable)
                        .on_click(cx.listener(|view, _, window, cx| view.add_empty_device(window, cx))))
                }))
            .child(div().text_sm().text_color(rgb(0xa6b6c3)).child(
                "Connected devices are routed automatically. Select a sequencer source, then a matching target to change it. Hardware timing is best-effort."))
            .when(!editable, |view| view.child(div().text_sm().text_color(rgb(0xf2a968)).child(
                if self.playing.is_some() { "Stop playback to edit connections." } else { "Wait for the connection operation to finish." }
            )))
            .child(div().id("patchbay-scroll").w_full().h(px(graph.height)).overflow_x_scrollbar()
                .child(div().relative().w(px(patchbay::WIDTH)).h(px(graph.height))
                    .child(canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            for cable in &cables {
                                let start = point(bounds.origin.x + px(cable.source_x),
                                    bounds.origin.y + px(cable.source_y));
                                let end = point(bounds.origin.x + px(cable.target_x),
                                    bounds.origin.y + px(cable.target_y));
                                let mut path = PathBuilder::stroke(px(if cable.hardware { 2.5 } else { 1.5 }));
                                if !cable.available { path = path.dash_array(&[px(6.), px(4.)]); }
                                path.move_to(start);
                                let control_offset = (end.x - start.x) * 0.45;
                                path.cubic_bezier_to(end, point(start.x + control_offset, start.y),
                                    point(end.x - control_offset, end.y));
                                if let Ok(path) = path.build() {
                                    window.paint_path(path, rgb(if !cable.available { 0xf2a968 }
                                        else if cable.hardware { ACCENT_COLOR } else { 0x466370 }));
                                }
                            }
                        }
                    ).absolute().top_0().left_0().w(px(patchbay::WIDTH)).h(px(graph.height)))
                    .children(panels)
                    .children(nodes)))
    }

    fn devices_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("device-list")
            .size_full()
            .overflow_y_scrollbar()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .p_4()
                    .gap_3()
                    .child(self.routing_view(cx)),
            )
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
                let unsupported = unsupported_chips(
                    &track.chips,
                    self.routing.connections(),
                    &self.device_status,
                );
                let dimmed = !unsupported.is_empty();
                let unsupported_tooltip = format!(
                    "No software or routed hardware support: {}",
                    unsupported.join(", ")
                );
                let title_tooltip = if dimmed {
                    format!("{title}\n{unsupported_tooltip}")
                } else {
                    title
                };
                let next_path = self
                    .filtered_tracks
                    .get(index + 1)
                    .map(|&next| self.tracks[next].path.as_path());
                let folder_boundary = ends_folder_group(&track.path, next_path);
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
                    .border_color(rgb(if folder_boundary { 0x718c9a } else { 0x303b45 }))
                    .when(selected, |row| row.bg(rgb(0x304755)))
                    .hover(|style| style.bg(rgb(0x293a47)))
                    .when(dimmed, |row| {
                        row.tooltip(move |window, cx| {
                            Tooltip::new(unsupported_tooltip.clone()).build(window, cx)
                        })
                    })
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
                                    .child(div().flex_1().min_w_0().child(track_title(
                                        SharedString::from(format!("track-title-{index}")),
                                        &track.path,
                                        &track.metadata,
                                        dimmed,
                                        title_tooltip,
                                    )))
                                    .when(!track.chips.is_empty(), |row| {
                                        row.child(
                                            div()
                                                .id(SharedString::from(format!("chips-{index}")))
                                                .flex()
                                                .flex_shrink_0()
                                                .max_w(relative(0.35))
                                                .overflow_hidden()
                                                .items_center()
                                                .gap_1()
                                                .tooltip(move |window, cx| {
                                                    Tooltip::new(chip_list.clone())
                                                        .build(window, cx)
                                                })
                                                .children(
                                                    track
                                                        .chips
                                                        .iter()
                                                        .map(|chip| chip_tag(chip, dimmed)),
                                                ),
                                        )
                                    })
                                    .child(
                                        div()
                                            .w(px(48.))
                                            .flex_shrink_0()
                                            .flex()
                                            .justify_center()
                                            .rounded_md()
                                            .bg(rgb(if dimmed { 0x303d48 } else { 0x4a606d }))
                                            .text_color(rgb(if dimmed {
                                                0x8c9ca6
                                            } else {
                                                0xe7f1f3
                                            }))
                                            .text_xs()
                                            .whitespace_nowrap()
                                            .child(music_format(&track.path)),
                                    ),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .text_color(rgb(if dimmed { 0x778893 } else { 0xa6b6c3 }))
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
                        let add_view = view.clone();
                        let copy_view = view.clone();
                        let copy_path = menu_path.clone();
                        menu.item(PopupMenuItem::new("Add selected to playlist").on_click(
                            move |_, _, cx| {
                                let _ =
                                    add_view.update(cx, |view, cx| view.add_selected_tracks(cx));
                            },
                        ))
                        .item(
                            PopupMenuItem::new("Copy full path").on_click(move |_, window, cx| {
                                match clipboard_track_path(&copy_path) {
                                    Ok(path) => {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            path.to_owned(),
                                        ));
                                        let _ = copy_view.update(cx, |view, cx| {
                                            view.status = "Copied full path".into();
                                            cx.notify();
                                        });
                                    }
                                    Err(error) => {
                                        window.push_notification(
                                            error_notification("Cannot copy path", error),
                                            cx,
                                        );
                                    }
                                }
                            }),
                        )
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

    fn move_track_selection(&mut self, down: bool) {
        let current = self.track_selection_order.last().and_then(|path| {
            self.filtered_tracks
                .iter()
                .position(|&index| &self.tracks[index].path == path)
        });
        let Some(index) = list_selection_step(current, self.filtered_tracks.len(), down) else {
            return;
        };
        let path = self.tracks[self.filtered_tracks[index]].path.clone();
        select_item(&mut self.selected_tracks, path.clone(), false);
        self.track_anchor = Some(path.clone());
        self.update_track_selection_order(Some(&path));
        self.library_scroll
            .scroll_to_item(index, ScrollStrategy::Nearest);
    }

    fn update_track_selection_order(&mut self, touched: Option<&PathBuf>) {
        update_selection_order(
            &mut self.track_selection_order,
            &self.selected_tracks,
            self.filtered_tracks
                .iter()
                .map(|&index| self.tracks[index].path.clone()),
            touched,
        );
        self.details_selection = DetailsSelection::Music;
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
        self.update_queue_selection_order(None);
    }

    fn move_queue_selection(&mut self, down: bool) {
        let current = self.queue_selection_order.last().copied();
        let Some(index) = list_selection_step(current, self.config.playlist.len(), down) else {
            return;
        };
        self.select_queue_row(index);
    }

    fn select_queue_row(&mut self, index: usize) {
        select_item(&mut self.selected_queue, index, false);
        self.queue_anchor = Some(index);
        self.update_queue_selection_order(Some(index));
        self.queue_scroll
            .scroll_to_item(index, ScrollStrategy::Nearest);
    }

    fn delete_queue_selection(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_queue.iter().min().copied() else {
            return;
        };
        self.remove_queue_selection(false, cx);
        if !self.config.playlist.is_empty() {
            self.select_queue_row(index.min(self.config.playlist.len() - 1));
        }
    }

    fn update_queue_selection_order(&mut self, touched: Option<usize>) {
        update_selection_order(
            &mut self.queue_selection_order,
            &self.selected_queue,
            0..self.config.playlist.len(),
            touched.as_ref(),
        );
        self.details_selection = DetailsSelection::Playlist;
    }

    fn queue_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        range
            .map(|index| {
                let path = &self.config.playlist[index];
                let track = self
                    .track_by_path
                    .get(path)
                    .map(|&track_index| &self.tracks[track_index])
                    .or_else(|| self.playlist_tracks.get(path));
                let fallback_metadata = TrackMetadata::default();
                let metadata = track.map_or(&fallback_metadata, |track| &track.metadata);
                let unsupported = track
                    .map(|track| {
                        unsupported_chips(
                            &track.chips,
                            self.routing.connections(),
                            &self.device_status,
                        )
                    })
                    .unwrap_or_default();
                let unsupported_tooltip = format!(
                    "No software or routed hardware support: {}",
                    unsupported.join(", ")
                );
                let title = Track::format_display_title(path, metadata);
                let artist = metadata.artist.as_deref().unwrap_or_default().to_owned();
                let playing = self.playing == Some(index);
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
                    .border_l_2()
                    .border_color(rgb(if playing { ACCENT_COLOR } else { 0x17212b }))
                    .when(playing, |row| row.bg(rgb(0x223e40)))
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
                            view.update_queue_selection_order(Some(index));
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .w(px(40.))
                            .flex_shrink_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_sm()
                            .text_color(rgb(if playing { 0x7de0d6 } else { 0x8195a3 }))
                            .when(playing, |marker| {
                                marker
                                    .child(gpui_kit::component::Icon::from(assets::IconName::Play))
                            })
                            .when(!playing, |marker| marker.child(format!("{:02}", index + 1))),
                    )
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
                                    .gap_1()
                                    .min_w_0()
                                    .child(track_title(
                                        SharedString::from(format!("queue-title-{index}")),
                                        path,
                                        metadata,
                                        false,
                                        title,
                                    ))
                                    .when(!unsupported.is_empty(), |row| {
                                        row.child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "queue-warning-{index}"
                                                )))
                                                .size(px(16.))
                                                .flex_shrink_0()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded_full()
                                                .bg(rgb(0xc94343))
                                                .text_color(rgb(0xffffff))
                                                .text_xs()
                                                .child("!")
                                                .tooltip(move |window, cx| {
                                                    Tooltip::new(unsupported_tooltip.clone())
                                                        .build(window, cx)
                                                }),
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
                                    .child(artist),
                            ),
                    )
                    .context_menu(move |menu, _, cx| {
                        let _ = view.update(cx, |view, cx| {
                            if select_for_context(&mut view.selected_queue, index) {
                                view.queue_anchor = Some(index);
                            }
                            view.update_queue_selection_order(Some(index));
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

fn renumber_connected_devices(
    devices: &mut [routing::Device],
    models: &HashMap<u64, String>,
) -> bool {
    let mut counts = HashMap::<&str, usize>::new();
    let mut used = HashSet::new();
    let mut changed = false;
    for device in devices {
        if let Some(model) = models.get(&device.id) {
            let count = counts.entry(model).or_default();
            let name = loop {
                *count += 1;
                let candidate = if *count == 1 {
                    model.clone()
                } else {
                    format!("{model} #{}", *count)
                };
                if !used.contains(&candidate)
                    && (candidate == *model || !models.values().any(|other| other == &candidate))
                {
                    break candidate;
                }
            };
            used.insert(name.clone());
            if device.name != name {
                device.name = name;
                changed = true;
            }
        }
    }
    changed
}

fn details_target<'a>(
    playing: Option<usize>,
    playlist: &'a [PathBuf],
    selected_music: &'a [PathBuf],
    selected_queue: &[usize],
    selection: DetailsSelection,
) -> Option<&'a PathBuf> {
    let music = || selected_music.last();
    let queue = || selected_queue.last().and_then(|index| playlist.get(*index));
    playing
        .and_then(|index| playlist.get(index))
        .or_else(|| match selection {
            DetailsSelection::Music => music().or_else(queue),
            DetailsSelection::Playlist => queue().or_else(music),
        })
}

fn update_selection_order<T: Clone + Eq + Hash>(
    order: &mut Vec<T>,
    selected: &HashSet<T>,
    visible: impl IntoIterator<Item = T>,
    touched: Option<&T>,
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
                if selected.contains(&path) && seen.insert(path.clone()) {
                    order.push(path);
                }
            }
        }
    }
    if let Some(path) = touched.filter(|path| selected.contains(*path)) {
        order.retain(|entry| entry != path);
        order.push(path.clone());
    }
}

fn unsupported_chips(
    chips: &[ChipTag],
    connections: &[routing::Connection],
    statuses: &HashMap<u64, DeviceStatus>,
) -> Vec<String> {
    chips
        .iter()
        .filter(|tag| {
            if tag.software_supported() {
                return false;
            }
            let Some(chip) = routing::HardwareChip::ALL
                .into_iter()
                .find(|chip| chip.name() == tag.name)
            else {
                return true;
            };
            let Some(instances) = routing::ChipInstance::ALL.get(..tag.instances) else {
                return true;
            };
            if instances.is_empty() {
                return true;
            }
            !instances.iter().all(|source| {
                connections.iter().any(|connection| {
                    connection.chip == chip
                        && connection.source == *source
                        && matches!(
                            statuses.get(&connection.device_id),
                            Some(DeviceStatus::Connected(entries))
                                if chip.advertised(connection.target, entries)
                        )
                })
            })
        })
        .map(ChipTag::label)
        .collect()
}

fn has_connected_nanodrive(statuses: &HashMap<u64, DeviceStatus>) -> bool {
    statuses
        .values()
        .any(|status| matches!(status, DeviceStatus::Connected(_)))
}

fn nanodrive_chips(statuses: &HashMap<u64, DeviceStatus>) -> HashSet<&'static str> {
    routing::HardwareChip::ALL
        .into_iter()
        .filter(|chip| {
            statuses.values().any(|status| {
                if let DeviceStatus::Connected(entries) = status {
                    routing::ChipInstance::ALL
                        .into_iter()
                        .any(|instance| chip.advertised(instance, entries))
                } else {
                    false
                }
            })
        })
        .map(routing::HardwareChip::name)
        .collect()
}

fn capability_labels(entries: &[ndsif::CapabilityEntry]) -> Vec<String> {
    let mut labels: Vec<_> = entries
        .iter()
        .map(|entry| {
            let name = match entry.write_id {
                0x30 | 0x50 => "SN76489",
                0x51 => "YM2413",
                0x52 | 0x53 => "YM2612",
                0x54 => "YM2151",
                0x55 | 0xa5 => "YM2203",
                0x56 => "YM2608",
                0x58 => "YM2610/B",
                0x5a => "YM3812",
                0x5b => "YM3526",
                0x5c => "Y8950",
                0x5e | 0x5f => "YMF262",
                0xa0 => "AY8910",
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
    nanodrive_connected: bool,
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
        let matches: Vec<_> = ["MDX", "VGM"]
            .into_iter()
            .chain(nanodrive_connected.then_some("NanoDrive"))
            .chain(chips.iter().copied())
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

#[derive(Debug, PartialEq, Eq)]
enum ListKey {
    SelectAll,
    Up,
    Down,
    Add,
    Delete,
}

fn list_key(keystroke: &Keystroke, list: DetailsSelection) -> Option<ListKey> {
    if is_select_all(keystroke) {
        return Some(ListKey::SelectAll);
    }
    let modifiers = keystroke.modifiers;
    if modifiers.control
        || modifiers.platform
        || modifiers.alt
        || modifiers.shift
        || modifiers.function
    {
        return None;
    }
    match (keystroke.key.as_str(), list) {
        ("up", _) => Some(ListKey::Up),
        ("down", _) => Some(ListKey::Down),
        ("enter", DetailsSelection::Music) => Some(ListKey::Add),
        ("delete", DetailsSelection::Playlist) => Some(ListKey::Delete),
        _ => None,
    }
}

fn list_selection_step(current: Option<usize>, len: usize, down: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match current.filter(|&index| index < len) {
        Some(index) if down => Some(index.saturating_add(1).min(len - 1)),
        Some(index) => Some(index.saturating_sub(1)),
        None if down => Some(0),
        None => Some(len - 1),
    }
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
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    use gpui_kit::Keystroke;
    use gpui_kit::component::input::{InlineToken, InputContent};

    use super::{
        ACCENT_COLOR, DetailsSelection, DeviceStatus, ListKey, ScanOrigin, adjacent_track,
        can_remove_device, capability_labels, chip_colors, chip_suggestions, clipboard_track_path,
        connection_action, details_target, ends_folder_group, inclusive_range,
        initial_window_bounds, is_select_all, keep_preview_sources, list_key, list_selection_step,
        music_format, notification_copy_text, remove_playlist_entries, renumber_connected_devices,
        reported_scan_errors, scan_progress_text, search_terms, select_for_context, select_item,
        select_range, selection_status, unsupported_chips, update_selection_order,
    };

    #[test]
    fn notification_copy_text_includes_full_message_and_details() {
        assert_eq!(
            notification_copy_text(
                "Playback error",
                "NanoDrive device 1: error\n原因",
                Some("first path\nsecond path")
            ),
            "Playback error\nNanoDrive device 1: error\n原因\nfirst path\nsecond path"
        );
        assert_eq!(
            notification_copy_text("Library scan complete", "12 tracks found", None),
            "Library scan complete\n12 tracks found"
        );
        assert_eq!(
            notification_copy_text("Error", "message", Some("")),
            "Error\nmessage"
        );
    }

    #[test]
    fn clipboard_track_path_preserves_absolute_unicode_paths() {
        let path = std::env::temp_dir().join("Music").join("曲.vgz");
        assert_eq!(clipboard_track_path(&path), Ok(path.to_str().unwrap()));
        assert!(clipboard_track_path(std::path::Path::new("Music/track.vgm")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn clipboard_track_path_rejects_non_utf8_paths() {
        use std::os::unix::ffi::OsStrExt;
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(b"/music/\xff.vgm"));
        assert!(clipboard_track_path(path).is_err());
    }

    #[test]
    fn connection_action_reflects_device_state() {
        use gpui_kit::assets::IconName;

        assert!(matches!(
            connection_action(None),
            (IconName::Plug, "Connect")
        ));
        assert!(matches!(
            connection_action(Some(&DeviceStatus::Connecting)),
            (IconName::Plug, "Connecting...")
        ));
        assert!(matches!(
            connection_action(Some(&DeviceStatus::Connected(vec![]))),
            (IconName::Unplug, "Disconnect")
        ));
        assert!(matches!(
            connection_action(Some(&DeviceStatus::Disconnecting)),
            (IconName::Unplug, "Disconnecting...")
        ));
    }

    #[test]
    fn scan_errors_are_not_notified_for_background_scans() {
        let progress = super::ScanProgress {
            files: 12,
            errors: 2,
            ..Default::default()
        };
        let errors = vec!["bad track".to_owned(), "missing directory".to_owned()];
        assert_eq!(
            scan_progress_text(ScanOrigin::Background, progress),
            "12 files scanned"
        );
        assert!(reported_scan_errors(ScanOrigin::Background, errors.clone()).is_empty());
        for origin in [ScanOrigin::Add, ScanOrigin::Rescan] {
            assert_eq!(
                scan_progress_text(origin, progress),
                "12 files scanned; 2 errors"
            );
            assert_eq!(reported_scan_errors(origin, errors.clone()), errors);
        }
    }

    #[test]
    fn scan_progress_is_shown_once_and_then_updated_in_place() {
        use super::{ScanProgress, ScanProgressAction, scan_progress_action};
        use std::time::Duration;

        let initial = ScanProgress::default();
        let next = ScanProgress {
            files: 1,
            errors: 0,
        };
        assert_eq!(
            scan_progress_action(false, None, initial, None),
            ScanProgressAction::Show
        );
        assert_eq!(
            scan_progress_action(true, Some(initial), next, Some(Duration::from_millis(500))),
            ScanProgressAction::None
        );
        assert_eq!(
            scan_progress_action(true, Some(initial), next, Some(Duration::from_secs(1))),
            ScanProgressAction::Update
        );
        assert_eq!(
            scan_progress_action(true, Some(next), next, Some(Duration::from_secs(2))),
            ScanProgressAction::None
        );
        assert_eq!(
            scan_progress_action(true, None, initial, None),
            ScanProgressAction::Update
        );
        assert_eq!(
            scan_progress_action(false, None, initial, None),
            ScanProgressAction::Show
        );
    }

    #[test]
    fn unsupported_chips_follow_connected_routes_and_required_instances() {
        use super::{ChipTag, DeviceStatus, routing};
        use routing::{ChipInstance, Connection, HardwareChip};

        let ay = ChipTag {
            name: "AY8910",
            instances: 1,
            ymfm: false,
        };
        let dual_ay = ChipTag {
            instances: 2,
            ..ay.clone()
        };
        let pokey = ChipTag {
            name: "POKEY",
            instances: 1,
            ymfm: false,
        };
        let ym = ChipTag {
            name: "YM2151",
            instances: 1,
            ymfm: true,
        };
        let capability = ndsif::CapabilityEntry {
            write_id: 0xa0,
            instance: 1,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Variable,
            fixed_clock_hz: 0,
        };
        let first = Connection {
            chip: HardwareChip::Ay8910,
            source: ChipInstance::Primary,
            device_id: 1,
            target: ChipInstance::Secondary,
        };
        let mut statuses = HashMap::from([(1, DeviceStatus::Connected(vec![capability]))]);
        assert_eq!(
            unsupported_chips(&[ay.clone(), pokey.clone(), ym], &[], &statuses),
            ["AY8910", "POKEY"]
        );
        assert_eq!(
            unsupported_chips(
                &[ay.clone(), pokey.clone()],
                std::slice::from_ref(&first),
                &statuses
            ),
            ["POKEY"]
        );
        assert_eq!(
            unsupported_chips(
                std::slice::from_ref(&dual_ay),
                std::slice::from_ref(&first),
                &statuses
            ),
            ["AY8910x2"]
        );

        let second = Connection {
            source: ChipInstance::Secondary,
            device_id: 2,
            target: ChipInstance::Primary,
            ..first.clone()
        };
        statuses.insert(
            2,
            DeviceStatus::Connected(vec![ndsif::CapabilityEntry {
                instance: 0,
                ..capability
            }]),
        );
        assert!(
            unsupported_chips(
                std::slice::from_ref(&dual_ay),
                &[first.clone(), second.clone()],
                &statuses
            )
            .is_empty()
        );
        statuses.insert(2, DeviceStatus::Disconnecting);
        assert_eq!(
            unsupported_chips(
                std::slice::from_ref(&dual_ay),
                &[first.clone(), second],
                &statuses
            ),
            ["AY8910x2"]
        );
        statuses.insert(1, DeviceStatus::Connected(vec![]));
        assert_eq!(unsupported_chips(&[ay], &[first], &statuses), ["AY8910"]);
        assert_eq!(unsupported_chips(&[pokey], &[], &statuses), ["POKEY"]);
    }

    #[test]
    fn preview_sources_remain_visible_only_until_playback_sources_arrive() {
        let current: &[super::GainSource] = &[];
        assert!(keep_preview_sources(true, true, Some(current), None));
        assert!(!keep_preview_sources(true, true, Some(current), Some(&[])));
        assert!(!keep_preview_sources(true, true, None, None));
        assert!(!keep_preview_sources(true, false, Some(current), None));
        assert!(!keep_preview_sources(false, true, Some(current), None));
    }

    #[test]
    fn list_shortcuts_are_scoped_to_the_focused_list_and_ignore_modifiers() {
        let key = |value: &'static str, list| list_key(&Keystroke::parse(value).unwrap(), list);
        for list in [DetailsSelection::Music, DetailsSelection::Playlist] {
            assert_eq!(key("up", list), Some(ListKey::Up));
            assert_eq!(key("down", list), Some(ListKey::Down));
            assert_eq!(key("ctrl-a", list), Some(ListKey::SelectAll));
            for modified in ["shift-up", "ctrl-down", "alt-enter", "cmd-delete"] {
                assert_eq!(key(modified, list), None);
            }
        }
        assert_eq!(key("enter", DetailsSelection::Music), Some(ListKey::Add));
        assert_eq!(key("delete", DetailsSelection::Music), None);
        assert_eq!(key("enter", DetailsSelection::Playlist), None);
        assert_eq!(
            key("delete", DetailsSelection::Playlist),
            Some(ListKey::Delete)
        );
    }

    #[test]
    fn list_selection_navigation_handles_empty_and_boundaries() {
        assert_eq!(list_selection_step(None, 0, true), None);
        assert_eq!(list_selection_step(None, 3, true), Some(0));
        assert_eq!(list_selection_step(None, 3, false), Some(2));
        assert_eq!(list_selection_step(Some(1), 3, true), Some(2));
        assert_eq!(list_selection_step(Some(1), 3, false), Some(0));
        assert_eq!(list_selection_step(Some(2), 3, true), Some(2));
        assert_eq!(list_selection_step(Some(0), 3, false), Some(0));
        assert_eq!(list_selection_step(Some(3), 3, true), Some(0));
    }

    #[test]
    fn music_folder_separator_uses_next_visible_track() {
        use std::path::Path;

        let first = Path::new("/music/game/a.vgm");
        let same_folder = Path::new("/music/game/c.vgm");
        let nested_folder = Path::new("/music/game/other/d.vgm");
        let other_folder = Path::new("/music/other/e.vgm");
        assert!(!ends_folder_group(first, Some(same_folder)));
        assert!(ends_folder_group(same_folder, Some(nested_folder)));
        assert!(ends_folder_group(nested_folder, Some(other_folder)));
        assert!(!ends_folder_group(other_folder, None));
    }

    #[test]
    fn music_format_labels_vgm_vgz_and_mdx() {
        for (file, expected) in [
            ("song.vgm", "VGM"),
            ("song.VGZ", "VGM"),
            ("song.mdx", "MDX"),
            ("song.MDX", "MDX"),
        ] {
            assert_eq!(music_format(&PathBuf::from(file)), expected);
        }
    }

    #[test]
    fn initial_window_fits_and_centers_in_visible_area() {
        use gpui_kit::{Bounds, point, px, size};

        let visible = Bounds::new(point(px(0.), px(0.)), size(px(1920.), px(1040.)));
        let bounds = initial_window_bounds(visible);
        assert_eq!(bounds.size, size(px(1600.), px(900.)));
        assert_eq!(bounds.origin, point(px(160.), px(70.)));

        let visible = Bounds::new(point(px(10.), px(20.)), size(px(1100.), px(680.)));
        let bounds = initial_window_bounds(visible);
        assert_eq!(bounds, visible);
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
        update_selection_order(
            &mut order,
            &selected,
            visible.iter().cloned(),
            Some(&visible[0]),
        );
        let target = |playing, order: &[PathBuf]| {
            details_target(playing, &playlist, order, &[], DetailsSelection::Music).cloned()
        };
        assert_eq!(target(None, &order), Some(visible[0].clone()));
        assert_eq!(target(Some(1), &order), Some(playlist[1].clone()));
        selected.remove(&visible[0]);
        update_selection_order(
            &mut order,
            &selected,
            visible.iter().cloned(),
            Some(&visible[0]),
        );
        assert_eq!(target(None, &order), Some(visible[1].clone()));
        selected.insert(visible[2].clone());
        update_selection_order(
            &mut order,
            &selected,
            visible.iter().cloned(),
            Some(&visible[2]),
        );
        assert_eq!(target(None, &order), Some(visible[2].clone()));
        selected.clear();
        update_selection_order(&mut order, &selected, visible.iter().cloned(), None);
        assert_eq!(target(None, &order), None);
        assert_eq!(target(Some(0), &order), Some(playlist[0].clone()));
    }

    #[test]
    fn track_details_follow_latest_selection_in_either_list() {
        let playlist = [
            PathBuf::from("first.vgm"),
            PathBuf::from("second.vgm"),
            PathBuf::from("first.vgm"),
        ];
        let music = [PathBuf::from("music.vgm")];
        let mut selected_queue = HashSet::new();
        let mut queue_order = Vec::new();
        let update_queue = |order: &mut Vec<usize>, selected: &HashSet<usize>, touched| {
            update_selection_order(order, selected, 0..playlist.len(), touched);
        };

        select_item(&mut selected_queue, 2, false);
        update_queue(&mut queue_order, &selected_queue, Some(&2));
        assert_eq!(
            details_target(
                None,
                &playlist,
                &music,
                &queue_order,
                DetailsSelection::Playlist
            ),
            Some(&playlist[2])
        );
        assert_eq!(
            details_target(
                None,
                &playlist,
                &music,
                &queue_order,
                DetailsSelection::Music
            ),
            Some(&music[0])
        );
        assert_eq!(
            details_target(None, &playlist, &[], &queue_order, DetailsSelection::Music),
            Some(&playlist[2])
        );
        assert_eq!(
            details_target(
                Some(1),
                &playlist,
                &music,
                &queue_order,
                DetailsSelection::Music
            ),
            Some(&playlist[1])
        );

        select_range(&mut selected_queue, 0..=2, false);
        update_queue(&mut queue_order, &selected_queue, Some(&0));
        assert_eq!(queue_order.last(), Some(&0));
        select_item(&mut selected_queue, 0, true);
        update_queue(&mut queue_order, &selected_queue, Some(&0));
        assert_eq!(queue_order.last(), Some(&1));
        assert_eq!(
            details_target(
                None,
                &playlist,
                &music,
                &queue_order,
                DetailsSelection::Playlist
            ),
            Some(&playlist[1])
        );
        selected_queue.clear();
        update_queue(&mut queue_order, &selected_queue, None);
        assert_eq!(
            details_target(
                None,
                &playlist,
                &music,
                &queue_order,
                DetailsSelection::Playlist
            ),
            Some(&music[0])
        );
        assert_eq!(
            details_target(
                None,
                &playlist,
                &[],
                &queue_order,
                DetailsSelection::Playlist
            ),
            None
        );
    }

    #[test]
    fn selection_status_follows_latest_list_and_requires_multiple_tracks() {
        assert_eq!(
            selection_status(DetailsSelection::Music, 3, 5).as_deref(),
            Some("Music: 3 selected")
        );
        assert_eq!(
            selection_status(DetailsSelection::Playlist, 3, 5).as_deref(),
            Some("Playlist: 5 selected")
        );
        assert_eq!(selection_status(DetailsSelection::Music, 1, 5), None);
        assert_eq!(selection_status(DetailsSelection::Playlist, 3, 0), None);
    }

    #[test]
    fn device_names_number_only_connected_models_in_box_order() {
        let device = |id, name: &str| crate::routing::Device {
            id,
            name: name.into(),
            port: format!("port{id}"),
            baud_rate: 115_200,
        };
        let mut devices = vec![
            device(1, "NanoDrive8"),
            device(2, "NanoDrive8 #2"),
            device(3, ""),
            device(4, "NanoDrive6"),
        ];
        let mut models = HashMap::from([(3, "NanoDrive8".to_string())]);
        assert!(renumber_connected_devices(&mut devices, &models));
        assert_eq!(devices[2].name, "NanoDrive8");
        assert!(!renumber_connected_devices(&mut devices, &models));
        models.insert(2, "NanoDrive8".into());
        models.insert(4, "NanoDrive6".into());
        assert!(renumber_connected_devices(&mut devices, &models));
        assert_eq!(devices[1].name, "NanoDrive8");
        assert_eq!(devices[2].name, "NanoDrive8 #2");
        assert_eq!(devices[3].name, "NanoDrive6");
        models.remove(&2);
        assert!(renumber_connected_devices(&mut devices, &models));
        assert_eq!(devices[2].name, "NanoDrive8");
        models.insert(1, "NanoDrive8".into());
        assert!(renumber_connected_devices(&mut devices, &models));
        assert_eq!(devices[0].name, "NanoDrive8");
        assert_eq!(devices[2].name, "NanoDrive8 #2");
    }

    #[test]
    fn only_nonterminal_disconnected_boxes_can_be_removed() {
        let device = |id| crate::routing::Device {
            id,
            name: String::new(),
            port: String::new(),
            baud_rate: 115_200,
        };
        let devices = vec![device(1), device(2), device(3)];
        let mut statuses = HashMap::from([(1, DeviceStatus::Connected(vec![]))]);
        assert!(!can_remove_device(1, &devices, &statuses));
        assert!(can_remove_device(2, &devices, &statuses));
        assert!(!can_remove_device(3, &devices, &statuses));
        assert!(!can_remove_device(4, &devices, &statuses));
        statuses.insert(2, DeviceStatus::Connecting);
        assert!(!can_remove_device(2, &devices, &statuses));
        assert!(!can_remove_device(1, &devices[..1], &HashMap::new()));
        statuses.remove(&1);
        assert!(can_remove_device(1, &devices, &statuses));
        let remaining = vec![device(3)];
        assert!(!can_remove_device(3, &remaining, &HashMap::new()));
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
            chip_suggestions("game ym", 7, &[], &chips, false),
            Some((5..7, vec!["YM2151", "YM2612"]))
        );
        assert_eq!(
            chip_suggestions("game Sega P", 11, &[], &chips, false),
            Some((5..11, vec!["Sega PCM"]))
        );
        assert_eq!(
            chip_suggestions("ym2612", 2, &[], &chips, false),
            Some((0..6, vec!["YM2151", "YM2612"]))
        );
        let content = InputContent::new("YM2151ym")
            .with_token(0..6, InlineToken::new("YM2151", "YM2151"))
            .unwrap();
        assert_eq!(
            chip_suggestions(content.text(), 8, content.tokens(), &chips, false),
            Some((6..8, vec!["YM2612"]))
        );
        assert_eq!(
            chip_suggestions(content.text(), 3, content.tokens(), &chips, false),
            None
        );
        assert_eq!(chip_suggestions("音", 1, &[], &chips, false), None);
        assert_eq!(
            chip_suggestions("vg", 2, &[], &chips, false),
            Some((0..2, vec!["VGM"]))
        );
        assert_eq!(
            chip_suggestions("md", 2, &[], &chips, false),
            Some((0..2, vec!["MDX"]))
        );
        let content = InputContent::new("VGMvg")
            .with_token(0..3, InlineToken::new("VGM", "VGM"))
            .unwrap();
        assert_eq!(
            chip_suggestions(content.text(), 5, content.tokens(), &chips, false),
            None
        );
        assert_eq!(chip_suggestions("nano", 4, &[], &chips, false), None);
        assert_eq!(
            chip_suggestions("nano", 4, &[], &chips, true),
            Some((0..4, vec!["NanoDrive"]))
        );
        let content = InputContent::new("NanoDrivenano")
            .with_token(0..9, InlineToken::new("NanoDrive", "NanoDrive"))
            .unwrap();
        assert_eq!(
            chip_suggestions(content.text(), 13, content.tokens(), &chips, true),
            None
        );
    }

    #[test]
    fn nanodrive_filter_uses_connected_advertised_chip_types() {
        use super::{has_connected_nanodrive, nanodrive_chips};

        let capability = ndsif::CapabilityEntry {
            write_id: 0xa0,
            instance: 1,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Variable,
            fixed_clock_hz: 0,
        };
        let mut statuses = HashMap::from([
            (1, DeviceStatus::Connected(vec![capability])),
            (
                2,
                DeviceStatus::Connected(vec![ndsif::CapabilityEntry {
                    write_id: 0x54,
                    instance: 0,
                    ..capability
                }]),
            ),
            (3, DeviceStatus::Connecting),
        ]);
        assert!(has_connected_nanodrive(&statuses));
        assert_eq!(
            nanodrive_chips(&statuses),
            HashSet::from(["AY8910", "YM2151"])
        );
        statuses.insert(1, DeviceStatus::Disconnecting);
        assert_eq!(nanodrive_chips(&statuses), HashSet::from(["YM2151"]));
        statuses.remove(&2);
        assert!(!has_connected_nanodrive(&statuses));
        assert!(nanodrive_chips(&statuses).is_empty());
        statuses.insert(1, DeviceStatus::Connected(vec![]));
        assert!(has_connected_nanodrive(&statuses));
        assert!(nanodrive_chips(&statuses).is_empty());
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
        if self.page == Page::Piano && self.playing.is_some() && !self.paused {
            window.request_animation_frame();
        }
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
                                div()
                                    .relative()
                                    .child(
                                        Button::new("piano-icon")
                                            .icon(assets::IconName::Piano)
                                            .tooltip("Piano roll")
                                            .accessibility_label("Piano roll")
                                            .toggled(self.page == Page::Piano)
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.page = Page::Piano;
                                                cx.notify();
                                            })),
                                    )
                                    .when(self.playing.is_some(), |node| {
                                        node.child(
                                            div()
                                                .absolute()
                                                .top_0()
                                                .right_0()
                                                .size(px(8.))
                                                .rounded_full()
                                                .bg(rgb(ACCENT_COLOR)),
                                        )
                                    }),
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
                            .when(self.page == Page::Piano, |container| {
                                let snapshot = if self.playing.is_some() {
                                    self.player
                                        .visual_snapshot_with_preview(self.roll_mode, self.transition_preview.as_ref())
                                        .map_err(|error| error.to_string())
                                } else if let Some(error) = &self.idle_preview_error {
                                    Err(format!("piano roll preview: {error}"))
                                } else if let Some(preview) = self.current_idle_preview() {
                                    preview
                                        .visual_snapshot(self.roll_mode)
                                        .map_err(|error| error.to_string())
                                } else {
                                    Ok(visualizer::Snapshot::default())
                                };
                                if (self.paused || self.playing.is_none())
                                    && snapshot.as_ref().is_ok_and(|snapshot| snapshot.scanning)
                                {
                                    window.request_animation_frame();
                                }
                                container.child(
                                    div()
                                        .relative()
                                        .flex()
                                        .size_full()
                                        .child(
                                            div()
                                                .relative()
                                                .flex_1()
                                                .min_w_0()
                                                .h_full()
                                                .child(visualizer::view(
                                                    self.piano_title(),
                                                    self.playing.is_some(),
                                                    self.playing.is_none() && !self.config.playlist.is_empty(),
                                                    self.paused,
                                                    self.roll_mode,
                                                    self.roll_hidden_channels.clone(),
                                                    snapshot,
                                                    self.playback_buttons(cx),
                                                ))
                                                .child(
                                                    div()
                                                        .absolute()
                                                        .bottom(px(16.))
                                                        .right(px(28.))
                                                        .flex()
                                                        .gap_2()
                                                        .child(
                                                            Button::new("piano-incoming")
                                                                .label("Incoming")
                                                                .when(
                                                                    self.roll_mode == visualizer::RollMode::Incoming,
                                                                    |button| button.primary(),
                                                                )
                                                                .toggled(self.roll_mode == visualizer::RollMode::Incoming)
                                                                .on_click(cx.listener(|view, _, _, cx| {
                                                                    view.roll_mode = visualizer::RollMode::Incoming;
                                                                    cx.notify();
                                                                })),
                                                        )
                                                        .child(
                                                            Button::new("piano-trail")
                                                                .label("Trail")
                                                                .when(
                                                                    self.roll_mode == visualizer::RollMode::Trail,
                                                                    |button| button.primary(),
                                                                )
                                                                .toggled(self.roll_mode == visualizer::RollMode::Trail)
                                                                .on_click(cx.listener(|view, _, _, cx| {
                                                                    view.roll_mode = visualizer::RollMode::Trail;
                                                                    cx.notify();
                                                                })),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .absolute()
                                                        .top(px(24.))
                                                        .right(px(28.))
                                                        .bottom(px(64.))
                                                        .w(px(260.))
                                                        .flex()
                                                        .flex_col()
                                                        .gap_2()
                                                        .child(self.piano_gain_panel(cx))
                                                        .child(self.piano_channel_panel(cx)),
                                                )
                                        )
                                        ,
                                )
                            })
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
                                                        .flex_col()
                                                        .flex_1()
                                                        .min_h_0()
                                                        .overflow_hidden()
                                                        .when(self.config.directories.is_empty(), |area| {
                                                            area.justify_end().child(
                                                                div()
                                                                    .flex()
                                                                    .flex_col()
                                                                    .items_center()
                                                                    .flex_shrink_0()
                                                                    .child(
                                                                        Bubble::new()
                                                                            .content(
                                                                                BubbleContent::new()
                                                                                    .bg(rgb(0x674080))
                                                                                    .text_color(rgb(0xffffff)),
                                                                            )
                                                                            .w_full()
                                                                            .max_w_full()
                                                                            .child("Add a music folder."),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .text_color(rgb(0xc792ea))
                                                                            .child(Icon::new(assets::IconName::ArrowDown).size_4()),
                                                                    ),
                                                            )
                                                        })
                                                        .when(!self.config.directories.is_empty(), |area| {
                                                            area.child(
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
                                                        }),
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
                                                            view.scan(ScanOrigin::Rescan, cx)
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
                                                        |view, event: &KeyDownEvent, window, cx| {
                                                            if !view.track_focus.is_focused(window) {
                                                                return;
                                                            }
                                                            match list_key(&event.keystroke, DetailsSelection::Music) {
                                                                Some(ListKey::SelectAll) => view.select_all_tracks(),
                                                                Some(ListKey::Up) => view.move_track_selection(false),
                                                                Some(ListKey::Down) => view.move_track_selection(true),
                                                                Some(ListKey::Add) if !view.selected_tracks.is_empty() => view.add_selected_tracks(cx),
                                                                _ => return,
                                                            }
                                                            cx.stop_propagation();
                                                            cx.notify();
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
                                                            |view, event: &KeyDownEvent, window, cx| {
                                                                if !view.queue_focus.is_focused(window) {
                                                                    return;
                                                                }
                                                                match list_key(&event.keystroke, DetailsSelection::Playlist) {
                                                                    Some(ListKey::SelectAll) => view.select_all_queue(),
                                                                    Some(ListKey::Up) => view.move_queue_selection(false),
                                                                    Some(ListKey::Down) => view.move_queue_selection(true),
                                                                    Some(ListKey::Delete) => view.delete_queue_selection(cx),
                                                                    _ => return,
                                                                }
                                                                cx.stop_propagation();
                                                                cx.notify();
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
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .bg(rgb(0x243441))
                    .text_sm()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(self.status.clone()),
                    )
                    .when_some(
                        selection_status(
                            self.details_selection,
                            self.selected_tracks.len(),
                            self.selected_queue.len(),
                        ),
                        |bar, status| {
                            bar.child(div().flex_shrink_0().text_color(rgb(0xa6b6c3)).child(status))
                        },
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(rgb(0xa6b6c3))
                            .child(concat!("v", env!("CARGO_PKG_VERSION"))),
                    )
                    .when_some(self.update_available.clone(), |bar, version| {
                        let url = format!(
                            "https://github.com/h1romas4/chipstream/releases/tag/nddrv-v{version}"
                        );
                        let blue = ButtonCustomVariant::new(cx)
                            .color(rgb(0x0078d4).into())
                            .foreground(rgb(0xffffff).into())
                            .hover(rgb(0x006cbe).into())
                            .active(rgb(0x005a9e).into());
                        bar.child(
                            Button::new("update-available")
                                .label(format!("Update available: v{version}"))
                                .custom(blue)
                                .on_click(move |_, _, cx| cx.open_url(&url)),
                        )
                    })
                    .child(
                        Button::new("settings")
                            .icon(assets::IconName::Settings)
                            .ghost()
                            .xsmall()
                            .tooltip("Settings")
                            .accessibility_label("Settings")
                            .on_click(cx.listener(|view, _, window, cx| view.open_settings(window, cx))),
                    ),
            )
    }
}

fn main() {
    application().with_assets(assets::AllAssets).run(|cx| {
        init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        Theme::global_mut(cx).notification.placement = Anchor::BottomRight;
        let bounds = cx
            .primary_display()
            .map(|display| initial_window_bounds(display.visible_bounds()))
            .unwrap_or_else(|| Bounds::new(point(px(0.), px(0.)), INITIAL_WINDOW_SIZE));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_decorations: Some(WindowDecorations::Client),
            ..TitleBar::window_options()
        };
        open_window(options, cx, |window, cx| {
            window.set_window_title(APP_TITLE);
            cx.new(|cx| {
                let mut app = AppView::new(window, cx);
                app.check_for_updates(cx);
                if !app.config.directories.is_empty() {
                    app.scan(ScanOrigin::Background, cx);
                } else {
                    app.load_playlist_metadata(ScanOrigin::Background, cx);
                }
                app
            })
        })
        .expect("failed to open nddrv window");
    });
}
