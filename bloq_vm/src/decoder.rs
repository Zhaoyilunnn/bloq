//! Seeded mock streaming decoder for dynamic VM runs.
//!
//! This is deliberately a policy model, not a decoder. Each causal solve samples
//! an acceptance decision and a residual output-bit error. The raw parity is
//! treated as nominal truth; this model does not infer a physical correction
//! from syndrome data. Accepted solves default to a 0.1% residual error and
//! rejected solves to 10%. The confidence GAP is sampled above or below the
//! configured threshold to match that decision.
//! A measurement-timed solve becomes ready after the configured number of local
//! memory rounds following its last input measurement. Factory solves use the
//! explicit deadline reached after the runtime executes the configured physical
//! GAP rounds. Repeated requests with the same [`DecodeKey`] return the cached
//! decision, keeping corrected and flip outputs causally identical. Confidence
//! acceptance does not itself restart a region; its explicit predicate owns that decision.

use std::collections::HashMap;

use rand::{RngExt, SeedableRng, rngs::StdRng};
use serde::{Deserialize, Serialize};

/// Identifies one observable solve in one dynamic attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DecodeKey {
    /// Dense, globally scoped VM observable task id.
    pub observable_task: u32,
    /// Runtime attempt epoch. Zero is the top-level execution.
    pub attempt: u64,
}

/// Tunable assumptions for the mock decoder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MockDecoderConfig {
    /// Probability that the confidence policy accepts a solve.
    pub acceptance_probability: f64,
    /// Probability that an accepted solve leaves no synthetic residual error.
    pub accepted_accuracy: f64,
    /// Probability that a rejected solve leaves no synthetic residual error.
    pub rejected_accuracy: f64,
    /// Confidence GAP at which policy acceptance begins.
    pub gap_threshold: f64,
    /// Optional global deterministic solve prefix for retry tests and examples.
    /// Unlike seeded random draws, this deliberate override follows execution
    /// order across factories.
    pub acceptance_script: Vec<bool>,
}

impl Default for MockDecoderConfig {
    fn default() -> Self {
        Self {
            acceptance_probability: 0.8,
            accepted_accuracy: 0.999,
            rejected_accuracy: 0.9,
            gap_threshold: 0.5,
            acceptance_script: Vec::new(),
        }
    }
}

impl MockDecoderConfig {
    fn validate(&self) -> Result<(), DecoderError> {
        for (name, value) in [
            ("acceptance_probability", self.acceptance_probability),
            ("accepted_accuracy", self.accepted_accuracy),
            ("rejected_accuracy", self.rejected_accuracy),
        ] {
            if !(0.0..=1.0).contains(&value) {
                return Err(DecoderError::InvalidProbability { name, value });
            }
        }
        if !(0.0 < self.gap_threshold && self.gap_threshold < 1.0) {
            return Err(DecoderError::InvalidGapThreshold(self.gap_threshold));
        }
        Ok(())
    }
}

/// One inspectable mock-decoder result.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DecoderDecision {
    /// Observable and attempt sharing this causal solve.
    pub key: DecodeKey,
    /// Raw observable parity supplied to the decoder.
    pub raw: bool,
    /// Synthetic residual decoder error. XOR this with `raw`.
    pub flip: bool,
    /// Sampled confidence-policy acceptance; diagnostic, not a retry trigger.
    pub accepted: bool,
    /// Sampled confidence GAP in `[0, 1]`.
    pub confidence_gap: f64,
    /// Time of the last contributing measurement.
    pub measurements_ready_at: Option<f64>,
    /// Time at which this decision may be consumed.
    pub ready_at: f64,
}

/// Failures at the mock-decoder boundary.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DecoderError {
    /// Probability-like configuration must be finite and in `[0, 1]`.
    #[error("{name} must be in [0, 1], got {value}")]
    InvalidProbability {
        /// Invalid field name.
        name: &'static str,
        /// Invalid value.
        value: f64,
    },
    /// Confidence threshold must separate accepted and rejected GAP ranges.
    #[error("gap_threshold must be strictly between 0 and 1, got {0}")]
    InvalidGapThreshold(f64),
    /// A duration was negative, non-finite, or overflowed the VM clock.
    #[error("invalid decoder timing")]
    InvalidTiming,
    /// One causal key was requested with different immutable inputs.
    #[error("decoder solve {key:?} was requested with inconsistent inputs")]
    InconsistentRequest {
        /// Reused causal key.
        key: DecodeKey,
    },
}

