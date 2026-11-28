//! Bounded, typed execution events for generator-backed playback.

use std::any::Any;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::rc::Rc;

use soundlog::ExecutionEvent;
use soundlog::vgm::VGM_SAMPLE_RATE;

use super::VgmSequencerError;

const MAX_PENDING_EXECUTION_EVENTS: usize = 4096;

/// Failure to queue a generator execution event.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExecutionEventError {
    /// The receiver has not been registered with a sequencer.
    ReceiverNotAttached,
    /// Too many events are waiting for output.
    QueueFull {
        /// Maximum number of pending events.
        limit: usize,
    },
    /// Events must be submitted in nondecreasing source-sample order.
    OutOfOrder {
        /// Previous source sample.
        previous: u64,
        /// Newly submitted source sample.
        sample: u64,
    },
    /// The generator reported an event after its source time was consumed.
    Late {
        /// Event's source sample.
        sample: u64,
        /// Source sample already consumed.
        current: u64,
    },
    /// An event was queued after its output frame had already been delivered.
    LateOutput {
        /// Event's output frame.
        frame: usize,
        /// Next output frame.
        current: usize,
    },
}

impl fmt::Display for ExecutionEventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReceiverNotAttached => write!(f, "execution event receiver is not attached"),
            Self::QueueFull { limit } => {
                write!(f, "execution event queue reached its limit of {limit}")
            }
            Self::OutOfOrder { previous, sample } => {
                write!(f, "execution event sample {sample} precedes {previous}")
            }
            Self::Late { sample, current } => {
                write!(
                    f,
                    "execution event sample {sample} was reported after source sample {current}"
                )
            }
            Self::LateOutput { frame, current } => {
                write!(
                    f,
                    "execution event frame {frame} was reported after output frame {current}"
                )
            }
        }
    }
}

impl Error for ExecutionEventError {}

struct EventBuffer<T> {
    pending: VecDeque<Rc<ExecutionEvent<T>>>,
    last_sample: Option<u64>,
    minimum_sample: u64,
    error: Option<ExecutionEventError>,
    attached: bool,
    replaying: bool,
}

/// Producer for typed generator execution events.
///
/// Send events synchronously from the generator in nondecreasing sample order.
/// The receiver must be registered before generation starts. Overflow and
/// ordering errors are also recorded and returned by the sequencer even if the
/// generator does not propagate the result. At most 4096 events can be pending.
pub struct ExecutionEventSender<T> {
    inner: Rc<RefCell<EventBuffer<T>>>,
}

impl<T> Clone for ExecutionEventSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<T> fmt::Debug for ExecutionEventSender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecutionEventSender")
            .finish_non_exhaustive()
    }
}

impl<T> ExecutionEventSender<T> {
    /// Queue a source event. During checkpoint replay, events are suppressed.
    pub fn send(&self, event: ExecutionEvent<T>) -> Result<(), ExecutionEventError> {
        let mut buffer = self.inner.borrow_mut();
        if buffer.replaying {
            return Ok(());
        }
        if !buffer.attached {
            return Err(ExecutionEventError::ReceiverNotAttached);
        }
        if let Some(error) = &buffer.error {
            return Err(error.clone());
        }
        let error = if event.sample < buffer.minimum_sample {
            Some(ExecutionEventError::Late {
                sample: event.sample,
                current: buffer.minimum_sample,
            })
        } else if let Some(previous) = buffer.last_sample
            && event.sample < previous
        {
            Some(ExecutionEventError::OutOfOrder {
                previous,
                sample: event.sample,
            })
        } else if buffer.pending.len() >= MAX_PENDING_EXECUTION_EVENTS {
            Some(ExecutionEventError::QueueFull {
                limit: MAX_PENDING_EXECUTION_EVENTS,
            })
        } else {
            None
        };
        if let Some(error) = error {
            buffer.error = Some(error.clone());
            return Err(error);
        }
        buffer.last_sample = Some(event.sample);
        buffer.pending.push_back(Rc::new(event));
        Ok(())
    }

    #[cfg(feature = "mdx")]
    pub(super) fn send_if_attached(&self, event: ExecutionEvent<T>) {
        if self.inner.borrow().attached {
            // The sequencer returns any recorded error before delivering output.
            let _ = self.send(event);
        }
    }
}

/// Receiver registered with [`super::VgmSequencer::on_execution`].
pub struct ExecutionEventReceiver<T> {
    inner: Rc<RefCell<EventBuffer<T>>>,
    identity: Rc<()>,
}

/// Create a bounded execution-event channel for one sequencer.
pub fn execution_event_channel<T>() -> (ExecutionEventSender<T>, ExecutionEventReceiver<T>) {
    let inner = Rc::new(RefCell::new(EventBuffer {
        pending: VecDeque::new(),
        last_sample: None,
        minimum_sample: 0,
        error: None,
        attached: false,
        replaying: false,
    }));
    (
        ExecutionEventSender {
            inner: Rc::clone(&inner),
        },
        ExecutionEventReceiver {
            inner,
            identity: Rc::new(()),
        },
    )
}

struct BufferSnapshot<T> {
    pending: VecDeque<Rc<ExecutionEvent<T>>>,
    last_sample: Option<u64>,
    error: Option<ExecutionEventError>,
}

