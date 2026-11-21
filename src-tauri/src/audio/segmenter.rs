use std::collections::VecDeque;

use std::time::{Duration, Instant};

/// WASAPI may stop delivering loopback buffers when the render device goes silent.
pub(crate) struct SilenceClock {
    last_input: Instant,
    accounted: Instant,
}

impl SilenceClock {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            last_input: now,
            accounted: now,
        }
    }

    pub(crate) fn input(&mut self, now: Instant) {
        self.last_input = now;
        self.accounted = now;
    }

    pub(crate) fn missing(&mut self, now: Instant, rate: u32) -> usize {
        if now.duration_since(self.last_input) <= Duration::from_millis(100) {
            return 0;
        }
        let gap = now.duration_since(self.accounted);
        // Five seconds includes the longest configurable pause, even after system sleep.
        let count = (gap.min(Duration::from_secs(5)).as_nanos() * u128::from(rate) / 1_000_000_000)
            as usize;
        if count > 0 {
            self.accounted = if gap > Duration::from_secs(5) {
                now
            } else {
                self.accounted
                    + Duration::from_nanos(count as u64 * 1_000_000_000 / u64::from(rate))
            };
        }
        count
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    Silence,
    Limit,
    Stop,
}

pub(crate) struct Segment {
    pub samples: Vec<f32>,
    pub rate: u32,
    pub ended_at: u64,
    pub reason: Reason,
}

struct Frame {
    samples: Vec<f32>,
    rms: f32,
    voiced: bool,
}
pub(crate) struct Meter {
    pub level: f32,
    pub speaking: bool,
}

pub(crate) struct Segmenter {
    rate: u32,
    silence_ms: u32,
    sensitivity: f32,
    noise: f32,
    frames: VecDeque<Frame>,
    len: usize,
    active: bool,
    quiet: usize,
    history: VecDeque<(usize, f32)>,
    history_len: usize,
    limits: u32,
}

