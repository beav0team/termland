//! Animation primitives and the frame clock.
//!
//! A terminal UI does not need animations, but a *desktop* does: a hover that
//! snaps, a menu that appears instantly and a chart that steps once a second
//! read as "a TUI". The difference is entirely in the timing, which means two
//! things have to exist before any of it can be built:
//!
//! * a **clock** whose `dt` is the real elapsed time, clamped so a stall cannot
//!   turn into one visible jump
//! * **motion** functions: easing for things that must arrive in a fixed time
//!   (a menu opening), a spring for things that must feel physical (a pane
//!   resizing, a chart scrolling).
//!
//! Both are pure and unit-tested: motion that cannot be tested is motion that
//! jitters on the one machine you did not try.

use std::time::{Duration, Instant};

/// How long a frame may claim to have taken. A machine that stalls for two
/// seconds (a swap storm, a suspended laptop) must not then run two seconds of
/// animation in one step, which overshoots every spring in the UI and looks
/// like a glitch rather than a catch-up.
pub const MAX_DT: f32 = 0.1;

/// A monotonic clock that reports clamped deltas.
#[derive(Debug)]
pub struct Clock {
    last: Instant,
    elapsed: f32,
}

impl Clock {
    pub fn new() -> Self {
        Self {
            last: Instant::now(),
            elapsed: 0.0,
        }
    }

    /// Advance and return the delta in seconds, clamped to [`MAX_DT`].
    pub fn tick(&mut self) -> f32 {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(MAX_DT);
        self.last = now;
        self.elapsed += dt;
        dt
    }

    /// Seconds since this clock was created; the time base UI animation reads.
    pub fn elapsed(&self) -> f32 {
        self.elapsed
    }

    /// Reset the delta base without losing the time base. Used after the loop
    /// was blocked on something (a pane spawn) so the next frame is not one
    /// enormous step.
    pub fn resync(&mut self) {
        self.last = Instant::now();
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

/// Cubic ease-out: fast at the start, settles gently. The default for anything
/// that appears (menus, overlays, toasts).
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Cubic ease-in-out, for movement that has a start and an end but no urgency.
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

/// Linear interpolation between two values.
pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t.clamp(0.0, 1.0)
}

/// A time-boxed transition from one value to another.
#[derive(Debug, Clone, Copy)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Option<Instant>,
    dur: Duration,
}

impl Tween {
    /// A tween that is already finished and sitting at `value` — the state of
    /// every widget that has never been animated.
    pub fn settled(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start: None,
            dur: Duration::ZERO,
        }
    }

    /// Start moving to `to`. Retargeting mid-flight keeps the current value as
    /// the new origin, so a hover that is reversed halfway does not jump.
    pub fn to(&mut self, to: f32, dur: Duration) {
        self.retarget_at(to, dur, Instant::now());
    }

    /// [`Tween::to`] with the clock supplied, so a test can drive a transition
    /// through an imaginary timeline instead of sleeping.
    pub fn retarget_at(&mut self, to: f32, dur: Duration, now: Instant) {
        let from = self.value_at(now);
        self.from = from;
        self.to = to;
        self.dur = dur;
        self.start = Some(now);
    }

    /// The value right now.
    pub fn value(&self) -> f32 {
        self.value_at(Instant::now())
    }

    /// The value at a given instant; the testable half.
    pub fn value_at(&self, now: Instant) -> f32 {
        let Some(start) = self.start else {
            return self.to;
        };
        let total = self.dur.as_secs_f32();
        if total <= 0.0 {
            return self.to;
        }
        let t = (now - start).as_secs_f32() / total;
        if t >= 1.0 {
            self.to
        } else {
            lerp(self.from, self.to, ease_out(t))
        }
    }

    /// True while the value is still moving, so the loop knows to keep the
    /// fast clock running.
    pub fn running(&self) -> bool {
        match self.start {
            None => false,
            Some(start) => start.elapsed() < self.dur,
        }
    }
}

impl Default for Tween {
    fn default() -> Self {
        Self::settled(0.0)
    }
}

/// A critically-damped-ish spring: overshoots slightly, settles, and behaves
/// the same at any frame rate because it is integrated with a real `dt`.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
    stiffness: f32,
    damping: f32,
}

