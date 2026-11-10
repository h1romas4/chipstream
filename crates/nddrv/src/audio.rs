use anyhow::{Context, Error, Result};
use cpal::traits::{DeviceTrait, HostTrait};

pub enum Direction {
    Input,
    Output,
}

pub fn devices(direction: Direction) -> Result<Vec<(String, String)>> {
    let host = cpal::default_host();
    let devices = host_devices(&host, &direction)?;
    #[cfg(target_os = "linux")]
    if host.id() == cpal::HostId::PipeWire {
        let alsa = cpal::host_from_id(cpal::HostId::Alsa)
            .context("could not initialize ALSA for additional audio devices")?;
        let mut devices = devices;
        devices.extend(host_devices(&alsa, &direction)?);
        return Ok(devices);
    }
    Ok(devices)
}

fn host_devices(host: &cpal::Host, direction: &Direction) -> Result<Vec<(String, String)>> {
    #[cfg(target_os = "linux")]
    let pipewire_nodes = if host.id() == cpal::HostId::PipeWire {
        Some(pipewire_audio_nodes(host)?)
    } else {
        None
    };
    host.devices()
        .context("could not enumerate audio devices")?
        .filter(|device| match direction {
            Direction::Input => device.supports_input(),
            Direction::Output => device.supports_output(),
        })
        .filter_map(|device| {
            let id = match device.id().context("could not read audio device ID") {
                Ok(id) => id,
                Err(error) => return Some(Err(error)),
            };
            #[cfg(target_os = "linux")]
            if let Some(nodes) = &pipewire_nodes
                && !matches!(id.id(), "sink_default" | "input_default" | "output_default")
                && !nodes.contains(id.id())
            {
                return None;
            }
            Some(Ok((id.to_string(), device.to_string())))
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn pipewire_audio_nodes(_host: &cpal::Host) -> Result<std::collections::HashSet<String>> {
    use std::cell::{Cell, RefCell};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::time::Duration;

    use pipewire as pw;

    fn socket_in(dir: &Path) -> Option<PathBuf> {
        let socket = dir.join("pipewire-0");
        socket.exists().then_some(socket)
    }

    let socket = std::env::var_os("PIPEWIRE_RUNTIME_DIR")
        .and_then(|dir| socket_in(Path::new(&dir)))
        .or_else(|| {
            let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
            let dir = Path::new(&dir);
            socket_in(dir).or_else(|| dir.parent().and_then(socket_in))
        })
        .context("could not locate the PipeWire socket for device enumeration")?;
    // The CPAL host keeps PipeWire initialized until all registry objects are dropped.
    let mainloop =
        pw::main_loop::MainLoopRc::new(None).context("could not create PipeWire loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .context("could not create PipeWire context")?;
    let mut properties = pw::properties::PropertiesBox::new();
    properties.insert(*pw::keys::REMOTE_NAME, socket.to_string_lossy().as_ref());
    let core = context
        .connect_rc(Some(properties))
        .context("could not connect to PipeWire")?;
    let registry = core
        .get_registry_rc()
        .context("could not access PipeWire registry")?;
    let endpoints = Rc::new(RefCell::new(HashSet::new()));
    let streams = Rc::new(RefCell::new(HashSet::new()));
    let unnamed_endpoint = Rc::new(Cell::new(false));
    let done = Rc::new(Cell::new(false));
    let error = Rc::new(RefCell::new(None::<String>));
    let pending = core.sync(0).context("could not sync PipeWire registry")?;
    let _core_listener = core
        .add_listener_local()
        .done({
            let done = done.clone();
            let mainloop = mainloop.clone();
            move |id, seq| {
                if id == pw::core::PW_ID_CORE && seq == pending {
                    done.set(true);
                    mainloop.quit();
                }
            }
        })
        .error({
            let error = error.clone();
            let mainloop = mainloop.clone();
            move |id, _, code, message| {
                if id == pw::core::PW_ID_CORE {
                    *error.borrow_mut() =
                        Some(format!("PipeWire registry error {code}: {message}"));
                    mainloop.quit();
                }
            }
        })
        .register();
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let endpoints = endpoints.clone();
            let streams = streams.clone();
            let unnamed_endpoint = unnamed_endpoint.clone();
            move |global| {
                if global.type_ != pw::types::ObjectType::Node {
                    return;
                }
                let Some(props) = global.props else {
                    return;
                };
                let class = props.get(*pw::keys::MEDIA_CLASS);
                let name = props.get(*pw::keys::NODE_NAME);
                if is_pipewire_endpoint(class) {
                    if let Some(name) = name {
                        endpoints.borrow_mut().insert(name.to_owned());
                    } else {
                        unnamed_endpoint.set(true);
                    }
                } else if matches!(class, Some("Stream/Input/Audio" | "Stream/Output/Audio"))
                    && let Some(name) = name
                {
                    streams.borrow_mut().insert(name.to_owned());
                }
            }
        })
        .register();
    let loop_quit = mainloop.clone();
    let timer = mainloop.loop_().add_timer(move |_| loop_quit.quit());
    timer
        .update_timer(Some(Duration::from_secs(2)), None)
        .into_result()
        .context("could not set PipeWire enumeration timeout")?;
    mainloop.run();
    if let Some(error) = error.borrow_mut().take() {
        anyhow::bail!("{error}");
    }
    anyhow::ensure!(done.get(), "PipeWire device enumeration timed out");
    anyhow::ensure!(
        !unnamed_endpoint.get(),
        "PipeWire reported an audio device without a node name"
    );
    let endpoints = endpoints.take();
    anyhow::ensure!(
        endpoints.is_disjoint(&streams.borrow()),
        "PipeWire audio device and application stream have the same node name"
    );
    Ok(endpoints)
}

#[cfg(target_os = "linux")]
fn is_pipewire_endpoint(class: Option<&str>) -> bool {
    matches!(
        class,
        Some(
            "Audio/Sink"
                | "Audio/Source"
                | "Audio/Duplex"
                | "Audio/Sink/Internal"
                | "Audio/Source/Internal"
                | "Audio/Duplex/Internal"
        )
    )
}

pub fn with_device_busy_hint(error: Error, device_id: Option<&str>) -> Error {
    let direct_alsa = cfg!(target_os = "linux")
        && device_id.is_some_and(|id| id.starts_with("alsa:hw:") || id.starts_with("alsa:plughw:"));
    let busy = error.chain().any(|cause| {
        cause
            .downcast_ref::<cpal::Error>()
            .is_some_and(|error| error.kind() == cpal::ErrorKind::DeviceBusy)
    });
    if direct_alsa && busy {
        error.context(
            "The selected ALSA device is accessed directly and may be in use by an audio server. \
             Try System default or a PipeWire device and select the device in system sound settings",
        )
    } else {
        error
    }
}

#[cfg(test)]
mod tests {
    use super::with_device_busy_hint;

    #[cfg(target_os = "linux")]
    #[test]
    fn pipewire_audio_endpoints_exclude_application_streams() {
        for class in [
            "Audio/Sink",
            "Audio/Source",
            "Audio/Duplex",
            "Audio/Sink/Internal",
            "Audio/Source/Internal",
            "Audio/Duplex/Internal",
        ] {
            assert!(super::is_pipewire_endpoint(Some(class)), "{class}");
        }
        for class in ["Stream/Output/Audio", "Stream/Input/Audio", "Video/Source"] {
            assert!(!super::is_pipewire_endpoint(Some(class)), "{class}");
        }
        assert!(!super::is_pipewire_endpoint(None));
    }

    #[test]
    fn busy_hint_is_limited_to_direct_alsa_devices_on_linux() {
        let error = || anyhow::Error::new(cpal::Error::new(cpal::ErrorKind::DeviceBusy));
        let direct = with_device_busy_hint(error(), Some("alsa:plughw:CARD=A,DEV=0"));
        assert_eq!(
            format!("{direct:#}").contains("Try System default or a PipeWire device"),
            cfg!(target_os = "linux")
        );
        assert!(direct.downcast_ref::<cpal::Error>().is_some());
        for id in [None, Some("alsa:pipewire"), Some("alsa:pulse")] {
            assert!(
                !format!("{:#}", with_device_busy_hint(error(), id)).contains("Try System default")
            );
        }
        let other = anyhow::Error::new(cpal::Error::new(cpal::ErrorKind::PermissionDenied));
        assert!(
            !format!(
                "{:#}",
                with_device_busy_hint(other, Some("alsa:hw:CARD=A,DEV=0"))
            )
            .contains("Try System default")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a running PipeWire server and an output device"]
    fn pipewire_enumerates_and_plays_silence_on_a_specific_device() {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        use std::time::{Duration, Instant};

        assert_eq!(cpal::default_host().id(), cpal::HostId::PipeWire);
        let host = cpal::host_from_id(cpal::HostId::PipeWire).unwrap();
        let device = host
            .output_devices()
            .unwrap()
            .find(|device| {
                let id = device.id().unwrap();
                assert!(
                    cpal::host_from_id(id.host())
                        .unwrap()
                        .device_by_id(&id)
                        .is_some()
                );
                assert!(
                    !crate::playback::audio_sample_rates(Some(&id.to_string()))
                        .unwrap()
                        .is_empty()
                );
                !matches!(id.id(), "sink_default" | "output_default")
            })
            .expect("no PipeWire output device");
        let id = device.id().unwrap();
        assert!(
            super::devices(super::Direction::Output)
                .unwrap()
                .iter()
                .any(|(candidate, _)| candidate == &id.to_string())
        );
        assert!(
            super::devices(super::Direction::Input)
                .unwrap()
                .iter()
                .any(|(candidate, _)| candidate.starts_with("pipewire:"))
        );
        for input in host.input_devices().unwrap() {
            let input_id = input.id().unwrap();
            let resolved = cpal::host_from_id(input_id.host())
                .unwrap()
                .device_by_id(&input_id)
                .expect("PipeWire input must be resolvable by its saved ID");
            let config = resolved.default_input_config().unwrap();
            assert!(config.sample_rate() > 0);
            assert!(config.channels() > 0);
        }
        let supported = device.default_output_config().unwrap();
        assert_eq!(supported.sample_format(), cpal::SampleFormat::F32);
        let callbacks = Arc::new(AtomicUsize::new(0));
        let count = callbacks.clone();
        let (errors, received) = std::sync::mpsc::channel();
        let stream = device
            .build_output_stream(
                supported.config(),
                move |data: &mut [f32], _| {
                    data.fill(0.);
                    count.fetch_add(1, Ordering::Relaxed);
                },
                move |error| {
                    let _ = errors.send(error);
                },
                None,
            )
            .unwrap();
        stream.play().unwrap();
        let until = Instant::now() + Duration::from_secs(3);
        while callbacks.load(Ordering::Relaxed) == 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(callbacks.load(Ordering::Relaxed) > 0, "no audio callbacks");
        assert!(
            received.try_recv().is_err(),
            "audio stream reported an error"
        );
        let host = cpal::host_from_id(cpal::HostId::PipeWire).unwrap();
        let endpoints = super::pipewire_audio_nodes(&host).unwrap();
        let raw: Vec<_> = host
            .output_devices()
            .unwrap()
            .map(|device| device.id().unwrap())
            .collect();
        let excluded: Vec<_> = raw
            .iter()
            .filter(|id| {
                !matches!(id.id(), "sink_default" | "input_default" | "output_default")
                    && !endpoints.contains(id.id())
            })
            .collect();
        assert!(
            !excluded.is_empty(),
            "expected an active application stream"
        );
        let listed = super::devices(super::Direction::Output).unwrap();
        for id in excluded {
            assert!(
                !listed
                    .iter()
                    .any(|(candidate, _)| candidate == &id.to_string())
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "run with PIPEWIRE_RUNTIME_DIR and XDG_RUNTIME_DIR unset"]
    fn falls_back_to_alsa_without_a_pipewire_socket() {
        assert_eq!(cpal::default_host().id(), cpal::HostId::Alsa);
        let devices = super::devices(super::Direction::Output).unwrap();
        assert!(devices.iter().all(|(id, _)| id.starts_with("alsa:")));
    }
}
