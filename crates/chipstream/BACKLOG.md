# chipstream backlog

## Engine and game integration

- Explore an engine-independent playback engine with a dedicated worker that
  owns sequencers. Define command and PCM queues, bounded buffering, error and
  underrun reporting, and latency. Evaluate whether moving sequencers across
  threads is necessary; their current API does not require `Send`.
- Manage multiple concurrent music and sound-effect instances: start, pause,
  stop, completion, per-instance and bus gains, and block alignment. Consider
  separate chip state per instance and whether shared-chip playback is needed.
- Separate reusable music/sample assets from mutable playback state. Evaluate
  shared VGM bytes and MDX/PDX data without retaining a full VGM document, and
  measure memory and initialization costs for concurrent playback.
- Define generated versus audible playback position and when controls take
  effect relative to buffering and read-ahead. Consider sample-accurate
  controls only where needed.
- Explore a `chipstream-bevy` crate for Bevy asset loading, playback handles,
  mixing, and resource lifecycle, building on the engine-independent layer.

VGM remains the common command representation for custom formats; retain the
existing generator path unless a concrete requirement calls for another one.
