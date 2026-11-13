//! Background ownership of connected NanoDrive serial devices.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use ndsif::CapabilityEntry;
use ndsif_transport::{NdsifSerialConfig, NdsifSerialTransport};

use crate::routing::Device;

enum Command<T> {
    Connect(Device),
    Disconnect(u64),
    ScanPorts,
    Acquire(Vec<u64>, Sender<Result<Vec<(u64, T)>, String>>),
    Return {
        devices: Vec<(u64, T)>,
        abandoned: Vec<u64>,
        reply: Option<Sender<Result<(), String>>>,
    },
}

pub enum DeviceEvent {
    Connected(u64, String, Vec<CapabilityEntry>),
    Disconnected(u64),
    Error(u64, String),
    Busy(u64, String, Vec<CapabilityEntry>),
    Ports(Result<Vec<String>, String>),
}

pub const BAUD_RATE: u32 = 115_200;

pub struct DeviceService<T: DeviceConnection = NdsifSerialTransport> {
    commands: Sender<Command<T>>,
    pub events: Receiver<DeviceEvent>,
}

enum Connection<T> {
    Available(T),
    Leased(Vec<CapabilityEntry>),
    Quarantined,
}

impl DeviceService<NdsifSerialTransport> {
    pub fn new() -> Self {
        Self::with_connector(
            |device| {
                let mut transport = NdsifSerialTransport::open(NdsifSerialConfig {
                    port: device.port.clone(),
                    baud_rate: BAUD_RATE,
                    response_timeout: Duration::from_millis(500),
                    reset_device: false,
                })
                .map_err(|error| error.to_string())?;
                let info = transport.device_info().map_err(|error| error.to_string())?;
                if info.model.trim().is_empty() {
                    return Err("Device returned an empty model name".into());
                }
                Ok((transport, info.model.trim().to_owned()))
            },
            || {
                let mut ports: Vec<_> = serialport::available_ports()
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .map(|port| port.port_name)
                    .collect();
                ports.sort();
                ports.dedup();
                Ok(ports)
            },
        )
    }
}

impl<T: DeviceConnection + 'static> DeviceService<T> {
    fn with_connector(
        connector: impl Fn(&Device) -> std::result::Result<(T, String), String> + Send + 'static,
        scan_ports: impl Fn() -> std::result::Result<Vec<String>, String> + Send + 'static,
    ) -> Self {
        let (commands, requests) = mpsc::channel();
        let (send_events, events) = mpsc::channel();
        std::thread::spawn(move || {
            let mut devices: HashMap<u64, Connection<T>> = HashMap::new();
            while let Ok(command) = requests.recv() {
                let event = match command {
                    Command::Connect(device) => match devices.get(&device.id) {
                        Some(Connection::Leased(capabilities)) => DeviceEvent::Busy(
                            device.id,
                            "Device is in use by playback".into(),
                            capabilities.clone(),
                        ),
                        Some(Connection::Quarantined) => DeviceEvent::Error(
                            device.id,
                            "Device is still held by a previous playback".into(),
                        ),
                        Some(Connection::Available(_)) => {
                            DeviceEvent::Error(device.id, "Device is already connected".into())
                        }
                        None => match connector(&device) {
                            Ok((connection, model)) => {
                                let capabilities = connection.capabilities().to_vec();
                                devices.insert(device.id, Connection::Available(connection));
                                DeviceEvent::Connected(device.id, model, capabilities)
                            }
                            Err(error) => DeviceEvent::Error(device.id, error),
                        },
                    },
                    Command::Disconnect(id) => match devices.get(&id) {
                        Some(Connection::Leased(capabilities)) => DeviceEvent::Busy(
                            id,
                            "Device is in use by playback".into(),
                            capabilities.clone(),
                        ),
                        Some(Connection::Quarantined) => DeviceEvent::Error(
                            id,
                            "Device is still held by a previous playback".into(),
                        ),
                        Some(Connection::Available(_)) => {
                            devices.remove(&id);
                            DeviceEvent::Disconnected(id)
                        }
                        None => DeviceEvent::Error(id, "Device is not connected".into()),
                    },
                    Command::ScanPorts => DeviceEvent::Ports(scan_ports()),
                    Command::Acquire(ids, reply) => {
                        let result = acquire_connections(&mut devices, &ids);
                        if let Err(mpsc::SendError(Ok(connections))) = reply.send(result) {
                            for (id, connection) in connections {
                                devices.insert(id, Connection::Available(connection));
                            }
                        }
                        continue;
                    }
                    Command::Return {
                        devices: returned,
                        abandoned,
                        reply,
                    } => {
                        let mut errors = Vec::new();
                        for (id, mut connection) in returned {
                            match connection.reset_for_reuse() {
                                Ok(()) => {
                                    devices.insert(id, Connection::Available(connection));
                                }
                                Err(error) => {
                                    devices.remove(&id);
                                    let message = format!("Could not reset device: {error}");
                                    errors.push(format!("Device {id}: {message}"));
                                    let _ = send_events.send(DeviceEvent::Error(id, message));
                                }
                            }
                        }
                        for id in abandoned {
                            devices.insert(id, Connection::Quarantined);
                            let message =
                                "Device is still held by a previous playback; restart to reconnect"
                                    .to_owned();
                            errors.push(format!("Device {id}: {message}"));
                            let _ = send_events.send(DeviceEvent::Error(id, message));
                        }
                        if let Some(reply) = reply {
                            let result = if errors.is_empty() {
                                Ok(())
                            } else {
                                Err(errors.join("; "))
                            };
                            let _ = reply.send(result);
                        }
                        continue;
                    }
                };
                if send_events.send(event).is_err() {
                    break;
                }
            }
        });
        Self { commands, events }
    }

    pub fn connect(&self, device: Device) -> Result<()> {
        self.commands
            .send(Command::Connect(device))
            .map_err(|_| anyhow!("NanoDrive connection worker has stopped"))
    }

    pub fn disconnect(&self, id: u64) -> Result<()> {
        self.commands
            .send(Command::Disconnect(id))
            .map_err(|_| anyhow!("NanoDrive connection worker has stopped"))
    }

    pub fn scan_ports(&self) -> Result<()> {
        self.commands
            .send(Command::ScanPorts)
            .map_err(|_| anyhow!("NanoDrive connection worker has stopped"))
    }

    pub fn playback_handle(&self) -> DeviceHandle<T> {
        DeviceHandle {
            commands: self.commands.clone(),
        }
    }
}

