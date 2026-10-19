//! The colours the page and its text can be: one run of reading colours from black to white,
//! through night blues, warm charcoal, sepia, old paper and cream. Each slider picks a point
//! along it, so dark text on cream and light text on night are both a slide away.

pub type Rgb = [u8; 3];

/// Where each colour sits along the run, from 0 to 1, darkest first.
const STOPS: [(f64, Rgb); 10] = [
    (0.00, [0x00, 0x00, 0x00]),
    (0.10, [0x16, 0x16, 0x1e]),
    (0.22, [0x1e, 0x24, 0x30]),
    (0.34, [0x2b, 0x2a, 0x27]),
    (0.46, [0x4a, 0x3f, 0x33]),
    (0.58, [0x8b, 0x7b, 0x65]),
    (0.70, [0xc9, 0xb9, 0x9a]),
    (0.80, [0xe9, 0xdf, 0xc7]),
    (0.90, [0xf7, 0xf1, 0xe3]),
    (1.00, [0xff, 0xff, 0xff]),
];

/// The colour at `at` along the run, clamped to its ends.
pub fn at(at: f64) -> Rgb {
    let at = if at.is_finite() {
        at.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let after = STOPS
        .iter()
        .position(|(stop, _)| *stop >= at)
        .unwrap_or(STOPS.len() - 1);
    if after == 0 {
        return STOPS[0].1;
    }
    let (from_at, from) = STOPS[after - 1];
    let (to_at, to) = STOPS[after];
    mix(from, to, (at - from_at) / (to_at - from_at))
}

/// The point along the run whose colour is closest to `colour`: where a slider sits while the
/// reader follows the theme.
pub fn nearest(colour: Rgb) -> f64 {
    const STEPS: u32 = 400;
    let distance = |other: Rgb| -> i32 {
        (0..3)
            .map(|i| (colour[i] as i32 - other[i] as i32).pow(2))
            .sum()
    };
    (0..=STEPS)
        .map(|step| step as f64 / STEPS as f64)
        .min_by_key(|point| distance(at(*point)))
        .unwrap_or(0.0)
}

/// `from` taken `amount` of the way to `to`.
pub fn mix(from: Rgb, to: Rgb, amount: f64) -> Rgb {
    let amount = amount.clamp(0.0, 1.0);
    std::array::from_fn(|i| {
        (from[i] as f64 + (to[i] as f64 - from[i] as f64) * amount).round() as u8
    })
}

pub fn hex(colour: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", colour[0], colour[1], colour[2])
}

/// The colour with an alpha, for CSS.
pub fn rgba(colour: Rgb, alpha: f64) -> String {
    format!(
        "rgba({}, {}, {}, {alpha:.2})",
        colour[0], colour[1], colour[2]
    )
}

/// The whole run as a CSS gradient, darkest at the bottom, for a slider's track.
pub fn gradient() -> String {
    let stops: Vec<String> = STOPS
        .iter()
        .map(|(at, colour)| format!("{} {:.0}%", hex(*colour), at * 100.0))
        .collect();
    format!("linear-gradient(to top, {})", stops.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ends_are_black_and_white() {
        assert_eq!(at(0.0), [0, 0, 0]);
        assert_eq!(at(1.0), [255, 255, 255]);
    }

    #[test]
    fn past_the_ends_stays_at_them() {
        assert_eq!(at(-3.0), [0, 0, 0]);
        assert_eq!(at(7.0), [255, 255, 255]);
        assert_eq!(at(f64::NAN), [0, 0, 0]);
    }

    #[test]
    fn a_stop_is_its_own_colour() {
        assert_eq!(at(0.80), [0xe9, 0xdf, 0xc7]);
    }

    #[test]
    fn between_stops_is_a_mix_of_the_two() {
        let half = at(0.95);
        assert_eq!(half, mix([0xf7, 0xf1, 0xe3], [0xff, 0xff, 0xff], 0.5));
    }

    #[test]
    fn the_run_only_gets_lighter() {
        let luma = |c: Rgb| c[0] as u32 * 3 + c[1] as u32 * 6 + c[2] as u32;
        let mut last = 0;
        for step in 0..=100 {
            let now = luma(at(step as f64 / 100.0));
            assert!(now >= last, "darker at {step}%");
            last = now;
        }
    }

    #[test]
    fn the_nearest_point_finds_a_colour_on_the_run() {
        assert_eq!(nearest([0, 0, 0]), 0.0);
        assert_eq!(nearest([255, 255, 255]), 1.0);
        assert!((nearest(at(0.8)) - 0.8).abs() < 0.01);
        // A theme's dark grey lands near the dark end.
        assert!(nearest([0x1a, 0x1b, 0x26]) < 0.2);
    }

    #[test]
    fn hex_is_lower_case_with_a_hash() {
        assert_eq!(hex([0xab, 0x01, 0xff]), "#ab01ff");
    }
}