pub(super) struct ExecutionSnapshot {
    identity: Rc<()>,
    state: Box<dyn Any>,
}

pub(super) trait PendingExecutionEvents {
    fn snapshot(&self) -> ExecutionSnapshot;
    fn matches(&self, snapshot: &ExecutionSnapshot) -> bool;
    fn restore(&mut self, snapshot: &ExecutionSnapshot);
    fn set_replaying(&self, replaying: bool);
    fn set_minimum_sample(&self, sample: u64);
    fn check(&self) -> Result<(), VgmSequencerError>;
    fn discard_before(
        &mut self,
        frame: usize,
        rate: u32,
        step: u32,
    ) -> Result<(), VgmSequencerError>;
    fn dispatch(
        &mut self,
        start: usize,
        end: usize,
        inclusive: bool,
        rate: u32,
        step: u32,
    ) -> Result<(), VgmSequencerError>;
}

type ExecutionCallback<'a, T> = dyn FnMut(usize, &ExecutionEvent<T>) + 'a;

struct AttachedEvents<'a, T> {
    receiver: ExecutionEventReceiver<T>,
    callback: Box<ExecutionCallback<'a, T>>,
}

impl<T> Drop for AttachedEvents<'_, T> {
    fn drop(&mut self) {
        let mut buffer = self.receiver.inner.borrow_mut();
        buffer.attached = false;
        buffer.pending.clear();
    }
}

pub(super) fn attach<'a, T: 'static>(
    receiver: ExecutionEventReceiver<T>,
    callback: impl FnMut(usize, &ExecutionEvent<T>) + 'a,
) -> Box<dyn PendingExecutionEvents + 'a> {
    receiver.inner.borrow_mut().attached = true;
    Box::new(AttachedEvents {
        receiver,
        callback: Box::new(callback),
    })
}

fn output_frame(sample: u64, rate: u32, step: u32) -> Result<usize, VgmSequencerError> {
    let sample = u128::from(sample);
    let whole = sample * u128::from(rate / VGM_SAMPLE_RATE);
    let fraction = (sample * u128::from(step) + (1_u128 << 31)) >> 32;
    usize::try_from(whole + fraction).map_err(|_| VgmSequencerError::DurationOverflow)
}

impl<T: 'static> PendingExecutionEvents for AttachedEvents<'_, T> {
    fn snapshot(&self) -> ExecutionSnapshot {
        let buffer = self.receiver.inner.borrow();
        ExecutionSnapshot {
            identity: Rc::clone(&self.receiver.identity),
            state: Box::new(BufferSnapshot {
                pending: buffer.pending.clone(),
                last_sample: buffer.last_sample,
                error: buffer.error.clone(),
            }),
        }
    }

    fn matches(&self, snapshot: &ExecutionSnapshot) -> bool {
        Rc::ptr_eq(&self.receiver.identity, &snapshot.identity)
            && snapshot.state.is::<BufferSnapshot<T>>()
    }

    fn restore(&mut self, snapshot: &ExecutionSnapshot) {
        let saved = snapshot
            .state
            .downcast_ref::<BufferSnapshot<T>>()
            .expect("checked snapshot type");
        let mut buffer = self.receiver.inner.borrow_mut();
        buffer.pending.clone_from(&saved.pending);
        buffer.last_sample = saved.last_sample;
        buffer.error.clone_from(&saved.error);
    }

    fn set_replaying(&self, replaying: bool) {
        self.receiver.inner.borrow_mut().replaying = replaying;
    }

    fn set_minimum_sample(&self, sample: u64) {
        self.receiver.inner.borrow_mut().minimum_sample = sample;
    }

    fn check(&self) -> Result<(), VgmSequencerError> {
        match &self.receiver.inner.borrow().error {
            Some(error) => Err(VgmSequencerError::ExecutionEvent(error.clone())),
            None => Ok(()),
        }
    }

    fn discard_before(
        &mut self,
        frame: usize,
        rate: u32,
        step: u32,
    ) -> Result<(), VgmSequencerError> {
        let mut buffer = self.receiver.inner.borrow_mut();
        while let Some(event) = buffer.pending.front() {
            if output_frame(event.sample, rate, step)? >= frame {
                break;
            }
            buffer.pending.pop_front();
        }
        Ok(())
    }

    fn dispatch(
        &mut self,
        start: usize,
        end: usize,
        inclusive: bool,
        rate: u32,
        step: u32,
    ) -> Result<(), VgmSequencerError> {
        loop {
            let next = {
                let mut buffer = self.receiver.inner.borrow_mut();
                let Some(event) = buffer.pending.front() else {
                    break;
                };
                let frame = output_frame(event.sample, rate, step)?;
                if frame < start {
                    return Err(VgmSequencerError::ExecutionEvent(
                        ExecutionEventError::LateOutput {
                            frame,
                            current: start,
                        },
                    ));
                }
                if frame > end || (frame == end && !inclusive) {
                    break;
                }
                (frame, buffer.pending.pop_front().expect("front exists"))
            };
            (self.callback)(next.0, &next.1);
        }
        if inclusive {
            self.receiver.inner.borrow_mut().pending.clear();
        }
        self.check()
    }
}
