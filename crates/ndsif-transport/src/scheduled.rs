use ndsif::{BytePosition, Divider, Pan, RegisterWrite, WriteId, ZeroPair};

use crate::NdsifSerialTransport;

/// One timed register write for an NDSIF transport to schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdsifRegisterEvent {
    /// NDSIF target command ID.
    pub write_id: WriteId,
    /// Target chip instance.
    pub instance: u8,
    /// Target register page.
    pub page: u8,
    /// Register address and value.
    pub write: RegisterWrite,
    /// Absolute host output-frame position.
    pub sample_frame: usize,
    /// Host output rate in frames per second.
    pub sample_rate: u32,
}

/// One timed raw byte for an NDSIF SN76489 stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NdsifPsgEvent {
    /// NDSIF target command ID.
    pub write_id: WriteId,
    /// Target chip instance.
    pub instance: u8,
    /// Target page.
    pub page: u8,
    /// Absolute host output-frame position.
    pub sample_frame: usize,
    /// Host output rate in frames per second.
    pub sample_rate: u32,
    /// Raw PSG byte.
    pub value: u8,
}

/// One encoded ADPCM byte for an NDSIF OKIM6258 stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NdsifAdpcmEvent {
    /// Audio stream instance.
    pub instance: u8,
    /// Absolute position in ADPCM bytes.
    pub position: BytePosition,
    /// Host frame through which the byte was rendered and can be committed.
    /// Playback timing is determined by `position`, not by this batch horizon.
    pub sample_frame: usize,
    /// Host output rate in frames per second.
    pub sample_rate: u32,
    /// Encoded ADPCM byte.
    pub value: u8,
}

/// A control change positioned on the NDSIF ADPCM stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NdsifAudioControl {
    /// Change the stream clock and divider.
    Clock {
        /// Input clock in hertz.
        clock_hz: u32,
        /// Clock divider.
        divider: Divider,
    },
    /// Change the output-channel selection.
    Pan(Pan),
    /// End the stream using a state-dependent repeated ADPCM byte.
    End(ZeroPair),
}

/// One positioned audio-stream control event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NdsifAudioControlEvent {
    /// Audio stream instance.
    pub instance: u8,
    /// Absolute position in ADPCM bytes.
    pub position: BytePosition,
    /// Absolute host output-frame position.
    pub sample_frame: usize,
    /// Host output rate in frames per second.
    pub sample_rate: u32,
    /// Stream control to execute.
    pub control: NdsifAudioControl,
}

/// Schedules chip writes on an NDSIF-capable device.
///
/// Implementations must queue writes; they must not treat this call as an
/// instruction to write to the device immediately. Register and PSG writes
/// use `sample_frame`; audio data and control events use byte positions for
/// playback timing. ADPCM data's frame is a commit horizon. Sequencer
/// rendering is ahead of playback, and events from different chip instances
/// may arrive out of chronological order.
pub trait NdsifTransport {
    /// Whether this transport accepts source-timed VGM initialization.
    fn supports_vgm_initialization(&self) -> bool {
        false
    }

    /// Configure a chip instance before playback.
    fn configure_chip(
        &mut self,
        write_id: WriteId,
        instance: u8,
        clock_hz: u32,
    ) -> Result<(), String>;

    /// Queue a register write at its absolute output-frame position.
    fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String>;

    /// Queue a register write from before the first positive VGM wait.
    fn schedule_vgm_initial_register_write(
        &mut self,
        event: NdsifRegisterEvent,
    ) -> Result<(), String> {
        self.schedule_register_write(event)
    }

    /// Queue a raw PSG byte at its absolute output-frame position.
    fn schedule_psg_write(&mut self, _event: NdsifPsgEvent) -> Result<(), String> {
        Err("NDSIF transport does not support raw PSG writes".to_owned())
    }

    /// Queue a PSG write from before the first positive VGM wait.
    fn schedule_vgm_initial_psg_write(&mut self, event: NdsifPsgEvent) -> Result<(), String> {
        self.schedule_psg_write(event)
    }

    /// Mark the first positive VGM wait.
    fn finish_vgm_initialization(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Configure an OKIM6258 stream and its host timing.
    fn configure_audio_stream(
        &mut self,
        _instance: u8,
        _clock_hz: u32,
        _divider: Divider,
        _output_rate: u32,
    ) -> Result<(), String> {
        Err("NDSIF transport does not support audio streams".to_owned())
    }

    /// Queue one ADPCM byte at its absolute output-frame position.
    fn schedule_adpcm_data(&mut self, _event: NdsifAdpcmEvent) -> Result<(), String> {
        Err("NDSIF transport does not support audio streams".to_owned())
    }

    /// Queue a stream control at its absolute output-frame position.
    fn schedule_audio_control(&mut self, _event: NdsifAudioControlEvent) -> Result<(), String> {
        Err("NDSIF transport does not support audio streams".to_owned())
    }

    /// Commit all queued events through an output-frame position.
    fn flush_scheduled_until(
        &mut self,
        _sample_frame: usize,
        _output_rate: u32,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Start a configured audio stream after prebuffering.
    fn start_audio_stream(&mut self, _instance: u8) -> Result<(), String> {
        Err("NDSIF transport does not support audio streams".to_owned())
    }

    /// Finish an audio stream at an absolute output-frame position.
    fn finish_audio_stream(
        &mut self,
        _instance: u8,
        _sample_frame: usize,
        _output_rate: u32,
    ) -> Result<(), String> {
        Err("NDSIF transport does not support audio streams".to_owned())
    }

    /// Seal and drain an optional timed DAC profile at end of playback.
    fn finish_dac_stream(&mut self, _sample_frame: usize, _output_rate: u32) -> Result<(), String> {
        Ok(())
    }
}

/// A scheduled transport with cached route-capability checks.
///
/// The checks must not perform I/O. Configuration and write errors are
/// reported separately.
pub trait NdsifRoutingTransport: NdsifTransport {
    /// Return whether the target supports the requested chip clock.
    fn supports_route_clock(&self, write_id: WriteId, instance: u8, clock: u32) -> bool;
    /// Return whether the target supports an OKIM6258 audio stream.
    fn supports_route_audio(&self, instance: u8, clock: u32) -> bool;
}

impl NdsifRoutingTransport for NdsifSerialTransport {
    /// Delegate clock support checks to the connected device.
    fn supports_route_clock(&self, write_id: WriteId, instance: u8, clock: u32) -> bool {
        self.supports_clock(write_id, instance, clock)
    }

    /// Delegate audio support checks to the connected device.
    fn supports_route_audio(&self, instance: u8, clock: u32) -> bool {
        self.supports_audio_stream(instance, clock)
    }
}
