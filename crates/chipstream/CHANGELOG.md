# Changelog

## v0.2.0 (unreleased)

- Breaking: Configure generator PWM compatibility in `GeneratorConfig` instead of `set_pwm_compatibility`.
- Improve: Use f32 arithmetic for the DC blocker.
- Improve: Reuse sequencer capture buffers and append resampled frames directly to the output queue.
- Improve: Reduce sequencer voice construction stack usage.
- Improve: Share MultiPCM lookup tables to reduce stack and per-instance memory usage.
- Improve: Share the SCSP noise table statically instead of allocating it per instance.
- Add: XGM sequencer support for output pan, effects, write callbacks, state tracking, and command position.
- Update: Improve documentation clarity and playback examples.