/// Seeded streaming policy model with one cached result per causal solve.
#[derive(Debug)]
pub struct MockStreamingDecoder {
    config: MockDecoderConfig,
    seed: u64,
    latency_rounds: u32,
    decisions: HashMap<DecodeKey, DecoderDecision>,
    solves: usize,
}

impl MockStreamingDecoder {
    /// Build a reproducible mock decoder.
    ///
    /// # Errors
    ///
    /// Returns [`DecoderError::InvalidProbability`] or
    /// [`DecoderError::InvalidGapThreshold`] for invalid assumptions.
    pub fn new(
        config: MockDecoderConfig,
        seed: u64,
        latency_rounds: u32,
    ) -> Result<Self, DecoderError> {
        config.validate()?;
        Ok(Self {
            config,
            seed,
            latency_rounds,
            decisions: HashMap::new(),
            solves: 0,
        })
    }

    /// Request or reuse one causal solve.
    ///
    /// `round_duration` is the local seam memory-round duration, rather than a
    /// global gate unit. A measurement-backed solve is ready at
    /// `measurements_ready_at + latency_rounds * round_duration`, or immediately
    /// when a later request arrives after that deadline. A constant solve with
    /// no contributing measurement is ready immediately at `requested_at`.
    ///
    /// # Errors
    ///
    /// Returns [`DecoderError::InvalidTiming`] if the ready deadline is invalid,
    /// or [`DecoderError::InconsistentRequest`] if `key` is reused for a
    /// different raw parity or deadline.
    pub fn request(
        &mut self,
        key: DecodeKey,
        raw: bool,
        measurements_ready_at: Option<f64>,
        requested_at: f64,
        round_duration: f64,
    ) -> Result<DecoderDecision, DecoderError> {
        if !requested_at.is_finite()
            || requested_at < 0.0
            || !round_duration.is_finite()
            || round_duration < 0.0
            || measurements_ready_at.is_some_and(|time| !time.is_finite() || time < 0.0)
        {
            return Err(DecoderError::InvalidTiming);
        }
        let ready_at = self.decisions.get(&key).map_or_else(
            || match measurements_ready_at {
                Some(measured_at) => (measured_at
                    + round_duration * f64::from(self.latency_rounds))
                .max(requested_at),
                None => requested_at,
            },
            |decision| decision.ready_at,
        );
        self.request_at(key, raw, measurements_ready_at, ready_at)
    }

