// SPDX-License-Identifier: MIT
//
// Scene luminance -> the float VRChat gets. Two ideas, both borrowed from the
// real eye:
//
//   - response is logarithmic. Doubling the light moves the pupil by a fixed
//     amount, whether you go from a dim room to a lit one or from daylight to
//     snow. Working in stops (log2 of luminance) is what makes the whole usable
//     range fit in one parameter instead of the top 10% of it.
//   - response is asymmetric. Light hits and the pupil constricts in a fraction
//     of a second; darkness falls and it opens over several. Using one time
//     constant for both is the detail that reads as fake.
//
// Windows-free and side-effect-free, so all of it is tested.

/// Tuning, straight from the config file.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    /// Luminance in stops (log2) that maps to the smallest pupil (-1).
    pub bright_stops: f32,
    /// Luminance in stops that maps to the widest pupil (+1). Below bright_stops.
    pub dark_stops: f32,
    /// Time constant while the pupil shrinks, seconds.
    pub constrict_seconds: f32,
    /// Time constant while it widens, seconds.
    pub dilate_seconds: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            // Chosen against the range VRChat actually produces, not against the
            // eye's full 10-decade range: a well-exposed scene averages ~18% of
            // white (-2.5 stops), a bright world ~0.5 (-1), a dark one ~0.01
            // (-6.6). Spanning -1..-7 puts ordinary scenes across the middle of
            // the parameter. A wider span looks broken in practice — the pupil
            // sits near fully constricted and barely moves.
            bright_stops: -1.0,
            dark_stops: -7.0,
            constrict_seconds: 0.3,
            dilate_seconds: 4.0,
        }
    }
}

/// Luminance below this reads as pitch black. Guards log2(0) and stops sensor
/// noise near zero from swinging the pupil.
const BLACK_FLOOR: f32 = 1e-6;

/// Where the pupil would settle for this luminance, in VRChat's -1..1 float
/// range: -1 smallest (bright), +1 widest (dark).
pub fn target(luminance: f32, p: &Params) -> f32 {
    let span = p.dark_stops - p.bright_stops;
    if span.abs() < f32::EPSILON {
        // Degenerate config: refuse to divide, sit in the middle.
        return 0.0;
    }
    let stops = luminance.max(BLACK_FLOOR).log2();
    let t = ((stops - p.bright_stops) / span).clamp(0.0, 1.0);
    t * 2.0 - 1.0
}

/// The pupil itself: holds where it currently is and eases toward the target.
#[derive(Debug, Clone, Copy)]
pub struct Pupil {
    value: f32,
}

impl Pupil {
    /// Starts mid-range, so the first frames ease into place from somewhere
    /// plausible rather than snapping from an extreme.
    pub fn new() -> Self {
        Self { value: 0.0 }
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    /// Advances by `dt` seconds toward what `luminance` calls for, and returns
    /// the new value. The exponential form makes the result depend on elapsed
    /// time rather than on how often this is called, so a dropped frame or a
    /// change of capture rate does not change how fast the pupil moves.
    pub fn step(&mut self, luminance: f32, dt: f32, p: &Params) -> f32 {
        let target = target(luminance, p);

        // Moving toward -1 is the pupil closing down: that is the fast one.
        let tau = if target < self.value {
            p.constrict_seconds
        } else {
            p.dilate_seconds
        };

        let alpha = if tau <= 0.0 || dt <= 0.0 {
            // No inertia asked for (or no time passed): jump.
            if dt <= 0.0 {
                0.0
            } else {
                1.0
            }
        } else {
            1.0 - (-dt / tau).exp()
        };

        self.value += (target - self.value) * alpha;
        self.value = self.value.clamp(-1.0, 1.0);
        self.value
    }
}

impl Default for Pupil {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Params {
        Params::default()
    }