impl Spring {
    pub fn new(value: f32) -> Self {
        Self {
            value,
            velocity: 0.0,
            target: value,
            stiffness: 120.0,
            damping: 18.0,
        }
    }

    pub fn with_response(mut self, stiffness: f32, damping: f32) -> Self {
        self.stiffness = stiffness;
        self.damping = damping;
        self
    }

    pub fn set(&mut self, target: f32) {
        self.target = target;
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    /// Integrate one step. Returns the new value.
    pub fn step(&mut self, dt: f32) -> f32 {
        let dt = dt.clamp(0.0, MAX_DT);
        // Semi-implicit Euler with a few substeps: stable for the stiffness
        // range the UI uses, and a 16 ms frame is split into steps small
        // enough that a fast spring cannot ring.
        let steps = 4;
        let h = dt / steps as f32;
        for _ in 0..steps {
            let force = (self.target - self.value) * self.stiffness;
            let damp = -self.velocity * self.damping;
            self.velocity += (force + damp) * h;
            self.value += self.velocity * h;
        }
        // Snap when it is close enough that nobody can see the difference: a
        // spring that never *quite* arrives keeps the fast clock on forever.
        if (self.target - self.value).abs() < 0.001 && self.velocity.abs() < 0.01 {
            self.value = self.target;
            self.velocity = 0.0;
        }
        self.value
    }

    pub fn settled(&self) -> bool {
        (self.target - self.value).abs() < 0.001 && self.velocity.abs() < 0.01
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stall_does_not_produce_one_enormous_step() {
        let mut c = Clock::new();
        std::thread::sleep(Duration::from_millis(120));
        let dt = c.tick();
        assert!(dt <= MAX_DT, "dt {dt} was not clamped");
        assert!(dt > 0.0);
    }

    #[test]
    fn easing_functions_hit_both_ends_and_stay_inside_them() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_in_out(0.0), 0.0);
        assert_eq!(ease_in_out(1.0), 1.0);
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            assert!((0.0..=1.0).contains(&ease_out(t)));
            assert!((0.0..=1.0).contains(&ease_in_out(t)));
        }
        // Ease-out is in front of linear for the whole middle of the curve.
        assert!(ease_out(0.5) > 0.5);
    }

    #[test]
    fn a_tween_ends_exactly_on_its_target() {
        let mut t = Tween::settled(0.0);
        t.to(1.0, Duration::from_millis(100));
        assert_eq!(t.value_at(Instant::now() + Duration::from_secs(1)), 1.0);
        assert!(t.running());
        assert_eq!(Tween::settled(0.4).value(), 0.4);
    }

    #[test]
    fn a_retargeted_tween_continues_from_where_it_was() {
        let t0 = Instant::now();
        let mut t = Tween::settled(0.0);
        t.retarget_at(1.0, Duration::from_millis(200), t0);
        let half = t0 + Duration::from_millis(100);
        let mid = t.value_at(half);
        assert!(mid > 0.0 && mid < 1.0, "mid = {mid}");
        // Reverse halfway: it must start from where it is, not from 0 or 1.
        t.retarget_at(0.0, Duration::from_millis(200), half);
        assert!((t.value_at(half) - mid).abs() < 0.001, "retarget jumped");
        let quarter_later = t.value_at(half + Duration::from_millis(50));
        assert!(quarter_later < mid, "it must be heading back down");
    }

    #[test]
    fn a_spring_converges_and_snaps() {
        let mut s = Spring::new(0.0);
        s.set(1.0);
        for _ in 0..600 {
            s.step(1.0 / 60.0);
        }
        assert!(s.settled(), "spring never settled: {}", s.value());
        assert_eq!(s.value(), 1.0);
    }

    #[test]
    fn a_spring_is_frame_rate_independent() {
        let mut slow = Spring::new(0.0);
        let mut fast = Spring::new(0.0);
        slow.set(1.0);
        fast.set(1.0);
        // Slow machine: 20 fps. Fast machine: 60 fps. Same wall-clock time.
        for _ in 0..20 {
            slow.step(0.05);
        }
        for _ in 0..60 {
            fast.step(1.0 / 60.0);
        }
        assert!(
            (slow.value() - fast.value()).abs() < 0.05,
            "20 fps gave {} but 60 fps gave {}",
            slow.value(),
            fast.value()
        );
    }
}
