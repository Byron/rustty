//! A temporary panel shader's displacement, sampled independently of frame rate.
use rustty::config::ScreenShakeType;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub struct State {
    kind: ScreenShakeType,
    duration: Duration,
    seed: u64,
    started: Instant,
}

impl State {
    pub fn new(kind: ScreenShakeType, duration: Duration, seed: u64, started: Instant) -> Self {
        Self {
            kind,
            duration,
            seed,
            started,
        }
    }

    /// Logical points; `None` retires the shader and restores the original panel.
    pub fn offset(&self, now: Instant) -> Option<[f32; 2]> {
        let elapsed = now.saturating_duration_since(self.started);
        if elapsed >= self.duration {
            return None;
        }
        let phase = elapsed.as_secs_f32() * 24.0 * std::f32::consts::TAU;
        let sine = phase.sin();
        let square = if sine >= 0.0 { 1.0 } else { -1.0 };
        let random = |axis: u64| {
            let step = (elapsed.as_secs_f64() * 30.0) as u64;
            let mut value = self.seed ^ step.wrapping_mul(0x9e3779b97f4a7c15) ^ axis;
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
            value ^= value >> 31;
            value as u32 as f32 / u32::MAX as f32 * 2.0 - 1.0
        };
        use ScreenShakeType::*;
        let displacement = match self.kind {
            Random | RandomDiminishing => [random(0), random(0x9e3779b97f4a7c15)],
            HorizontalSine | HorizontalSineDiminishing => [sine, 0.0],
            HorizontalSquare | HorizontalSquareDiminishing => [square, 0.0],
            SineOffset => [sine, phase.cos()],
        };
        let amplitude = match self.kind {
            RandomDiminishing | HorizontalSineDiminishing | HorizontalSquareDiminishing => {
                4.0 * (1.0 - elapsed.as_secs_f32() / self.duration.as_secs_f32())
            }
            _ => 4.0,
        };
        Some(displacement.map(|value| value * amplitude))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_retires_the_shader_and_retrigger_restarts_it() {
        let now = Instant::now();
        let duration = Duration::from_millis(180);
        let first = State::new(ScreenShakeType::Random, duration, 1, now);
        assert!(first.offset(now).is_some());
        assert!(first.offset(now + duration).is_none());
        assert!(
            State::new(ScreenShakeType::Random, Duration::ZERO, 1, now)
                .offset(now)
                .is_none()
        );
        let second = State::new(ScreenShakeType::Random, duration, 1, now + duration);
        assert_eq!(first.offset(now), second.offset(now + duration));
        assert!(second.offset(now + duration + duration).is_none());
    }

    #[test]
    fn styles_are_bounded_deterministic_and_diminish() {
        use ScreenShakeType::*;
        let now = Instant::now();
        let duration = Duration::from_secs(1);
        let styles = [
            Random,
            RandomDiminishing,
            HorizontalSine,
            HorizontalSineDiminishing,
            HorizontalSquare,
            HorizontalSquareDiminishing,
            SineOffset,
        ];
        for kind in styles {
            let state = State::new(kind, duration, 17, now);
            for millis in 0..1000 {
                let sample = now + Duration::from_millis(millis);
                let offset = state.offset(sample).unwrap();
                assert_eq!(offset, state.offset(sample).unwrap());
                assert!(
                    offset
                        .into_iter()
                        .all(|value| value.is_finite() && value.abs() <= 4.0)
                );
                if matches!(
                    kind,
                    HorizontalSine
                        | HorizontalSineDiminishing
                        | HorizontalSquare
                        | HorizontalSquareDiminishing
                ) {
                    assert_eq!(offset[1], 0.0);
                }
            }
        }
        let sample = now + Duration::from_millis(375);
        for (constant, diminishing) in [
            (Random, RandomDiminishing),
            (HorizontalSine, HorizontalSineDiminishing),
            (HorizontalSquare, HorizontalSquareDiminishing),
        ] {
            let constant = State::new(constant, duration, 17, now)
                .offset(sample)
                .unwrap();
            let diminishing = State::new(diminishing, duration, 17, now)
                .offset(sample)
                .unwrap();
            assert_eq!(diminishing, constant.map(|value| value * 0.625));
        }
        let sample = now + Duration::from_millis(10);
        assert_ne!(
            State::new(HorizontalSine, duration, 17, now).offset(sample),
            State::new(HorizontalSquare, duration, 17, now).offset(sample)
        );
        assert_ne!(
            State::new(SineOffset, duration, 17, now).offset(sample),
            State::new(HorizontalSine, duration, 17, now).offset(sample)
        );
    }
}