    #[test]
    fn bright_closes_the_pupil_and_dark_opens_it() {
        let p = params();
        // Well above bright_stops (-1 stop == 0.5 linear).
        assert!((target(1.0, &p) - -1.0).abs() < 1e-6);
        // Well below dark_stops (-7 stops == 0.0078 linear).
        assert!((target(0.0, &p) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn response_is_monotonic_in_luminance() {
        let p = params();
        let mut previous = f32::INFINITY;
        for i in 0..64 {
            let luminance = (i as f32 / 63.0).powi(3);
            let v = target(luminance, &p);
            assert!(v <= previous + 1e-6, "target must never rise with light");
            previous = v;
        }
    }

    #[test]
    fn a_doubling_of_light_moves_the_pupil_by_a_constant() {
        // The signature of a log response: equal ratios, equal steps.
        let p = params();
        let a = target(0.01, &p) - target(0.02, &p);
        let b = target(0.02, &p) - target(0.04, &p);
        assert!((a - b).abs() < 1e-5, "{a} vs {b}");
    }

    #[test]
    fn the_default_range_spreads_real_scenes_across_the_parameter() {
        // The defaults are only good if ordinary VRChat brightness uses the whole
        // -1..1 range. Guards against the tempting-but-wrong "cover the eye's
        // full dynamic range", which pins the pupil shut for everything but a
        // pitch-black world.
        let p = params();
        let bright_world = target(0.5, &p);
        let well_exposed = target(0.18, &p);
        let dim = target(0.03, &p);
        let dark_world = target(0.01, &p);

        assert!(
            bright_world <= -0.95,
            "a bright world should close it fully"
        );
        assert!(
            (-0.75..=-0.25).contains(&well_exposed),
            "an average scene should sit mid-close, got {well_exposed}"
        );
        assert!(dim > 0.0, "dim should already be past neutral, got {dim}");
        assert!(
            dark_world >= 0.75,
            "a dark world should open it wide, got {dark_world}"
        );
    }

    #[test]
    fn constriction_outruns_dilation() {
        let p = params();
        // From the middle, one second of full dark vs one second of full light.
        let mut opening = Pupil::new();
        opening.step(0.0, 1.0, &p);
        let mut closing = Pupil::new();
        closing.step(1.0, 1.0, &p);

        let opened = opening.value() - 0.0;
        let closed = 0.0 - closing.value();
        assert!(
            closed > opened * 2.0,
            "closing ({closed}) should badly outpace opening ({opened})"
        );
    }

    #[test]
    fn movement_depends_on_elapsed_time_not_on_frame_count() {
        let p = params();
        let mut coarse = Pupil::new();
        coarse.step(1.0, 1.0, &p);

        let mut fine = Pupil::new();
        for _ in 0..100 {
            fine.step(1.0, 0.01, &p);
        }
        assert!(
            (coarse.value() - fine.value()).abs() < 1e-3,
            "1x1.0s {} vs 100x0.01s {}",
            coarse.value(),
            fine.value()
        );
    }

    #[test]
    fn settles_on_the_target_and_stays_in_range() {
        let p = params();
        let mut pupil = Pupil::new();
        for _ in 0..2000 {
            pupil.step(0.0, 0.1, &p);
        }
        assert!((pupil.value() - 1.0).abs() < 1e-3);

        for _ in 0..2000 {
            let v = pupil.step(1.0, 0.1, &p);
            assert!((-1.0..=1.0).contains(&v));
        }
        assert!((pupil.value() - -1.0).abs() < 1e-3);
    }

    #[test]
    fn degenerate_params_do_not_divide_by_zero() {
        let p = Params {
            bright_stops: -5.0,
            dark_stops: -5.0,
            ..Params::default()
        };
        assert_eq!(target(0.5, &p), 0.0);
    }

    #[test]
    fn a_zero_length_step_changes_nothing() {
        let p = params();
        let mut pupil = Pupil::new();
        let before = pupil.value();
        assert_eq!(pupil.step(1.0, 0.0, &p), before);
    }
}