impl Segmenter {
    pub(crate) fn new(rate: u32, silence_ms: u32, sensitivity: f32) -> Self {
        Self {
            rate,
            silence_ms,
            sensitivity,
            noise: 0.003,
            frames: VecDeque::new(),
            len: 0,
            active: false,
            quiet: 0,
            history: VecDeque::new(),
            history_len: 0,
            limits: 0,
        }
    }
    fn samples_for(&self, ms: u32) -> usize {
        (u64::from(self.rate) * u64::from(ms) / 1000) as usize
    }
    fn remember(&mut self, n: usize, rms: f32) {
        self.history.push_back((n, rms));
        self.history_len += n;
        while self
            .history
            .front()
            .is_some_and(|(count, _)| self.history_len - count >= self.samples_for(5000))
        {
            if let Some((len, _)) = self.history.pop_front() {
                self.history_len -= len;
            }
        }
    }
    pub(crate) fn push(&mut self, samples: Vec<f32>, now: u64) -> (Meter, Option<Segment>) {
        let n = samples.len();
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / n.max(1) as f32).sqrt();
        self.remember(n, rms);
        let threshold = (self.noise * 3.).max(0.006) / self.sensitivity;
        let voiced = rms > threshold * if self.active { 0.6 } else { 1. };
        self.frames.push_back(Frame {
            samples,
            rms,
            voiced,
        });
        self.len += n;
        let mut segment = None;
        if self.active || voiced {
            self.active = true;
            self.quiet = if voiced { 0 } else { self.quiet + n };
            if self.quiet >= self.samples_for(self.silence_ms) {
                segment = self.finish(Reason::Silence, now);
            } else if self.len >= self.samples_for(15000) {
                segment = self.cut(now);
            }
        } else {
            let weight = if rms < self.noise { 0.2 } else { 0.02 };
            self.noise += (rms - self.noise) * weight;
            while self
                .frames
                .front()
                .is_some_and(|f| self.len - f.samples.len() >= self.samples_for(300))
            {
                if let Some(frame) = self.frames.pop_front() {
                    self.len -= frame.samples.len();
                }
            }
        }
        (
            Meter {
                level: (rms.sqrt() * 2.2).min(1.),
                speaking: self.active && self.quiet < self.samples_for(250),
            },
            segment,
        )
    }

    fn cut(&mut self, now: u64) -> Option<Segment> {
        let lo = self.samples_for(13500);
        let hi = self.samples_for(15000);
        let mut offset = 0;
        let mut best = None;
        for (i, f) in self.frames.iter().enumerate() {
            offset += f.samples.len();
            if offset >= lo && offset <= hi && best.is_none_or(|(_, r)| f.rms <= r) {
                best = Some((i + 1, f.rms));
            }
        }
        let count = best.map_or(self.frames.len().saturating_sub(1).max(1), |(n, _)| n);
        let after = self.frames.split_off(count);
        let remaining: usize = after.iter().map(|f| f.samples.len()).sum();
        let voiced: usize = self
            .frames
            .iter()
            .filter(|f| f.voiced)
            .map(|f| f.samples.len())
            .sum();
        let samples = self.frames.drain(..).flat_map(|f| f.samples).collect();
        self.frames = after;
        self.len = remaining;
        self.quiet = self
            .frames
            .iter()
            .rev()
            .take_while(|f| !f.voiced)
            .map(|f| f.samples.len())
            .sum();
        self.limits += 1;
        if self.limits >= 2 {
            let mut levels: Vec<_> = self.history.iter().map(|(_, r)| *r).collect();
            levels.sort_unstable_by(f32::total_cmp);
            if let Some(r) = levels.get(levels.len().saturating_sub(1) / 5) {
                self.noise = self.noise.max(*r);
            }
        }
        let end = now.saturating_sub(remaining as u64 * 1000 / u64::from(self.rate));
        (voiced >= self.samples_for(350)).then_some(Segment {
            samples,
            rate: self.rate,
            ended_at: end,
            reason: Reason::Limit,
        })
    }

    pub(crate) fn finish(&mut self, reason: Reason, now: u64) -> Option<Segment> {
        let voiced: usize = self
            .frames
            .iter()
            .filter(|f| f.voiced)
            .map(|f| f.samples.len())
            .sum();
        let trim = self.quiet.saturating_sub(self.samples_for(250));
        let segment = (self.active && voiced >= self.samples_for(350)).then(|| Segment {
            samples: self
                .frames
                .drain(..)
                .flat_map(|f| f.samples)
                .take(self.len.saturating_sub(trim))
                .collect(),
            rate: self.rate,
            ended_at: now.saturating_sub(trim as u64 * 1000 / u64::from(self.rate)),
            reason,
        });
        self.frames.clear();
        self.active = false;
        self.len = 0;
        self.quiet = 0;
        self.limits = 0;
        segment
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn push(seg: &mut Segmenter, level: f32, n: usize) -> Vec<Segment> {
        (0..n)
            .filter_map(|i| seg.push(vec![level; 50], i as u64 * 50).1)
            .collect()
    }
    #[test]
    fn watchdog_finishes_speech_when_loopback_stops() {
        let start = Instant::now();
        let mut clock = SilenceClock::new(start);
        let mut s = Segmenter::new(16000, 700, 1.);
        for _ in 0..5 {
            let (m, out) = s.push(vec![0.25; 2048], 1000);
            assert!(m.speaking && m.level > 0. && out.is_none());
        }
        clock.input(start);
        assert_eq!(clock.missing(start + Duration::from_millis(100), 16000), 0);
        let mut closed = Vec::new();
        for ms in (110..=800).step_by(10) {
            let n = clock.missing(start + Duration::from_millis(ms), 16000);
            if let Some(out) = s.push(vec![0.; n], 1000 + ms).1 {
                closed.push(out);
            }
        }
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].reason, Reason::Silence);
        assert_eq!(closed[0].samples.len(), 5 * 2048 + 4000);
    }
    #[test]
    fn keeps_preroll_and_trims_silence() {
        let mut s = Segmenter::new(1000, 700, 1.);
        push(&mut s, 0., 20);
        push(&mut s, 0.2, 10);
        let out = push(&mut s, 0., 16);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].samples.len(), 300 + 500 + 250);
        assert_eq!(out[0].reason, Reason::Silence);
    }
    #[test]
    fn ignores_click_and_flushes_on_stop() {
        let mut s = Segmenter::new(1000, 700, 1.);
        push(&mut s, 0.4, 4);
        assert!(push(&mut s, 0., 16).is_empty());
        push(&mut s, 0.4, 9);
        let out = s.finish(Reason::Stop, 1234).unwrap();
        assert_eq!(out.ended_at, 1234);
        assert_eq!(out.rate, 1000);
        assert_eq!(out.reason, Reason::Stop);
    }
    #[test]
    fn quiet_cut_and_no_lost_samples() {
        let mut s = Segmenter::new(1000, 700, 1.);
        let mut parts = Vec::new();
        let mut original = Vec::new();
        for i in 0..400 {
            let frame = vec![if i == 280 { 0.02 } else { 0.25 }; 50];
            original.extend_from_slice(&frame);
            if let Some(out) = s.push(frame, i * 50 + 50).1 {
                assert_eq!(out.reason, Reason::Limit);
                assert_eq!(out.samples.len(), 14050);
                parts.extend(out.samples);
            }
        }
        parts.extend(s.finish(Reason::Stop, 20000).unwrap().samples);
        assert_eq!(parts, original);
    }
    #[test]
    fn raises_floor_after_two_limits() {
        let mut s = Segmenter::new(1000, 700, 1.);
        assert_eq!(push(&mut s, 0.1, 600).len(), 2);
        assert!((s.noise - 0.1).abs() < 0.001);
        assert!(push(&mut s, 0.1, 60).is_empty());
    }
}
