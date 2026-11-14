//! MAME plugin listener and device bridge.

mod clock;
mod install;
mod protocol;
#[cfg(all(test, unix))]
mod tests;

use std::collections::VecDeque;
use std::io::{ErrorKind, Read};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ndsif::{Command, OrderedLiveRecord, OrderedLiveWrite, RegisterWrite, WriteId};
use ndsif_transport::{
    NdsifOrderedLiveSession, NdsifRegisterEvent, NdsifSerialTransport, NdsifTransport,
};

use crate::device::DeviceHandle;
pub use install::install as install_plugins;
pub use protocol::Plugin;
use protocol::{Decoder, Event};

pub const ADDRESS: &str = "127.0.0.1:32151";
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug)]
pub enum ServerEvent {
    Waiting,
    Connected,
    Warning(String),
    Finished(Result<(), String>),
}

pub struct Server {
    stop: Arc<AtomicBool>,
    pub events: Receiver<ServerEvent>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub fn start(
        handle: DeviceHandle<NdsifSerialTransport>,
        device_id: u64,
        plugin: Plugin,
        mame_home: Option<PathBuf>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (sender, events) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = run(
                handle,
                device_id,
                plugin,
                mame_home.as_deref(),
                &worker_stop,
                &sender,
            );
            let _ = sender.send(ServerEvent::Finished(
                result.map_err(|error| format!("{error:#}")),
            ));
        });
        Self {
            stop,
            events,
            worker: Some(worker),
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            eprintln!("MAME server worker panicked");
        }
        for event in self.events.try_iter() {
            match event {
                ServerEvent::Warning(message) | ServerEvent::Finished(Err(message)) => {
                    eprintln!("MAME bridge: {message}");
                }
                _ => {}
            }
        }
    }
}

fn run(
    handle: DeviceHandle<NdsifSerialTransport>,
    device_id: u64,
    plugin: Plugin,
    mame_home: Option<&Path>,
    stop: &AtomicBool,
    events: &Sender<ServerEvent>,
) -> Result<()> {
    let listener = TcpListener::bind(ADDRESS).context("Could not start MAME listener")?;
    listener.set_nonblocking(true)?;
    let lease = handle
        .acquire(vec![device_id])
        .context("Could not acquire NanoDrive")?;
    let result = (|| -> Result<()> {
        let mut transport = lease.devices()[0]
            .1
            .lock()
            .map_err(|_| anyhow::anyhow!("NanoDrive connection lock is poisoned"))?;
        transport.reset_for_reuse()?;
        match plugin {
            Plugin::NanoDrive6 => {
                transport.configure_live_clock(WriteId::YM2612, 0, 7_670_454)?;
                transport.configure_live_clock(WriteId::SN76489, 0, 3_579_545)?;
                {
                    let live = transport.begin_ordered_live()?;
                    if live.capabilities().revision != 5 {
                        bail!("ND6 ordered live revision 5 is required for PSG");
                    }
                }
                transport.reset_for_reuse()?;
            }
            Plugin::NanoDrive8 => {
                clock::mame_executable(mame_home.context("Set MAME HOME in Routing > MAME")?)?;
                if !transport.capabilities().iter().any(|entry| {
                    entry.write_id == WriteId::YM2151.get()
                        && entry.instance == 0
                        && entry.page == 0
                        && entry.features & 1 != 0
                }) {
                    bail!("NanoDrive does not advertise primary YM2151 writes");
                }
            }
        }
        while !stop.load(Ordering::Acquire) {
            let _ = events.send(ServerEvent::Waiting);
            let stream = loop {
                if stop.load(Ordering::Acquire) {
                    break None;
                }
                match listener.accept() {
                    Ok((stream, _)) => break Some(stream),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => std::thread::sleep(POLL),
                    Err(error) => return Err(error).context("MAME listener failed"),
                }
            };
            let Some(mut stream) = stream else { break };
            stream.set_read_timeout(Some(POLL))?;
            stream.set_nodelay(true)?;
            let _ = events.send(ServerEvent::Connected);
            let session = match plugin {
                Plugin::NanoDrive6 => {
                    nd6_connection(&mut transport, &listener, &mut stream, stop, events)
                }
                Plugin::NanoDrive8 => nd8_connection(
                    &mut transport,
                    &listener,
                    &mut stream,
                    stop,
                    events,
                    |_, name, tag, stop| {
                        clock::ym2151_clock(
                            mame_home.context("Set MAME HOME in Routing > MAME")?,
                            name,
                            tag,
                            stop,
                        )
                    },
                ),
            };
            transport
                .reset_for_reuse()
                .context("Could not reset NanoDrive after MAME disconnected")?;
            if let Err(error) = session {
                let _ = events.send(ServerEvent::Warning(format!(
                    "MAME session ended: {error:#}"
                )));
            }
        }
        Ok(())
    })();
    let release = lease.release().context("Could not return NanoDrive");
    match (result, release) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(release)) => Err(anyhow::anyhow!("{error:#}; {release:#}")),
    }
}

