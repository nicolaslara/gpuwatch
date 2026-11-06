//! The throttling rule: a sliding window of busy samples per GPU, evaluated
//! against a sensitivity preset. No NVML here, so it is unit-testable.

use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};

/// One NVML poll of one GPU.
#[derive(Clone, Debug)]
pub struct Sample {
    pub util_pct: u32,
    pub sm_clock_mhz: u32,
    pub max_sm_clock_mhz: u32,
    pub temp_c: u32,
    pub power_w: f64,
    pub power_limit_w: f64,
    /// NVML clocks-event-reason bits (nvmlClocksEventReasons_t).
    pub reasons: u64,
}

pub const REASON_SW_THERMAL: u64 = 0x20;
pub const REASON_HW_THERMAL: u64 = 0x40;

const REASON_NAMES: &[(u64, &str)] = &[
    (0x01, "gpu_idle"),
    (0x02, "applications_clocks_setting"),
    (0x04, "sw_power_cap"),
    (0x08, "hw_slowdown"),
    (0x10, "sync_boost"),
    (0x20, "sw_thermal_slowdown"),
    (0x40, "hw_thermal_slowdown"),
    (0x80, "hw_power_brake_slowdown"),
    (0x100, "display_clock_setting"),
];

impl Sample {
    /// Loaded enough that a reduced clock costs the workload something.
    pub fn busy(&self) -> bool {
        self.util_pct > 50
    }
    pub fn thermal(&self) -> bool {
        self.reasons & (REASON_SW_THERMAL | REASON_HW_THERMAL) != 0
    }
    pub fn clock_ratio(&self) -> f64 {
        if self.max_sm_clock_mhz == 0 {
            return 1.0;
        }
        (f64::from(self.sm_clock_mhz) / f64::from(self.max_sm_clock_mhz)).min(1.0)
    }
    pub fn power_ratio(&self) -> f64 {
        if self.power_limit_w <= 0.0 {
            return 0.0;
        }
        self.power_w / self.power_limit_w
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    Low,
    Medium,
    High,
}

impl Sensitivity {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

/// What has to hold over a full window for an event to fire.
#[derive(Clone, Debug, Serialize)]
pub struct Thresholds {
    pub sensitivity: Sensitivity,
    /// Busy minutes a window spans.
    pub window_minutes: f64,
    /// Fraction of busy samples that must carry a thermal slowdown reason.
    pub min_thermal_fraction: f64,
    /// Mean clock reduction (1 - clock/max) over the thermally throttled
    /// samples that must be reached.
    pub min_reduction: f64,
}

impl Thresholds {
    /// `medium` is the rule Modal's worker applies when it logs
    /// "sustained thermal throttling"; `low` only catches a device running at
    /// half speed or worse; `high` reacts to a 15% loss in five minutes. A 10%
    /// reduction, common on a warm but healthy GPU, never fires.
    pub fn preset(sensitivity: Sensitivity) -> Self {
        let (window_minutes, min_thermal_fraction, min_reduction) = match sensitivity {
            Sensitivity::Low => (10.0, 0.5, 0.5),
            Sensitivity::Medium => (10.0, 0.5, 0.3),
            Sensitivity::High => (5.0, 0.3, 0.15),
        };
        Self {
            sensitivity,
            window_minutes,
            min_thermal_fraction,
            min_reduction,
        }
    }

    pub fn window_samples(&self, interval_secs: f64) -> usize {
        ((self.window_minutes * 60.0 / interval_secs).round() as usize).max(2)
    }
}

/// Aggregates over the busy samples of a window.
#[derive(Clone, Debug, Serialize)]
pub struct WindowStats {
    pub busy_samples: usize,
    pub busy_minutes: f64,
    /// Fraction of busy samples with a thermal slowdown reason.
    pub thermal_fraction: f64,
    /// Mean clock/max over all busy samples: the share of rated speed the
    /// workload actually got.
    pub clock_ratio_busy: f64,
    /// Mean clock/max over the thermally throttled samples only.
    pub clock_ratio_throttled: Option<f64>,
    pub clock_mhz_min: u32,
    pub temp_c_max: u32,
    pub temp_c_mean: f64,
    pub power_ratio_mean: f64,
    /// How many busy samples carried each clocks-event reason.
    pub reasons: BTreeMap<String, usize>,
}

/// Why a window qualified as an event.
#[derive(Clone, Debug, Serialize)]
pub struct Verdict {
    pub event: bool,
    pub reduction_throttled: Option<f64>,
    pub thresholds: Thresholds,
}

pub struct Window {
    capacity: usize,
    interval_secs: f64,
    samples: VecDeque<Sample>,
}

impl Window {
    pub fn new(capacity: usize, interval_secs: f64) -> Self {
        Self {
            capacity,
            interval_secs,
            samples: VecDeque::with_capacity(capacity),
        }
    }

