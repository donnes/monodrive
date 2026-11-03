//! Frame-driven motion, ported from Monocode's `pane_motion.rs`. A critically
//! damped spring gives a quick response and a quiet finish, and keeps its
//! velocity when a slide reverses partway.
use std::{cell::Cell, time::Instant};

pub(crate) struct Slide {
    from: f32,
    to: f32,
    velocity: f32,
    frequency: f32,
    pixels_per_unit: f32,
    limits: (f32, f32),
    start: Cell<Option<Instant>>,
}

impl Slide {
    /// From `from` to `to`, a fraction or a width, over `distance` points.
    pub(crate) fn new(from: f32, to: f32, distance: f32) -> Self {
        let pace = (distance.abs() / 360.).sqrt().clamp(1., 1.35);
        let opening = to > from;
        Self {
            from,
            to,
            velocity: (to - from) * if opening { 12. } else { 13.5 } / pace,
            frequency: if opening { 34. } else { 40. } / pace,
            pixels_per_unit: if (to - from).abs() > f32::EPSILON {
                (distance / (to - from)).abs().max(1.)
            } else {
                1.
            },
            limits: (from.min(to), from.max(to)),
            start: Cell::new(None),
        }
    }

    /// Heads for `to` from wherever the slide is at `now`, at its speed.
    pub(crate) fn retarget(&self, to: f32, now: Instant) -> Self {
        let (from, velocity, _) = self.sample(now);
        let distance = (to - from) * self.pixels_per_unit;
        let mut next = Self::new(from, to, distance);
        next.velocity = if distance.abs() < 0.2 { 0. } else { velocity };
        next.pixels_per_unit = self.pixels_per_unit;
        next.limits = (self.limits.0.min(to), self.limits.1.max(to));
        next
    }

    /// The value at `now`, and whether it has settled.
    pub(crate) fn at(&self, now: Instant) -> (f32, bool) {
        let (value, _, done) = self.sample(now);
        (value, done)
    }

    fn sample(&self, now: Instant) -> (f32, f32, bool) {
        // The clock starts at the first frame drawn, so a slow frame before it
        // can't skip the opening.
        let start = self.start.get().unwrap_or(now);
        self.start.set(Some(start));
        let t = now.saturating_duration_since(start).as_secs_f32();
        let displacement = self.from - self.to;
        let c = self.velocity + self.frequency * displacement;
        let decay = (-self.frequency * t).exp();
        let value = self.to + (displacement + c * t) * decay;
        let velocity = (self.velocity - self.frequency * c * t) * decay;
        let done = (value - self.to).abs() * self.pixels_per_unit < 0.2
            && velocity.abs() * self.pixels_per_unit < 5.;
        if done {
            return (self.to, 0., true);
        }
        if value < self.limits.0 {
            return (self.limits.0, velocity.max(0.), false);
        }
        if value > self.limits.1 {
            return (self.limits.1, velocity.min(0.), false);
        }
        (value, velocity, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn starts_at_first_frame_and_settles_without_overshoot_at_any_refresh_rate() {
        for hz in [60, 90, 120, 144] {
            for distance in [220., 360., 640.] {
                for opening in [true, false] {
                    let (from, to) = if opening {
                        (0., distance)
                    } else {
                        (distance, 0.)
                    };
                    let slide = Slide::new(from, to, distance);
                    let start = Instant::now() + Duration::from_secs(1);
                    assert_eq!(slide.at(start), (from, false));
                    let mut previous = from;
                    let mut finished = false;
                    for frame in 1..hz {
                        let (value, done) =
                            slide.at(start + Duration::from_secs_f32(frame as f32 / hz as f32));
                        assert!((0. ..=distance).contains(&value));
                        assert!(if opening {
                            value >= previous
                        } else {
                            value <= previous
                        });
                        previous = value;
                        if done {
                            assert_eq!(value, to);
                            finished = true;
                            break;
                        }
                    }
                    assert!(finished);
                }
            }
        }
    }

    #[test]
    fn reversal_keeps_position_and_velocity_then_settles() {
        let slide = Slide::new(0., 360., 360.);
        let start = Instant::now();
        slide.at(start);
        let now = start + Duration::from_millis(70);
        let (position, velocity, _) = slide.sample(now);
        let reversed = slide.retarget(0., now);
        let (next, next_velocity, _) = reversed.sample(now);
        assert!((next - position).abs() < 0.001);
        assert!((next_velocity - velocity).abs() < 0.001);
        assert_eq!(reversed.at(now + Duration::from_secs(1)), (0., true));
    }
}