struct Input {
    decoder: Decoder,
    pending: VecDeque<Event>,
}

impl Input {
    fn new(plugin: Plugin) -> Self {
        Self {
            decoder: Decoder::new(plugin),
            pending: VecDeque::new(),
        }
    }

    fn next(
        &mut self,
        listener: &TcpListener,
        stream: &mut TcpStream,
        stop: &AtomicBool,
    ) -> Result<Option<Event>> {
        loop {
            if stop.load(Ordering::Acquire) {
                return Ok(None);
            }
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }
            loop {
                match listener.accept() {
                    Ok((rejected, _)) => drop(rejected),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error).context("MAME listener failed"),
                }
            }
            let mut buffer = [0; 8192];
            match stream.read(&mut buffer) {
                Ok(0) => {
                    self.decoder.finish()?;
                    return Ok(None);
                }
                Ok(length) => {
                    self.pending.extend(self.decoder.push(&buffer[..length])?);
                    let deadline = Instant::now() + Duration::from_millis(5);
                    while self.pending.len() < 100 && !stop.load(Ordering::Acquire) {
                        let Some(remaining) = deadline.checked_duration_since(Instant::now())
                        else {
                            break;
                        };
                        if remaining < Duration::from_millis(1) {
                            break;
                        }
                        stream.set_read_timeout(Some(remaining))?;
                        match stream.read(&mut buffer) {
                            Ok(0) => {
                                self.decoder.finish()?;
                                break;
                            }
                            Ok(length) => {
                                self.pending.extend(self.decoder.push(&buffer[..length])?)
                            }
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    ErrorKind::WouldBlock | ErrorKind::TimedOut
                                ) =>
                            {
                                break;
                            }
                            Err(error) => return Err(error).context("MAME socket read failed"),
                        }
                    }
                    stream.set_read_timeout(Some(POLL))?;
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(error) => return Err(error).context("MAME socket read failed"),
            }
        }
    }
}

enum Nd6Control {
    Reset,
    Clock(u32),
    Finished,
}

fn nd6_connection(
    transport: &mut NdsifSerialTransport,
    listener: &TcpListener,
    stream: &mut TcpStream,
    stop: &AtomicBool,
    events: &Sender<ServerEvent>,
) -> Result<()> {
    let mut input = Input::new(Plugin::NanoDrive6);
    let mut clock = 7_670_454;
    loop {
        transport.reset_for_reuse()?;
        transport.configure_live_clock(WriteId::YM2612, 0, clock)?;
        transport.configure_live_clock(WriteId::SN76489, 0, 3_579_545)?;
        let control = {
            let mut live = transport.begin_ordered_live()?;
            if live.capabilities().revision != 5 {
                bail!("ND6 ordered live revision 5 is required for PSG");
            }
            nd6_events(&mut live, &mut input, listener, stream, stop, events)?
        };
        match control {
            Nd6Control::Finished => return Ok(()),
            Nd6Control::Reset => clock = 7_670_454,
            Nd6Control::Clock(hz @ (7_670_454 | 7_600_489)) => clock = hz,
            Nd6Control::Clock(hz) => bail!("Unsupported MAME YM2612 clock {hz} Hz"),
        }
    }
}