    /// Request a solve with an explicit causal deadline.
    ///
    /// Factory GAP decisions use this after their required physical QEC rounds;
    /// ordinary readouts should use [`Self::request`] to derive the deadline
    /// from the measurement cut.
    ///
    /// # Errors
    ///
    /// Returns [`DecoderError::InvalidTiming`] for a negative or non-finite
    /// timestamp, or a deadline before its measurement cut, or
    /// [`DecoderError::InconsistentRequest`] when a cached causal solve is
    /// requested with different inputs.
    pub fn request_at(
        &mut self,
        key: DecodeKey,
        raw: bool,
        measurements_ready_at: Option<f64>,
        ready_at: f64,
    ) -> Result<DecoderDecision, DecoderError> {
        if !ready_at.is_finite()
            || ready_at < 0.0
            || measurements_ready_at.is_some_and(|time| !time.is_finite() || time < 0.0)
            || ready_at < measurements_ready_at.unwrap_or(0.0)
        {
            return Err(DecoderError::InvalidTiming);
        }
        if let Some(&decision) = self.decisions.get(&key) {
            if decision.raw != raw
                || decision.measurements_ready_at != measurements_ready_at
                || (measurements_ready_at.is_some() && decision.ready_at != ready_at)
            {
                return Err(DecoderError::InconsistentRequest { key });
            }
            return Ok(decision);
        }

        // One causal solve owns one random stream, so a retry never advances
        // unrelated decoder randomness.
        let mut rng = StdRng::seed_from_u64(
            self.seed
                ^ u64::from(key.observable_task).wrapping_mul(0x9e37_79b9_7f4a_7c15)
                ^ key.attempt.wrapping_mul(0xbf58_476d_1ce4_e5b9),
        );
        let accepted = self
            .config
            .acceptance_script
            .get(self.solves)
            .copied()
            .unwrap_or_else(|| rng.random::<f64>() < self.config.acceptance_probability);
        self.solves += 1;
        let confidence_gap = if accepted {
            self.config.gap_threshold + rng.random::<f64>() * (1.0 - self.config.gap_threshold)
        } else {
            rng.random::<f64>() * self.config.gap_threshold
        };
        let accuracy = if accepted {
            self.config.accepted_accuracy
        } else {
            self.config.rejected_accuracy
        };
        let decision = DecoderDecision {
            key,
            raw,
            flip: rng.random::<f64>() >= accuracy,
            accepted,
            confidence_gap,
            measurements_ready_at,
            ready_at,
        };
        self.decisions.insert(key, decision);
        Ok(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solve_is_cached_and_uses_configured_latency() {
        let mut decoder = MockStreamingDecoder::new(
            MockDecoderConfig {
                acceptance_script: vec![false, true],
                accepted_accuracy: 1.0,
                rejected_accuracy: 1.0,
                ..MockDecoderConfig::default()
            },
            7,
            2,
        )
        .unwrap();
        let first = decoder
            .request(
                DecodeKey {
                    observable_task: 3,
                    attempt: 1,
                },
                true,
                Some(12.0),
                12.0,
                6.0,
            )
            .unwrap();
        assert!(!first.accepted);
        assert_eq!(first.ready_at, 24.0);
        assert_eq!(
            decoder
                .request(first.key, true, Some(12.0), 20.0, 6.0)
                .unwrap(),
            first
        );
        assert_eq!(
            decoder
                .request(first.key, true, Some(12.0), 100.0, 6.0)
                .unwrap(),
            first
        );
        assert!(
            decoder
                .request(
                    DecodeKey {
                        observable_task: 3,
                        attempt: 2,
                    },
                    true,
                    Some(20.0),
                    20.0,
                    6.0,
                )
                .unwrap()
                .accepted
        );
        assert_eq!(
            decoder
                .request_at(
                    DecodeKey {
                        observable_task: 4,
                        attempt: 9,
                    },
                    false,
                    Some(30.0),
                    90.0,
                )
                .unwrap()
                .ready_at,
            90.0
        );
        assert_eq!(
            decoder
                .request(
                    DecodeKey {
                        observable_task: 5,
                        attempt: 0,
                    },
                    false,
                    Some(12.0),
                    100.0,
                    6.0,
                )
                .unwrap()
                .ready_at,
            100.0
        );
    }

    #[test]
    fn zero_latency_is_immediate_and_timing_overflow_is_typed() {
        let key = DecodeKey {
            observable_task: 3,
            attempt: 1,
        };
        let mut immediate = MockStreamingDecoder::new(MockDecoderConfig::default(), 7, 0).unwrap();
        assert_eq!(
            immediate
                .request(key, true, Some(12.0), 12.0, 6.0)
                .unwrap()
                .ready_at,
            12.0
        );

        let mut overflowing =
            MockStreamingDecoder::new(MockDecoderConfig::default(), 7, 2).unwrap();
        assert_eq!(
            overflowing.request(key, true, Some(12.0), 12.0, f64::MAX),
            Err(DecoderError::InvalidTiming)
        );
    }

    #[test]
    fn explicit_deadlines_reject_invalid_timestamps_before_sampling() {
        let key = DecodeKey {
            observable_task: 0,
            attempt: 0,
        };
        let mut decoder = MockStreamingDecoder::new(
            MockDecoderConfig {
                acceptance_script: vec![false, true],
                ..MockDecoderConfig::default()
            },
            7,
            0,
        )
        .unwrap();
        for invalid in [-1.0, f64::NAN, f64::NEG_INFINITY, f64::INFINITY] {
            assert_eq!(
                decoder.request_at(key, false, Some(invalid), 1.0),
                Err(DecoderError::InvalidTiming)
            );
            assert_eq!(
                decoder.request_at(key, false, None, invalid),
                Err(DecoderError::InvalidTiming)
            );
        }
        assert_eq!(
            decoder.request_at(key, false, Some(2.0), 1.0),
            Err(DecoderError::InvalidTiming)
        );
        let decision = decoder.request_at(key, false, Some(0.0), 0.0).unwrap();
        assert!(!decision.accepted);
        assert_eq!(
            decoder.request_at(key, false, Some(0.0), 0.0).unwrap(),
            decision
        );
    }
}