fn acquire_connections<T: DeviceConnection>(
    devices: &mut HashMap<u64, Connection<T>>,
    ids: &[u64],
) -> Result<Vec<(u64, T)>, String> {
    if ids.is_empty() {
        return Err("No NanoDrive devices were requested".into());
    }
    let mut seen = HashSet::new();
    for &id in ids {
        if !seen.insert(id) {
            return Err(format!("Device {id} was requested more than once"));
        }
        match devices.get(&id) {
            Some(Connection::Available(_)) => {}
            Some(Connection::Leased(_) | Connection::Quarantined) => {
                return Err(format!("Device {id} is already in use"));
            }
            None => return Err(format!("Device {id} is not connected")),
        }
    }
    let mut connections = Vec::with_capacity(ids.len());
    for &id in ids {
        let slot = devices.get_mut(&id).expect("device was validated");
        let Connection::Available(connection) = slot else {
            unreachable!("all devices were validated as available");
        };
        let capabilities = connection.capabilities().to_vec();
        let Connection::Available(connection) =
            std::mem::replace(slot, Connection::Leased(capabilities))
        else {
            unreachable!("device was validated as available");
        };
        connections.push((id, connection));
    }
    Ok(connections)
}

pub struct DeviceHandle<T: DeviceConnection> {
    commands: Sender<Command<T>>,
}

impl<T: DeviceConnection + 'static> DeviceHandle<T> {
    /// Acquire all requested connections atomically. Call from a background worker.
    pub fn acquire(&self, ids: Vec<u64>) -> Result<DeviceLease<T>> {
        let (reply, result) = mpsc::channel();
        self.commands
            .send(Command::Acquire(ids, reply))
            .map_err(|_| anyhow!("NanoDrive connection worker has stopped"))?;
        let devices = result
            .recv()
            .map_err(|_| anyhow!("NanoDrive connection worker stopped during acquisition"))?
            .map_err(|error| anyhow!(error))?;
        Ok(DeviceLease {
            devices: devices
                .into_iter()
                .map(|(id, connection)| (id, Arc::new(Mutex::new(connection))))
                .collect(),
            commands: self.commands.clone(),
        })
    }
}

pub struct DeviceLease<T: DeviceConnection> {
    devices: Vec<(u64, Arc<Mutex<T>>)>,
    commands: Sender<Command<T>>,
}

impl<T: DeviceConnection> DeviceLease<T> {
    pub fn devices(&self) -> &[(u64, Arc<Mutex<T>>)] {
        &self.devices
    }

