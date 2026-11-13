//! A debug `NdsifTransport` implementation that prints scheduled operations
//! instead of communicating with a physical device.

use ndsif::{BytePosition, Divider, RegisterWrite, WriteId, ZeroPair};
use ndsif_transport::{
    NdsifAdpcmEvent, NdsifAudioControl, NdsifAudioControlEvent, NdsifRegisterEvent, NdsifTransport,
};

/// An operation buffered until its output-frame horizon is flushed.
enum PendingEvent {
    /// A chip register write.
    Register(NdsifRegisterEvent),
    /// An ADPCM byte for a configured audio stream.
    Adpcm(NdsifAdpcmEvent),
    /// A control event positioned on an ADPCM stream.
    AudioControl(NdsifAudioControlEvent),
}

impl PendingEvent {
    /// Return the output frame used to order and flush this operation.
    fn sample_frame(&self) -> usize {
        match self {
            Self::Register(event) => event.sample_frame,
            Self::Adpcm(event) => event.sample_frame,
            Self::AudioControl(event) => event.sample_frame,
        }
    }
}

/// Prints NDSIF configuration and playback operations for debugging.
#[derive(Default)]
struct DebugTransport {
    pending: Vec<PendingEvent>,
}

/// Demonstrates scheduled NDSIF operations by printing them instead of using a device.
impl NdsifTransport for DebugTransport {
    /// Print the configured chip and clock.
    fn configure_chip(
        &mut self,
        write_id: WriteId,
        instance: u8,
        clock_hz: u32,
    ) -> Result<(), String> {
        println!(
            "{:>8} | configure chip=0x{:02x} instance={instance} clock={clock_hz} Hz",
            "-",
            write_id.get()
        );
        Ok(())
    }

    /// Buffer a register write until its frame is flushed.
    fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String> {
        self.pending.push(PendingEvent::Register(event));
        Ok(())
    }

    /// Print the configured ADPCM stream parameters.
    fn configure_audio_stream(
        &mut self,
        instance: u8,
        clock_hz: u32,
        divider: Divider,
        output_rate: u32,
    ) -> Result<(), String> {
        println!(
            "{:>8} | configure ADPCM stream instance={instance} clock={clock_hz} Hz divider={divider:?} output_rate={output_rate} Hz",
            "-"
        );
        Ok(())
    }

    /// Buffer an ADPCM byte until its frame horizon is flushed.
    fn schedule_adpcm_data(&mut self, event: NdsifAdpcmEvent) -> Result<(), String> {
        self.pending.push(PendingEvent::Adpcm(event));
        Ok(())
    }

    /// Buffer an audio control event until its frame is flushed.
    fn schedule_audio_control(&mut self, event: NdsifAudioControlEvent) -> Result<(), String> {
        self.pending.push(PendingEvent::AudioControl(event));
        Ok(())
    }

    /// Print all buffered operations due through the requested frame.
    fn flush_scheduled_until(
        &mut self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), String> {
        if output_rate == 0 {
            return Err("output rate must be nonzero".to_owned());
        }

        self.pending.sort_by_key(PendingEvent::sample_frame);
        let split = self
            .pending
            .partition_point(|event| event.sample_frame() <= sample_frame);
        for event in self.pending.drain(..split) {
            match event {
                PendingEvent::Register(event) => println!(
                    "{:>8} | write rate={} chip=0x{:02x} instance={} page={} register=0x{:02x} value=0x{:02x}",
                    event.sample_frame,
                    event.sample_rate,
                    event.write_id.get(),
                    event.instance,
                    event.page,
                    event.write.address,
                    event.write.value,
                ),
                PendingEvent::Adpcm(event) => println!(
                    "{:>8} | ADPCM rate={} instance={} byte_position={} data=0x{:02x}",
                    event.sample_frame,
                    event.sample_rate,
                    event.instance,
                    event.position.get(),
                    event.value,
                ),
                PendingEvent::AudioControl(event) => println!(
                    "{:>8} | audio_control rate={} instance={} byte_position={} control={:?}",
                    event.sample_frame,
                    event.sample_rate,
                    event.instance,
                    event.position.get(),
                    event.control,
                ),
            }
        }
        Ok(())
    }

    /// Print that playback of the configured stream has started.
    fn start_audio_stream(&mut self, instance: u8) -> Result<(), String> {
        println!("{:>8} | start ADPCM stream instance={instance}", "-");
        Ok(())
    }

    /// Print that playback of the stream has finished.
    fn finish_audio_stream(
        &mut self,
        instance: u8,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), String> {
        println!("{sample_frame:>8} | finish ADPCM stream instance={instance} rate={output_rate}");
        Ok(())
    }
}

/// Run the debug transport example.
fn main() -> Result<(), String> {
    let mut transport = DebugTransport::default();
    transport.configure_chip(WriteId::YM2151, 0, 3_579_545)?;

    for (sample_frame, address, value) in [(0, 0x20, 0xc7), (22_050, 0x28, 0x40)] {
        transport.schedule_register_write(NdsifRegisterEvent {
            write_id: WriteId::YM2151,
            instance: 0,
            page: 0,
            write: RegisterWrite::new(address, value),
            sample_frame,
            sample_rate: 44_100,
        })?;
    }

    transport.flush_scheduled_until(0, 44_100)?;
    println!("-- later in playback --");
    transport.flush_scheduled_until(22_050, 44_100)?;

    const PREBUFFER_FRAME: usize = 22_050;
    const AUDIO_END_FRAME: usize = 44_100;
    const ADPCM: [u8; 4] = [0x12, 0x34, 0x56, 0x78];
    transport.configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)?;
    // Queue two bytes and flush them as a prebuffer before starting playback.
    for (position, value) in ADPCM.iter().copied().take(2).enumerate() {
        transport.schedule_adpcm_data(NdsifAdpcmEvent {
            instance: 0,
            position: BytePosition::new(position as u32),
            sample_frame: PREBUFFER_FRAME,
            sample_rate: 44_100,
            value,
        })?;
    }
    transport.flush_scheduled_until(PREBUFFER_FRAME, 44_100)?;
    transport.start_audio_stream(0)?;

    // ADPCM byte positions time the stream; sample_frame is the commit horizon.
    for (position, value) in ADPCM.iter().copied().enumerate().skip(2) {
        transport.schedule_adpcm_data(NdsifAdpcmEvent {
            instance: 0,
            position: BytePosition::new(position as u32),
            sample_frame: AUDIO_END_FRAME,
            sample_rate: 44_100,
            value,
        })?;
    }
    transport.schedule_audio_control(NdsifAudioControlEvent {
        instance: 0,
        position: BytePosition::new(ADPCM.len() as u32),
        sample_frame: AUDIO_END_FRAME,
        sample_rate: 44_100,
        control: NdsifAudioControl::End(ZeroPair::Byte80),
    })?;
    transport.flush_scheduled_until(AUDIO_END_FRAME, 44_100)?;
    transport.finish_audio_stream(0, AUDIO_END_FRAME, 44_100)?;
    Ok(())
}
