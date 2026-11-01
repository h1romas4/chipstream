use std::collections::{HashMap, VecDeque};

use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use soundlog::chip::event::StateEvent;
use soundlog::vgm::command::Instance;

const MAX_NOTES: usize = 8192;
pub const WINDOW_SECONDS: u64 = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RollMode {
    #[default]
    Incoming,
    Trail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Voice {
    chip: u8,
    instance: u8,
    channel: u8,
}

#[derive(Clone, Debug)]
pub struct Note {
    pub pitch: u8,
    pub chip: u8,
    pub channel: u8,
    pub start: u64,
    pub end: Option<u64>,
}

#[derive(Default)]
pub struct Snapshot {
    pub position: u64,
    pub rate: u32,
    pub notes: Vec<Note>,
    pub scanned: u64,
    pub incomplete: bool,
    pub started: bool,
    pub preparing: bool,
    pub scanning: bool,
}

#[derive(Default)]
pub struct PianoRoll {
    active: HashMap<Voice, Note>,
    finished: VecDeque<Note>,
    incomplete: bool,
}

impl PianoRoll {
    pub fn apply(&mut self, chip: u8, instance: Instance, frame: usize, events: Vec<StateEvent>) {
        let frame = frame as u64;
        for event in events {
            let channel = match &event {
                StateEvent::KeyOn { channel, .. }
                | StateEvent::KeyOff { channel }
                | StateEvent::ToneChange { channel, .. } => *channel,
            };
            let voice = Voice {
                chip,
                instance: instance as u8,
                channel,
            };
            match event {
                StateEvent::KeyOff { .. } => self.end(voice, frame),
                StateEvent::KeyOn { tone, .. } => {
                    self.end(voice, frame);
                    if let Some(pitch) = tone.freq_hz.and_then(frequency_to_pitch) {
                        self.begin(voice, pitch, frame);
                    }
                }
                StateEvent::ToneChange { tone, .. } => {
                    let pitch = tone.freq_hz.and_then(frequency_to_pitch);
                    if self.active.get(&voice).map(|note| note.pitch) != pitch {
                        self.end(voice, frame);
                        if let Some(pitch) = pitch {
                            self.begin(voice, pitch, frame);
                        }
                    }
                }
            }
        }
    }

    fn begin(&mut self, voice: Voice, pitch: u8, frame: u64) {
        self.active.insert(
            voice,
            Note {
                pitch,
                chip: voice.chip,
                channel: voice.channel,
                start: frame,
                end: None,
            },
        );
    }

    fn end(&mut self, voice: Voice, frame: u64) {
        if let Some(mut note) = self.active.remove(&voice) {
            note.end = Some(frame.max(note.start));
            self.finished.push_back(note);
            while self.finished.len() > MAX_NOTES {
                self.finished.pop_front();
                self.incomplete = true;
            }
        }
    }

    pub fn report_overflow(&mut self) {
        self.active.clear();
        self.finished.clear();
        self.incomplete = true;
    }

    pub fn incomplete(&self) -> bool {
        self.incomplete
    }

    pub fn finish(&mut self, frame: u64) {
        let voices: Vec<_> = self.active.keys().copied().collect();
        for voice in voices {
            self.end(voice, frame);
        }
    }

    pub fn prune(&mut self, position: u64, rate: u32) {
        let cutoff = position.saturating_sub(u64::from(rate) * WINDOW_SECONDS);
        self.finished
            .retain(|note| note.end.is_some_and(|end| end >= cutoff));
    }

    pub fn snapshot(&mut self, position: u64, rate: u32, mode: RollMode) -> Vec<Note> {
        let window = u64::from(rate) * WINDOW_SECONDS;
        let cutoff = position.saturating_sub(window);
        let limit = match mode {
            RollMode::Incoming => position.saturating_add(window),
            RollMode::Trail => position,
        };
        self.prune(position, rate);
        self.finished
            .iter()
            .chain(self.active.values())
            .filter(|note| {
                note.start <= limit
                    && note.end.is_none_or(|end| {
                        end >= if mode == RollMode::Incoming {
                            position
                        } else {
                            cutoff
                        }
                    })
            })
            .cloned()
            .collect()
    }
}

fn frequency_to_pitch(frequency: f32) -> Option<u8> {
    if !frequency.is_finite() || frequency <= 0.0 {
        return None;
    }
    let pitch = 69.0 + 12.0 * (frequency / 440.0).log2();
    if !(0.0..=127.0).contains(&pitch) {
        return None;
    }
    Some(pitch.round() as u8)
}

fn is_black(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

fn key_position(pitch: u8) -> Option<(f32, f32)> {
    if !(21..=108).contains(&pitch) {
        return None;
    }
    let before = (21..pitch).filter(|pitch| !is_black(*pitch)).count() as f32;
    let black = is_black(pitch);
    Some((
        (before + if black { 0.0 } else { 0.5 }) / 52.0,
        if black { 0.60 / 52.0 } else { 0.90 / 52.0 },
    ))
}

fn polygon(window: &mut Window, points: &[(f32, f32)], color: Rgba) {
    if points.len() < 3 {
        return;
    }
    let mut path = PathBuilder::fill();
    path.move_to(point(px(points[0].0), px(points[0].1)));
    for &(x, y) in &points[1..] {
        path.line_to(point(px(x), px(y)));
    }
    path.close();
    match path.build() {
        Ok(path) => window.paint_path(path, color),
        Err(error) => eprintln!("Could not draw piano roll: {error}"),
    }
}

fn line(window: &mut Window, points: &[(f32, f32)], width: f32, color: Rgba) {
    if points.len() < 2 {
        return;
    }
    let mut path = PathBuilder::stroke(px(width));
    path.move_to(point(px(points[0].0), px(points[0].1)));
    for &(x, y) in &points[1..] {
        path.line_to(point(px(x), px(y)));
    }
    match path.build() {
        Ok(path) => window.paint_path(path, color),
        Err(error) => eprintln!("Could not draw piano roll grid: {error}"),
    }
}

fn note_depths(
    note: &Note,
    position: u64,
    rate: u32,
    mode: RollMode,
    scanned: u64,
) -> Option<(f32, f32)> {
    if rate == 0 {
        return None;
    }
    let duration = u64::from(rate) * WINDOW_SECONDS;
    let (near, far) = match mode {
        RollMode::Trail => {
            if note.start > position {
                return None;
            }
            let end = note.end.unwrap_or(position).min(position);
            (
                position.saturating_sub(end) as f32 / duration as f32,
                position.saturating_sub(note.start) as f32 / duration as f32,
            )
        }
        RollMode::Incoming => {
            if note.end.is_some_and(|end| end < position) {
                return None;
            }
            let end = note.end.unwrap_or(scanned);
            (
                note.start.saturating_sub(position) as f32 / duration as f32,
                end.saturating_sub(position) as f32 / duration as f32,
            )
        }
    };
    if near >= 1.0 {
        return None;
    }
    Some((near, far.max(near + 0.004).min(1.0)))
}

fn paint_roll(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    position: u64,
    rate: u32,
    notes: &[Note],
    mode: RollMode,
    started: bool,
    scanned: u64,
) {
    let x = bounds.origin.x.as_f32();
    let y = bounds.origin.y.as_f32();
    let width = bounds.size.width.as_f32();
    let height = bounds.size.height.as_f32();
    if width < 40.0 || height < 80.0 {
        return;
    }
    let left = x + width * 0.04;
    let right = x + width * 0.96;
    let center = (left + right) * 0.5;
    let keyboard_y = y + height * 0.78;
    let horizon = y + height * 0.19;
    let keyboard_h = (height * 0.12).clamp(20.0, 100.0);
    let project = |key: f32, depth: f32| {
        let depth = depth.clamp(0.0, 1.0);
        let scale = 1.0 - depth * 0.68;
        (
            center + (left + key * (right - left) - center) * scale,
            keyboard_y - (keyboard_y - horizon) * depth.powf(0.7),
        )
    };

    polygon(
        window,
        &[
            project(0.0, 1.0),
            project(1.0, 1.0),
            project(1.0, 0.0),
            project(0.0, 0.0),
        ],
        rgba(0x102435b0),
    );
    for key in 0..=52 {
        let color = if key % 7 == 0 { 0x4bdddb70 } else { 0x318eaa35 };
        line(
            window,
            &[
                project(key as f32 / 52.0, 0.0),
                project(key as f32 / 52.0, 1.0),
            ],
            1.0,
            rgba(color),
        );
    }
    for row in 0..=12 {
        let depth = row as f32 / 12.0;
        line(
            window,
            &[project(0.0, depth), project(1.0, depth)],
            1.0,
            rgba(0x40b8d535),
        );
    }

    let palette = [
        (0x46ffe044, 0x64ffecdd),
        (0x6199ff44, 0x83b4ffdd),
        (0xee6eff44, 0xf3a4ffdd),
        (0xffb35d44, 0xffd68cdd),
        (0x57fda244, 0x96ffc5dd),
        (0xff649c44, 0xff9ec2dd),
    ];
    let mut ordered: Vec<_> = notes.iter().collect();
    ordered.sort_by_key(|note| note.start);
    let mut active = [false; 128];
    for note in ordered {
        let Some((key, key_width)) = key_position(note.pitch) else {
            continue;
        };
        if started && note.start <= position && note.end.is_none_or(|end| end > position) {
            active[note.pitch as usize] = true;
        }
        let Some((near, far)) = note_depths(note, position, rate, mode, scanned) else {
            continue;
        };
        let (glow, core) =
            palette[(usize::from(note.chip) + usize::from(note.channel)) % palette.len()];
        let shape = |half: f32| {
            [
                project(key - half, far),
                project(key + half, far),
                project(key + half, near),
                project(key - half, near),
            ]
        };
        polygon(window, &shape(key_width * 0.8), rgba(glow));
        polygon(window, &shape(key_width * 0.42), rgba(core));
    }

    for pitch in 21..=108 {
        if is_black(pitch) {
            continue;
        }
        let (key, key_width) = key_position(pitch).expect("piano key in range");
        let left_key = left + (key - key_width * 0.5) * (right - left);
        let right_key = left + (key + key_width * 0.5) * (right - left);
        let color = if active[pitch as usize] {
            0x9ffff4ff
        } else {
            0xd8f3f5ff
        };
        polygon(
            window,
            &[
                (left_key, keyboard_y),
                (right_key, keyboard_y),
                (right_key, keyboard_y + keyboard_h),
                (left_key, keyboard_y + keyboard_h),
            ],
            rgba(color),
        );
    }
    for pitch in 21..=108 {
        if !is_black(pitch) {
            continue;
        }
        let (key, key_width) = key_position(pitch).expect("piano key in range");
        let left_key = left + (key - key_width * 0.5) * (right - left);
        let right_key = left + (key + key_width * 0.5) * (right - left);
        let color = if active[pitch as usize] {
            0x39e5d7ff
        } else {
            0x142737ff
        };
        polygon(
            window,
            &[
                (left_key, keyboard_y),
                (right_key, keyboard_y),
                (right_key, keyboard_y + keyboard_h * 0.62),
                (left_key, keyboard_y + keyboard_h * 0.62),
            ],
            rgba(color),
        );
    }
    line(
        window,
        &[(left, keyboard_y), (right, keyboard_y)],
        2.0,
        rgba(0x63fff3dd),
    );
}

pub fn view(
    title: String,
    playing: bool,
    paused: bool,
    mode: RollMode,
    snapshot: Result<Snapshot, String>,
) -> Div {
    let (snapshot, error) = match snapshot {
        Ok(snapshot) => (snapshot, None),
        Err(error) => (Snapshot::default(), Some(error)),
    };
    let Snapshot {
        position,
        rate,
        notes,
        scanned,
        incomplete,
        started,
        preparing,
        ..
    } = snapshot;
    div()
        .relative()
        .size_full()
        .overflow_hidden()
        .bg(rgb(0x080f1c))
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    paint_roll(
                        window, bounds, position, rate, &notes, mode, started, scanned,
                    )
                },
            )
            .absolute()
            .size_full(),
        )
        .child(
            div()
                .absolute()
                .top(px(24.))
                .left(px(28.))
                .right(px(28.))
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().text_color(rgb(0x64eadc)).child(match mode {
                    RollMode::Incoming => "NDDRV  /  INCOMING PIANO ROLL",
                    RollMode::Trail => "NDDRV  /  LIVE PIANO ROLL",
                }))
                .child(div().text_xl().text_color(rgb(0xe4ffff)).child(title))
                .child(div().text_sm().text_color(rgb(0x82a8bb)).child(if paused {
                    "PAUSED"
                } else if playing {
                    "LIVE  ·  TRACKED CHIP VOICES"
                } else {
                    "Select a track and start playback"
                }))
                .when_some(error, |view, error| {
                    view.child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xff9e96))
                            .child(format!("Piano roll error: {error}")),
                    )
                })
                .when(incomplete, |view| {
                    view.child(
                        div().text_sm().text_color(rgb(0xffc583)).child(
                            "Some notes were skipped because a visualization buffer was full.",
                        ),
                    )
                })
                .when(preparing, |view| {
                    view.child(
                        div()
                            .text_sm()
                            .text_color(rgb(0x82a8bb))
                            .child("Preparing upcoming notes..."),
                    )
                }),
        )
        .child(
            div()
                .absolute()
                .bottom(px(16.))
                .left(px(28.))
                .text_xs()
                .text_color(rgb(0x6a8fa3))
                .child("Pitch from chip state  ·  PCM and noise are not shown"),
        )
}