    /// Return devices after dropping every sequencer and shared handle.
    /// Waits for the worker to reset them and reports any failure.
    pub fn release(mut self) -> Result<()> {
        if self.devices.is_empty() {
            return Err(anyhow!("No NanoDrive devices to return"));
        }
        let (reply, result) = mpsc::channel();
        self.return_devices(Some(reply))?;
        result
            .recv()
            .map_err(|_| anyhow!("NanoDrive connection worker stopped during return"))?
            .map_err(|error| anyhow!(error))
    }

    fn return_devices(&mut self, reply: Option<Sender<Result<(), String>>>) -> Result<()> {
        let mut devices = Vec::new();
        let mut abandoned = Vec::new();
        for (id, shared) in self.devices.drain(..) {
            match Arc::try_unwrap(shared) {
                Ok(connection) => {
                    let connection = match connection.into_inner() {
                        Ok(connection) => connection,
                        Err(error) => error.into_inner(),
                    };
                    devices.push((id, connection));
                }
                Err(_) => abandoned.push(id),
            }
        }
        if !devices.is_empty() || !abandoned.is_empty() {
            self.commands
                .send(Command::Return {
                    devices,
                    abandoned,
                    reply,
                })
                .map_err(|_| anyhow!("NanoDrive connection worker has stopped"))?;
        }
        Ok(())
    }
}

impl<T: DeviceConnection> Drop for DeviceLease<T> {
    fn drop(&mut self) {
        let _ = self.return_devices(None);
    }
}

pub trait DeviceConnection: Send {
    fn capabilities(&self) -> &[CapabilityEntry];
    fn reset_for_reuse(&mut self) -> Result<(), String>;
}

impl DeviceConnection for NdsifSerialTransport {
    fn capabilities(&self) -> &[CapabilityEntry] {
        NdsifSerialTransport::capabilities(self)
    }

