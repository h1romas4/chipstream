use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::state::Ymf271State;
use soundlog::chip::{Chip, Ymf271Spec};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::{VgmBuilder, VgmCallbackStream};

#[test]
fn callback_tracks_ports_instances_and_pitch() {
    let mut builder = VgmBuilder::new();
    for instance in [Instance::Primary, Instance::Secondary] {
        builder.register_chip(Chip::Ymf271, instance, 16_934_400);
        for (port, register, value) in [(6, 0, 3), (2, 0xa0, 0x45), (2, 0x90, 0x1c), (2, 0, 1)] {
            builder.add_chip_write(
                instance,
                Ymf271Spec {
                    port,
                    register,
                    value,
                },
            );
        }
    }
    builder.add_vgm_command(WaitSamples(10));
    builder.add_chip_write(
        Instance::Secondary,
        Ymf271Spec {
            port: 2,
            register: 0,
            value: 0,
        },
    );
    let mut stream = VgmCallbackStream::from_document(builder.finalize());
    for instance in [Instance::Primary, Instance::Secondary] {
        stream.track_state::<Ymf271State>(instance, 16_934_400.0);
    }
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&events);
    stream.on_write(move |instance, _: Ymf271Spec, _, changes| {
        if let Some(changes) = changes {
            captured
                .lock()
                .unwrap()
                .extend(changes.into_iter().map(|event| (instance, event)));
        }
    });
    for _ in stream {}
    let events = events.lock().unwrap();
    assert_eq!(events.len(), 3);
    for (instance, event) in &events[..2] {
        assert!(matches!(instance, Instance::Primary | Instance::Secondary));
        match event {
            StateEvent::KeyOn { channel: 24, tone } => {
                assert!((tone.freq_hz.unwrap() - 440.0).abs() < 1.0);
            }
            _ => panic!("unexpected event {event:?}"),
        }
    }
    assert!(matches!(
        events[2],
        (Instance::Secondary, StateEvent::KeyOff { channel: 24 })
    ));
}