#[cfg(test)]
mod tests {
    use super::{MAX_NOTES, Note, PianoRoll, RollMode, frequency_to_pitch, note_depths};
    use soundlog::chip::event::StateEvent;
    use soundlog::chip::event::ToneInfo;
    use soundlog::vgm::command::Instance;

    fn tone(frequency: f32) -> ToneInfo {
        ToneInfo::new(0, 0, Some(frequency))
    }

    #[test]
    fn tracks_notes_pitch_changes_and_independent_instances() {
        let mut roll = PianoRoll::default();
        roll.apply(
            1,
            Instance::Primary,
            10,
            vec![StateEvent::KeyOn {
                channel: 0,
                tone: tone(440.0),
            }],
        );
        roll.apply(
            1,
            Instance::Secondary,
            20,
            vec![StateEvent::KeyOn {
                channel: 0,
                tone: tone(220.0),
            }],
        );
        roll.apply(
            1,
            Instance::Primary,
            30,
            vec![StateEvent::ToneChange {
                channel: 0,
                tone: tone(880.0),
            }],
        );
        roll.apply(
            1,
            Instance::Primary,
            40,
            vec![StateEvent::KeyOff { channel: 0 }],
        );
        let notes = roll.snapshot(40, 100, RollMode::Trail);
        assert!(
            notes
                .iter()
                .any(|note| note.pitch == 69 && note.end == Some(30))
        );
        assert!(
            notes
                .iter()
                .any(|note| note.pitch == 81 && note.end == Some(40))
        );
        assert!(
            notes
                .iter()
                .any(|note| note.pitch == 57 && note.end.is_none())
        );
        assert_eq!(roll.snapshot(5, 100, RollMode::Trail).len(), 0);
    }