    fn reset_for_reuse(&mut self) -> Result<(), String> {
        NdsifSerialTransport::reset_for_reuse(self).map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndsif::ClockMode;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockConnection(Vec<CapabilityEntry>);

    impl DeviceConnection for MockConnection {
        fn capabilities(&self) -> &[CapabilityEntry] {
            &self.0
        }

        fn reset_for_reuse(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn connects_multiple_devices_and_reports_errors_without_blocking_ui() {
        let service = DeviceService::with_connector(
            |device| {
                if device.port == "bad" {
                    return Err("unavailable".into());
                }
                Ok((
                    MockConnection(vec![CapabilityEntry {
                        write_id: 0x54,
                        instance: 0,
                        page: 0,
                        features: 0,
                        clock_mode: ClockMode::Variable,
                        fixed_clock_hz: 0,
                    }]),
                    "NanoDrive8".into(),
                ))
            },
            || Ok(vec!["first".into(), "second".into()]),
        );
        let device = |id, port: &str| Device {
            id,
            name: format!("device-{id}"),
            port: port.into(),
            baud_rate: 115_200,
        };
        service.connect(device(1, "first")).unwrap();
        service.connect(device(2, "second")).unwrap();
        service.connect(device(3, "bad")).unwrap();
        for id in [1, 2] {
            assert!(matches!(
                service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
                DeviceEvent::Connected(actual, model, entries)
                    if actual == id && model == "NanoDrive8" && entries.len() == 1 && entries[0].write_id == 0x54
            ));
        }
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Error(3, message) if message == "unavailable"
        ));
        service.disconnect(1).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Disconnected(1)
        ));
        service.disconnect(1).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Error(1, _)
        ));
        service.scan_ports().unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Ports(Ok(ports)) if ports == ["first", "second"]
        ));
    }

    struct ResettableConnection {
        fail_reset: bool,
        resets: Arc<AtomicUsize>,
    }

    impl DeviceConnection for ResettableConnection {
        fn capabilities(&self) -> &[CapabilityEntry] {
            &[]
        }

        fn reset_for_reuse(&mut self) -> Result<(), String> {
            self.resets.fetch_add(1, Ordering::SeqCst);
            if self.fail_reset {
                Err("reset failed".into())
            } else {
                Ok(())
            }
        }
    }

    fn lease_service(
        resets: Arc<AtomicUsize>,
        failing_id: Option<u64>,
    ) -> DeviceService<ResettableConnection> {
        let service = DeviceService::with_connector(
            move |device| {
                Ok((
                    ResettableConnection {
                        fail_reset: failing_id == Some(device.id),
                        resets: resets.clone(),
                    },
                    "NanoDrive".into(),
                ))
            },
            || Ok(Vec::new()),
        );
        for id in [1, 2] {
            service
                .connect(Device {
                    id,
                    name: format!("device-{id}"),
                    port: format!("port-{id}"),
                    baud_rate: BAUD_RATE,
                })
                .unwrap();
            assert!(matches!(
                service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
                DeviceEvent::Connected(actual, _, _) if actual == id
            ));
        }
        service
    }

    #[test]
    fn leases_are_atomic_exclusive_and_reset_before_reuse() {
        let resets = Arc::new(AtomicUsize::new(0));
        let service = lease_service(resets.clone(), None);
        let handle = service.playback_handle();
        assert!(handle.acquire(vec![]).is_err());
        assert!(handle.acquire(vec![1, 1]).is_err());
        assert!(handle.acquire(vec![1, 3]).is_err());

        let lease = handle.acquire(vec![2, 1]).unwrap();
        assert_eq!(
            lease
                .devices()
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            [2, 1]
        );
        assert!(handle.acquire(vec![1]).is_err());
        service.disconnect(1).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Busy(1, _, _)
        ));
        lease.release().unwrap();
        assert_eq!(resets.load(Ordering::SeqCst), 2);
        handle.acquire(vec![1, 2]).unwrap().release().unwrap();
        assert_eq!(resets.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn reset_failure_disconnects_only_the_failed_device() {
        let resets = Arc::new(AtomicUsize::new(0));
        let service = lease_service(resets.clone(), Some(2));
        let handle = service.playback_handle();
        let error = handle.acquire(vec![1, 2]).unwrap().release().unwrap_err();
        assert!(error.to_string().contains("Device 2"));
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Error(2, message) if message.contains("reset failed")
        ));
        assert_eq!(resets.load(Ordering::SeqCst), 2);
        assert!(handle.acquire(vec![2]).is_err());
        handle.acquire(vec![1]).unwrap().release().unwrap();
    }

    #[test]
    fn outstanding_shared_handle_quarantines_the_device() {
        let service = lease_service(Arc::new(AtomicUsize::new(0)), None);
        let handle = service.playback_handle();
        let lease = handle.acquire(vec![1]).unwrap();
        let held = Arc::clone(&lease.devices()[0].1);
        let error = lease.release().unwrap_err();
        assert!(error.to_string().contains("still held"));
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Error(1, message) if message.contains("still held")
        ));
        assert!(handle.acquire(vec![1]).is_err());
        drop(held);
        assert!(handle.acquire(vec![1]).is_err());
        handle.acquire(vec![2]).unwrap().release().unwrap();
    }

    #[test]
    fn abandoned_acquisition_response_restores_connections() {
        let service = lease_service(Arc::new(AtomicUsize::new(0)), None);
        let (reply, result) = mpsc::channel();
        drop(result);
        service
            .commands
            .send(Command::Acquire(vec![1, 2], reply))
            .unwrap();
        service
            .playback_handle()
            .acquire(vec![1, 2])
            .unwrap()
            .release()
            .unwrap();
    }

    #[test]
    fn dropping_a_lease_returns_devices_and_reports_reset_errors() {
        let resets = Arc::new(AtomicUsize::new(0));
        let service = lease_service(resets.clone(), Some(2));
        let handle = service.playback_handle();
        drop(handle.acquire(vec![1, 2]).unwrap());
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Error(2, message) if message.contains("reset failed")
        ));
        assert_eq!(resets.load(Ordering::SeqCst), 2);
        handle.acquire(vec![1]).unwrap().release().unwrap();
        assert!(handle.acquire(vec![2]).is_err());
    }

    #[test]
    fn simultaneous_playback_workers_cannot_lease_the_same_device() {
        let service = lease_service(Arc::new(AtomicUsize::new(0)), None);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let (sender, receiver) = mpsc::channel();
        let mut workers = Vec::new();
        for _ in 0..2 {
            let handle = service.playback_handle();
            let barrier = barrier.clone();
            let sender = sender.clone();
            workers.push(std::thread::spawn(move || {
                let lease = handle.acquire(vec![1]).ok();
                sender.send(lease.is_some()).unwrap();
                barrier.wait();
                if let Some(lease) = lease {
                    lease.release().unwrap();
                }
            }));
        }
        let first = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        let second = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        barrier.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_ne!(first, second);
        service
            .playback_handle()
            .acquire(vec![1])
            .unwrap()
            .release()
            .unwrap();
    }
}
