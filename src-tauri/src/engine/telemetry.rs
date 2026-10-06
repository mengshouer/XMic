use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use rtrb::{Consumer, Producer, RingBuffer};
use xmic_engine_protocol::{
    ActiveRoute, EngineMetrics, EngineOutput, EngineState, PrimaryReason, PrimaryReasonCode,
};

const TELEMETRY_QUEUE_CAPACITY: usize = 16;

/// A complete control-thread transition. String-bearing route/reason data never enters the
/// realtime telemetry queue.
pub(super) struct RuntimeTransition {
    pub state: EngineState,
    pub output: EngineOutput,
    pub primary_reason: Option<PrimaryReason>,
    pub active_route: Option<ActiveRoute>,
    pub metrics: EngineMetrics,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum ReasonUpdate {
    Preserve,
    Clear,
    Code(PrimaryReasonCode),
}

/// Copy-only data emitted by an audio loop. `rtrb::Producer::push` is bounded and non-blocking, so a
/// slow status consumer can drop telemetry without ever delaying audio.
#[derive(Debug, Clone, Copy)]
pub(super) struct RealtimeTelemetry {
    pub state: EngineState,
    pub output: EngineOutput,
    pub reason: ReasonUpdate,
    pub metrics: EngineMetrics,
}

#[derive(Debug, Clone, Copy)]
struct TelemetryPacket {
    epoch: u64,
    telemetry: RealtimeTelemetry,
}

pub(super) struct TelemetryPublisher {
    producer: Producer<TelemetryPacket>,
    epoch: Arc<AtomicU64>,
}

impl TelemetryPublisher {
    pub fn publish(&mut self, telemetry: RealtimeTelemetry) {
        let packet = TelemetryPacket {
            epoch: self.epoch.load(Ordering::Acquire),
            telemetry,
        };
        let _ = self.producer.push(packet);
    }

    /// Invalidates queued packets from the previous session phase before a control-thread
    /// transition (for example Processing -> fallback) publishes its string-bearing snapshot.
    pub fn advance_epoch(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
}

pub(super) struct TelemetryReceiver {
    consumer: Consumer<TelemetryPacket>,
    epoch: Arc<AtomicU64>,
    #[cfg(test)]
    pub(super) after_take: Option<Box<dyn FnOnce() + Send>>,
}

impl TelemetryReceiver {
    pub fn take_latest(&mut self) -> Option<RealtimeTelemetry> {
        let current_epoch = self.epoch.load(Ordering::Acquire);
        let mut latest = None;
        while let Ok(packet) = self.consumer.pop() {
            if packet.epoch == current_epoch {
                latest = Some(packet.telemetry);
            }
        }
        #[cfg(test)]
        if let Some(after_take) = self.after_take.take() {
            after_take();
        }
        latest
    }
}

pub(super) fn telemetry_channel() -> (TelemetryPublisher, TelemetryReceiver) {
    let (producer, consumer) = RingBuffer::new(TELEMETRY_QUEUE_CAPACITY);
    let epoch = Arc::new(AtomicU64::new(0));
    (
        TelemetryPublisher {
            producer,
            epoch: Arc::clone(&epoch),
        },
        TelemetryReceiver {
            consumer,
            epoch,
            #[cfg(test)]
            after_take: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advancing_epoch_discards_packets_from_the_previous_owner() {
        let (mut publisher, mut receiver) = telemetry_channel();
        publisher.publish(RealtimeTelemetry {
            state: EngineState::Processing,
            output: EngineOutput::Processed,
            reason: ReasonUpdate::Clear,
            metrics: EngineMetrics::default(),
        });
        publisher.advance_epoch();

        assert!(receiver.take_latest().is_none());
    }

    #[test]
    fn receiver_keeps_the_latest_copy_packet() {
        let (mut publisher, mut receiver) = telemetry_channel();
        publisher.publish(RealtimeTelemetry {
            state: EngineState::Degraded,
            output: EngineOutput::Silence,
            reason: ReasonUpdate::Code(PrimaryReasonCode::AecWarmup),
            metrics: EngineMetrics {
                processed_blocks: 1,
                ..EngineMetrics::default()
            },
        });
        publisher.publish(RealtimeTelemetry {
            state: EngineState::Processing,
            output: EngineOutput::Processed,
            reason: ReasonUpdate::Clear,
            metrics: EngineMetrics {
                processed_blocks: 2,
                ..EngineMetrics::default()
            },
        });

        let latest = receiver.take_latest().expect("telemetry packet");
        assert_eq!(latest.state, EngineState::Processing);
        assert_eq!(latest.metrics.processed_blocks, 2);
        assert!(receiver.take_latest().is_none());
    }
}
