//! A `metrics` recorder that keeps only the gauges `ZEBRAD_status_json` reports.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use metrics::{Counter, Gauge, GaugeFn, Histogram, Key, KeyName, Metadata, SharedString, Unit};

pub static TIP: Slot = Slot::new();
pub static FINALIZED: Slot = Slot::new();
pub static NETWORK_TIP: Slot = Slot::new();
pub static PEERS: Slot = Slot::new();

pub struct Slot(AtomicU64);

impl Slot {
    const fn new() -> Self {
        Slot(AtomicU64::new(f64::NAN.to_bits()))
    }

    fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn store(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }

    /// The gauge as an integer, or -1 before Zebra first sets it.
    pub fn get(&self) -> i64 {
        let v = self.load();
        if v.is_finite() {
            v as i64
        } else {
            -1
        }
    }
}

pub fn reset() {
    for slot in [&TIP, &FINALIZED, &NETWORK_TIP, &PEERS] {
        slot.store(f64::NAN);
    }
}

struct SlotGauge(&'static Slot);

impl GaugeFn for SlotGauge {
    fn increment(&self, value: f64) {
        self.0.store(self.0.load() + value);
    }

    fn decrement(&self, value: f64) {
        self.0.store(self.0.load() - value);
    }

    fn set(&self, value: f64) {
        self.0.store(value);
    }
}

pub struct Recorder;

impl metrics::Recorder for Recorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, _: &Key, _: &Metadata<'_>) -> Counter {
        Counter::noop()
    }

    fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
        let slot = match key.name() {
            "zcash.chain.verified.block.height" => &TIP,
            "state.finalized.block.height" => &FINALIZED,
            "sync.estimated_network_tip_height" => &NETWORK_TIP,
            "zcash.net.peers" => &PEERS,
            _ => return Gauge::noop(),
        };
        Gauge::from_arc(Arc::new(SlotGauge(slot)))
    }

    fn register_histogram(&self, _: &Key, _: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}