    #[test]
    fn rejects_invalid_frequencies_and_bounds_history() {
        for frequency in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(frequency_to_pitch(frequency), None);
        }
        let mut roll = PianoRoll::default();
        for frame in 0..MAX_NOTES + 10 {
            roll.apply(
                0,
                Instance::Primary,
                frame,
                vec![
                    StateEvent::KeyOn {
                        channel: 0,
                        tone: tone(440.0),
                    },
                    StateEvent::KeyOff { channel: 0 },
                ],
            );
        }
        assert_eq!(roll.finished.len(), MAX_NOTES);
        assert!(roll.incomplete());
        assert!(roll.snapshot(100_000, 1, RollMode::Trail).is_empty());
    }

    #[test]
    fn incoming_notes_move_toward_the_keyboard() {
        let note = Note {
            pitch: 69,
            chip: 0,
            channel: 0,
            start: 200,
            end: Some(300),
        };
        let (near, far) = note_depths(&note, 0, 100, RollMode::Incoming, 600).unwrap();
        assert!((near - 1.0 / 3.0).abs() < 0.0001);
        assert!((far - 0.5).abs() < 0.0001);
        let (near, far) = note_depths(&note, 200, 100, RollMode::Incoming, 800).unwrap();
        assert_eq!(near, 0.0);
        assert!((far - 1.0 / 6.0).abs() < 0.0001);
        assert!(note_depths(&note, 301, 100, RollMode::Incoming, 900).is_none());
        assert!(note_depths(&note, 0, 100, RollMode::Trail, 0).is_none());
        assert!(note_depths(&note, 0, 0, RollMode::Incoming, 600).is_none());
        let sustained = Note { end: None, ..note };
        let (_, far) = note_depths(&sustained, 0, 100, RollMode::Incoming, 250).unwrap();
        assert!((far - 250.0 / 600.0).abs() < 0.0001);
    }

    #[test]
    fn incoming_snapshot_includes_future_and_finishes_sustained_notes() {
        let mut roll = PianoRoll::default();
        roll.apply(
            0,
            Instance::Primary,
            200,
            vec![StateEvent::KeyOn {
                channel: 0,
                tone: tone(440.0),
            }],
        );
        assert!(roll.snapshot(0, 100, RollMode::Trail).is_empty());
        assert_eq!(roll.snapshot(0, 100, RollMode::Incoming).len(), 1);
        roll.finish(300);
        let notes = roll.snapshot(100, 100, RollMode::Incoming);
        assert_eq!(notes[0].end, Some(300));
        assert!(roll.snapshot(301, 100, RollMode::Incoming).is_empty());
    }
}
