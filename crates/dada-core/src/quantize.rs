//! Kvantálás: a zenei rácshoz igazított indítás.

/// A lehetséges kvantálási értékek (negyedben kifejezve).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantize {
    Off,
    Sixteenth,
    Eighth,
    Quarter,
    Beat,
    TwoBeats,
    FourBeats,
    EightBeats,
    SixteenBeats,
    ThirtyTwoBeats,
    Bar,
    TwoBars,
    FourBars,
    EightBars,
}

impl Quantize {
    /// A rács mérete negyedben; `None` = nincs kvantálás.
    pub fn grid_beats(self) -> Option<f64> {
        use Quantize::*;
        Some(match self {
            Off => return None,
            Sixteenth => 0.25,
            Eighth => 0.5,
            Quarter => 0.75,
            Beat => 1.0,
            TwoBeats => 2.0,
            FourBeats => 4.0,
            EightBeats => 8.0,
            SixteenBeats => 16.0,
            ThirtyTwoBeats => 32.0,
            Bar => 4.0,
            TwoBars => 8.0,
            FourBars => 16.0,
            EightBars => 32.0,
        })
    }

    /// Szöveges megnevezés (UI-hoz, projektfájlhoz).
    pub fn as_str(self) -> &'static str {
        use Quantize::*;
        match self {
            Off => "off",
            Sixteenth => "1/16",
            Eighth => "1/8",
            Quarter => "1/4",
            Beat => "1",
            TwoBeats => "2",
            FourBeats => "4",
            EightBeats => "8",
            SixteenBeats => "16",
            ThirtyTwoBeats => "32",
            Bar => "1bar",
            TwoBars => "2bar",
            FourBars => "4bar",
            EightBars => "8bar",
        }
    }
}

impl Default for Quantize {
    fn default() -> Self {
        Quantize::Beat
    }
}

/// A következő rácsvonal (vagy ugyanaz, ha pont rajta vagyunk).
///
/// Ha `grid <= 0`, akkor a bemenetet adja vissza (nincs kvantálás).
pub fn next_grid(beats: f64, grid: f64) -> f64 {
    if grid <= 0.0 {
        return beats;
    }
    (beats / grid).ceil() * grid
}

/// A következő rácsvonal, ami szigorúan nagyobb, mint `beats`.
///
/// Ezt használjuk indításkor: ha pont egy rácsvonalon állunk, a *következőre*
/// ütemezzük a váltást, nem azonnalira.
pub fn next_grid_strict(beats: f64, grid: f64) -> f64 {
    if grid <= 0.0 {
        return beats;
    }
    let next = next_grid(beats, grid);
    if (next - beats).abs() < 1e-9 {
        next + grid
    } else {
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_grid_rounds_up() {
        assert!((next_grid(0.1, 1.0) - 1.0).abs() < 1e-9);
        assert!((next_grid(1.0, 1.0) - 1.0).abs() < 1e-9);
        assert!((next_grid(3.7, 4.0) - 4.0).abs() < 1e-9);
        assert!((next_grid(4.1, 4.0) - 8.0).abs() < 1e-9);
        assert!((next_grid(5.0, 0.5) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn strict_skips_current_line() {
        assert!((next_grid_strict(1.0, 1.0) - 2.0).abs() < 1e-9);
        assert!((next_grid_strict(0.9, 1.0) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn off_grid_is_noop() {
        assert_eq!(next_grid(1.234, 0.0), 1.234);
    }

    #[test]
    fn quantize_grid_values() {
        assert_eq!(Quantize::Off.grid_beats(), None);
        assert_eq!(Quantize::Sixteenth.grid_beats(), Some(0.25));
        assert_eq!(Quantize::Bar.grid_beats(), Some(4.0));
    }
}