fn nd6_events(
    live: &mut NdsifOrderedLiveSession<'_>,
    input: &mut Input,
    listener: &TcpListener,
    stream: &mut TcpStream,
    stop: &AtomicBool,
    events: &Sender<ServerEvent>,
) -> Result<Nd6Control> {
    let mut previous = None;
    let mut fast = false;
    let mut clock = ClockSkip::default();
    let mut batch = Vec::new();
    while let Some(event) = input.next(listener, stream, stop)? {
        match event {
            Event::Ym2612 {
                tick,
                page,
                address,
                value,
            } => {
                let write = if page == 0 && address == 0x2a {
                    OrderedLiveWrite::Dac(value)
                } else {
                    OrderedLiveWrite::Register {
                        page,
                        address,
                        value,
                    }
                };
                append_ordered(
                    &mut batch,
                    live,
                    &mut previous,
                    &mut clock,
                    tick,
                    write,
                    fast,
                )?;
            }
            Event::Psg { tick, value } => {
                append_ordered(
                    &mut batch,
                    live,
                    &mut previous,
                    &mut clock,
                    tick,
                    OrderedLiveWrite::Psg(value),
                    fast,
                )?;
            }
            Event::Reset | Event::Clock(_) | Event::Quit => {
                flush_ordered(live, &mut batch)?;
                return Ok(match event {
                    Event::Reset => Nd6Control::Reset,
                    Event::Clock(hz) => Nd6Control::Clock(hz),
                    _ => Nd6Control::Finished,
                });
            }
            Event::Pause | Event::Resume => {
                flush_ordered(live, &mut batch)?;
                if live.supports_output_volume() {
                    live.set_output_muted(matches!(event, Event::Pause))?;
                } else {
                    let _ = events.send(ServerEvent::Warning(
                        "NanoDrive does not support output mute".into(),
                    ));
                }
                previous = None;
                clock = ClockSkip::default();
            }
            Event::FastForward { enabled, .. } => {
                flush_ordered(live, &mut batch)?;
                fast = enabled;
                previous = None;
                clock = ClockSkip::default();
            }
            Event::Timestamp(_) => {}
            _ => bail!("Unexpected ND6 MAME message"),
        }
        if input.pending.is_empty() {
            flush_ordered(live, &mut batch)?;
        }
    }
    flush_ordered(live, &mut batch)?;
    Ok(Nd6Control::Finished)
}

fn append_ordered(
    batch: &mut Vec<OrderedLiveRecord>,
    live: &mut NdsifOrderedLiveSession<'_>,
    previous: &mut Option<u64>,
    clock: &mut ClockSkip,
    tick: u64,
    write: OrderedLiveWrite,
    fast: bool,
) -> Result<()> {
    let tick = match *previous {
        Some(prev) if tick < prev && prev - tick > 441 => {
            bail!("MAME time moved backwards by more than 10 ms")
        }
        Some(prev) => tick.max(prev),
        None => tick,
    };
    let delta = tick.saturating_sub(previous.unwrap_or(tick));
    let delta = u32::try_from(delta).context("MAME timestamp interval exceeds u32")?;
    *previous = Some(tick);
    let (wait_ticks, drop) = if fast {
        (0, matches!(write, OrderedLiveWrite::Dac(_)))
    } else {
        clock.adjust(
            tick,
            delta,
            Instant::now(),
            matches!(write, OrderedLiveWrite::Dac(_)),
        )
    };
    if drop {
        return Ok(());
    }
    let record = OrderedLiveRecord { wait_ticks, write };
    batch.push(record);
    let command = Command::OrderedLiveData {
        sequence: 0,
        records: batch,
    };
    let fits = batch.len() <= usize::from(live.capabilities().max_records)
        && command
            .to_frame(0)
            .is_ok_and(|frame| frame.payload().len() <= usize::from(live.max_payload()));
    if !fits {
        batch.pop();
        flush_ordered(live, batch)?;
        batch.push(record);
    }
    Ok(())
}

fn flush_ordered(
    live: &mut NdsifOrderedLiveSession<'_>,
    batch: &mut Vec<OrderedLiveRecord>,
) -> Result<()> {
    if !batch.is_empty() {
        live.send(batch)?;
        batch.clear();
    }
    Ok(())
}