    /// Idle samples are dropped: a throttled idle GPU costs nothing.
    pub fn push(&mut self, sample: Sample) {
        if !sample.busy() {
            return;
        }
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn is_full(&self) -> bool {
        self.samples.len() >= self.capacity
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    pub fn stats(&self) -> Option<WindowStats> {
        if self.samples.is_empty() {
            return None;
        }
        let n = self.samples.len();
        let mut thermal = 0usize;
        let mut ratio_sum = 0.0;
        let mut ratio_thr_sum = 0.0;
        let mut temp_sum = 0.0;
        let mut power_sum = 0.0;
        let mut clock_min = u32::MAX;
        let mut temp_max = 0;
        let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
        for s in &self.samples {
            let r = s.clock_ratio();
            ratio_sum += r;
            if s.thermal() {
                thermal += 1;
                ratio_thr_sum += r;
            }
            temp_sum += f64::from(s.temp_c);
            power_sum += s.power_ratio();
            clock_min = clock_min.min(s.sm_clock_mhz);
            temp_max = temp_max.max(s.temp_c);
            for (bit, name) in REASON_NAMES {
                if s.reasons & bit != 0 {
                    *reasons.entry((*name).to_string()).or_default() += 1;
                }
            }
        }
        let nf = n as f64;
        Some(WindowStats {
            busy_samples: n,
            busy_minutes: nf * self.interval_secs / 60.0,
            thermal_fraction: thermal as f64 / nf,
            clock_ratio_busy: ratio_sum / nf,
            clock_ratio_throttled: (thermal > 0).then(|| ratio_thr_sum / thermal as f64),
            clock_mhz_min: clock_min,
            temp_c_max: temp_max,
            temp_c_mean: temp_sum / nf,
            power_ratio_mean: power_sum / nf,
            reasons,
        })
    }

    /// Applies the thresholds to the current stats; `require_full` is the
    /// daemon's mode, a one-shot check judges whatever it managed to collect.
    pub fn evaluate(&self, t: &Thresholds, require_full: bool) -> Option<(WindowStats, Verdict)> {
        if require_full && !self.is_full() {
            return None;
        }
        let stats = self.stats()?;
        let reduction = stats.clock_ratio_throttled.map(|r| 1.0 - r);
        let event = stats.thermal_fraction >= t.min_thermal_fraction
            && reduction.is_some_and(|r| r >= t.min_reduction);
        let verdict = Verdict {
            event,
            reduction_throttled: reduction,
            thresholds: t.clone(),
        };
        Some((stats, verdict))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(util: u32, clock: u32, reasons: u64) -> Sample {
        Sample {
            util_pct: util,
            sm_clock_mhz: clock,
            max_sm_clock_mhz: 1980,
            temp_c: 80,
            power_w: 400.0,
            power_limit_w: 700.0,
            reasons,
        }
    }

    fn window_with(t: &Thresholds, samples: impl IntoIterator<Item = Sample>) -> Window {
        let mut w = Window::new(t.window_samples(2.0), 2.0);
        for s in samples {
            w.push(s);
        }
        w
    }

    #[test]
    fn medium_matches_the_worker_rule_at_the_boundary() {
        let t = Thresholds::preset(Sensitivity::Medium);
        let n = t.window_samples(2.0);
        assert_eq!(n, 300);
        // Half the samples throttled at 30% below max: fires.
        let half = (0..n).map(|i| {
            if i % 2 == 0 {
                sample(90, 1385, REASON_HW_THERMAL)
            } else {
                sample(90, 1980, 0)
            }
        });
        let w = window_with(&t, half);
        let (stats, v) = w.evaluate(&t, true).unwrap();
        assert!(v.event, "{v:?}");
        assert!((stats.thermal_fraction - 0.5).abs() < 1e-9);
        assert!((v.reduction_throttled.unwrap() - 0.3).abs() < 1e-3);
        // Just below either gate: does not fire.
        let w = window_with(
            &t,
            (0..n).map(|i| {
                if i % 2 == 0 {
                    sample(90, 1400, REASON_HW_THERMAL)
                } else {
                    sample(90, 1980, 0)
                }
            }),
        );
        assert!(!w.evaluate(&t, true).unwrap().1.event);
        let w = window_with(
            &t,
            (0..n).map(|i| {
                if i < n / 2 - 1 {
                    sample(90, 900, REASON_SW_THERMAL)
                } else {
                    sample(90, 1980, 0)
                }
            }),
        );
        assert!(!w.evaluate(&t, true).unwrap().1.event);
    }

    #[test]
    fn idle_samples_do_not_fill_the_window() {
        let t = Thresholds::preset(Sensitivity::High);
        let w = window_with(&t, (0..1000).map(|_| sample(10, 300, REASON_HW_THERMAL)));
        assert!(!w.is_full());
        assert!(w.evaluate(&t, true).is_none());
        assert!(w.stats().is_none());
    }

    #[test]
    fn power_cap_is_not_thermal() {
        let t = Thresholds::preset(Sensitivity::High);
        let w = window_with(&t, (0..200).map(|_| sample(95, 1200, 0x04)));
        let (stats, v) = w.evaluate(&t, true).unwrap();
        assert!(!v.event);
        assert_eq!(stats.thermal_fraction, 0.0);
        assert_eq!(stats.reasons.get("sw_power_cap"), Some(&150));
        assert!(v.reduction_throttled.is_none());
    }

    #[test]
    fn healthy_ten_percent_never_fires_even_on_high() {
        let t = Thresholds::preset(Sensitivity::High);
        let w = window_with(&t, (0..200).map(|_| sample(95, 1782, REASON_SW_THERMAL)));
        let (stats, v) = w.evaluate(&t, true).unwrap();
        assert!(!v.event);
        assert!((stats.clock_ratio_busy - 0.9).abs() < 1e-3);
    }

    #[test]
    fn low_needs_half_speed() {
        let t = Thresholds::preset(Sensitivity::Low);
        let w = window_with(&t, (0..300).map(|_| sample(95, 1100, REASON_HW_THERMAL)));
        assert!(!w.evaluate(&t, true).unwrap().1.event);
        let w = window_with(&t, (0..300).map(|_| sample(95, 900, REASON_HW_THERMAL)));
        assert!(w.evaluate(&t, true).unwrap().1.event);
    }

    #[test]
    fn one_shot_check_judges_a_partial_window() {
        let t = Thresholds::preset(Sensitivity::Medium);
        let w = window_with(&t, (0..10).map(|_| sample(95, 600, REASON_HW_THERMAL)));
        assert!(w.evaluate(&t, true).is_none());
        let (stats, v) = w.evaluate(&t, false).unwrap();
        assert!(v.event);
        assert_eq!(stats.busy_samples, 10);
        assert!((stats.busy_minutes - 20.0 / 60.0).abs() < 1e-9);
    }

    #[test]
    fn window_slides() {
        let t = Thresholds::preset(Sensitivity::High);
        let n = t.window_samples(2.0);
        let mut w = window_with(&t, (0..n).map(|_| sample(95, 500, REASON_HW_THERMAL)));
        assert!(w.evaluate(&t, true).unwrap().1.event);
        for _ in 0..n {
            w.push(sample(95, 1980, 0));
        }
        let (stats, v) = w.evaluate(&t, true).unwrap();
        assert!(!v.event);
        assert_eq!(stats.thermal_fraction, 0.0);
    }
}