#[derive(Default)]
struct ClockSkip {
    anchor: Option<(u64, Instant)>,
    previous: Option<(u64, Instant)>,
    removed: u64,
    debt: u64,
}

impl ClockSkip {
    fn adjust(&mut self, tick: u64, delta: u32, now: Instant, dac: bool) -> (u32, bool) {
        if let Some((last_tick, last_wall)) = self.previous
            && tick.saturating_sub(last_tick) >= 11_025
            && now.duration_since(last_wall) >= Duration::from_millis(250)
            && ((tick - last_tick) as f64 / 44_100.0 - now.duration_since(last_wall).as_secs_f64())
                .abs()
                < 0.05
        {
            *self = Self::default();
        }
        let (anchor_tick, anchor_wall) = *self.anchor.get_or_insert((tick, now));
        self.previous = Some((tick, now));
        let wall_ticks = (now.duration_since(anchor_wall).as_secs_f64() * 44_100.0) as u64;
        let ahead = tick
            .saturating_sub(anchor_tick)
            .saturating_sub(wall_ticks)
            .saturating_sub(self.removed);
        if self.debt == 0 && ahead > 441 {
            self.debt = ahead - 88;
        }
        self.debt = self.debt.min(ahead.saturating_sub(88));
        let skipping = self.debt != 0;
        let allowance = u64::from(delta).min(self.debt);
        self.debt -= allowance;
        self.removed += allowance;
        (
            delta - allowance as u32,
            dac && skipping && allowance == u64::from(delta),
        )
    }
}

fn nd8_connection(
    transport: &mut NdsifSerialTransport,
    listener: &TcpListener,
    stream: &mut TcpStream,
    stop: &AtomicBool,
    events: &Sender<ServerEvent>,
    mut query_clock: impl FnMut(u32, &str, &str, &AtomicBool) -> Result<u32>,
) -> Result<()> {
    let mut input = Input::new(Plugin::NanoDrive8);
    let mut chip_clock = None;
    let mut initialized = false;
    while let Some(event) = input.next(listener, stream, stop)? {
        match event {
            Event::Machine { pid, name, tag } => {
                if initialized {
                    transport
                        .flush_scheduled_until(0, 44_100)
                        .map_err(anyhow::Error::msg)?;
                }
                chip_clock = Some(query_clock(pid, &name, &tag, stop)?);
                initialized = false;
            }
            Event::Reset => {
                if initialized {
                    transport
                        .flush_scheduled_until(0, 44_100)
                        .map_err(anyhow::Error::msg)?;
                }
                let hz = chip_clock.context("MAME did not provide the YM2151 identity")?;
                transport.reset_for_reuse()?;
                transport
                    .configure_chip(WriteId::YM2151, 0, hz)
                    .map_err(anyhow::Error::msg)?;
                initialized = true;
            }
            Event::Ym2151 { address, value } => {
                if !initialized {
                    bail!("YM2151 write before MAME reset");
                }
                transport
                    .schedule_register_write(NdsifRegisterEvent {
                        write_id: WriteId::YM2151,
                        instance: 0,
                        page: 0,
                        write: RegisterWrite::new(address, value),
                        sample_frame: 0,
                        sample_rate: 44_100,
                    })
                    .map_err(anyhow::Error::msg)?;
                if input.pending.is_empty() {
                    transport
                        .flush_scheduled_until(0, 44_100)
                        .map_err(anyhow::Error::msg)?;
                }
            }
            Event::Pause | Event::Resume => {
                transport
                    .flush_scheduled_until(0, 44_100)
                    .map_err(anyhow::Error::msg)?;
                if transport.supports_output_volume() {
                    transport.set_output_muted(matches!(event, Event::Pause))?;
                } else {
                    let _ = events.send(ServerEvent::Warning(
                        "NanoDrive does not support output mute".into(),
                    ));
                }
            }
            Event::Quit => {
                transport
                    .flush_scheduled_until(0, 44_100)
                    .map_err(anyhow::Error::msg)?;
                return Ok(());
            }
            _ => bail!("Unexpected ND8 MAME message"),
        }
    }
    if initialized {
        transport
            .flush_scheduled_until(0, 44_100)
            .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}
