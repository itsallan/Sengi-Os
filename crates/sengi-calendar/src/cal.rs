//! Dates, by hand: leap years, the length of a month, the weekday of a day
//! and the grid of a month as a calendar draws it. Proleptic Gregorian, as
//! every clock is.

/// The day a week starts on, from `appearance.week-start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WeekStart {
    #[default]
    Monday,
    Sunday,
    Saturday,
}

impl WeekStart {
    /// What the setting says; anything else is Monday.
    pub fn parse(text: &str) -> WeekStart {
        match text {
            "sunday" => WeekStart::Sunday,
            "saturday" => WeekStart::Saturday,
            _ => WeekStart::Monday,
        }
    }

    /// The weekday it is, counted from Monday as 0.
    fn index(self) -> u32 {
        match self {
            WeekStart::Monday => 0,
            WeekStart::Saturday => 5,
            WeekStart::Sunday => 6,
        }
    }
}

/// A day of the month grid, with the month it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    /// In the month being shown, not one of the days around it.
    pub inside: bool,
}

pub const WEEKDAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

pub const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Six rows of seven: the most any month needs, so the popup never
/// changes height.
pub const CELLS: usize = 42;

pub fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// 1 to 12.
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01, which was a Thursday.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    // Years counted from March, so the leap day is the last of the year.
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let m = i64::from(month);
    let day_of_year = (153 * ((m + 9) % 12) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// 0 for Monday to 6 for Sunday.
pub fn weekday(year: i32, month: u32, day: u32) -> u32 {
    (days_from_civil(year, month, day) + 3).rem_euclid(7) as u32
}

/// The month `delta` months from this one.
pub fn step_month(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let index = year * 12 + (month as i32 - 1) + delta;
    (index.div_euclid(12), index.rem_euclid(12) as u32 + 1)
}

/// The weekday names in the order a week starting on `start` shows them.
pub fn weekday_names(start: WeekStart) -> [&'static str; 7] {
    std::array::from_fn(|i| WEEKDAYS[(start.index() as usize + i) % 7])
}

/// The days of a month in rows of seven, with the end of the month before
/// and the start of the next filling the first and last rows.
pub fn month_grid(year: i32, month: u32, start: WeekStart) -> [Day; CELLS] {
    let before = (weekday(year, month, 1) + 7 - start.index()) % 7;
    let (prev_year, prev_month) = step_month(year, month, -1);
    let (next_year, next_month) = step_month(year, month, 1);
    let prev_days = days_in_month(prev_year, prev_month);
    let length = days_in_month(year, month);
    std::array::from_fn(|i| {
        let n = i as u32;
        if n < before {
            Day {
                year: prev_year,
                month: prev_month,
                day: prev_days - before + n + 1,
                inside: false,
            }
        } else if n - before < length {
            Day {
                year,
                month,
                day: n - before + 1,
                inside: true,
            }
        } else {
            Day {
                year: next_year,
                month: next_month,
                day: n - before - length + 1,
                inside: false,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbers(grid: &[Day; CELLS]) -> Vec<u32> {
        grid.iter().map(|d| d.day).collect()
    }

    fn inside(grid: &[Day; CELLS]) -> usize {
        grid.iter().filter(|d| d.inside).count()
    }

    #[test]
    fn leap_years() {
        for (year, leap) in [
            (1900, false),
            (2000, true),
            (2023, false),
            (2024, true),
            (2026, false),
            (2100, false),
            (2400, true),
        ] {
            assert_eq!(is_leap(year), leap, "{year}");
        }
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert_eq!(days_in_month(2026, 11), 30);
        assert_eq!(days_in_month(2026, 12), 31);
    }

    #[test]
    fn known_weekdays() {
        // Monday is 0.
        assert_eq!(weekday(1970, 1, 1), 3, "a Thursday");
        assert_eq!(weekday(2000, 2, 29), 1, "a Tuesday");
        assert_eq!(weekday(2024, 2, 1), 3, "a Thursday");
        assert_eq!(weekday(2100, 2, 1), 0, "a Monday");
        assert_eq!(weekday(2026, 11, 1), 6, "a Sunday");
        assert_eq!(weekday(2026, 11, 26), 3, "a Thursday");
        assert_eq!(weekday(1969, 12, 31), 2, "a Wednesday");
        assert_eq!(weekday(1900, 3, 1), 3, "a Thursday");
    }

    #[test]
    fn february_2024_has_a_leap_day() {
        let grid = month_grid(2024, 2, WeekStart::Monday);
        // Thursday the first: Monday to Wednesday come from January.
        assert_eq!(numbers(&grid)[..6], [29, 30, 31, 1, 2, 3]);
        assert_eq!(inside(&grid), 29);
        assert_eq!(grid[3 + 28].day, 29);
        assert!(grid[3 + 28].inside);
        assert_eq!((grid[0].year, grid[0].month), (2024, 1));
        let last = grid[CELLS - 1];
        assert_eq!((last.year, last.month, last.day), (2024, 3, 10));
        assert!(!last.inside);
    }

    #[test]
    fn february_2100_has_no_leap_day() {
        let grid = month_grid(2100, 2, WeekStart::Monday);
        // A Monday the first and 28 days: exactly four rows, then March.
        assert_eq!(numbers(&grid)[..2], [1, 2]);
        assert_eq!(inside(&grid), 28);
        assert_eq!(numbers(&grid)[27], 28);
        assert_eq!(numbers(&grid)[28], 1);
        assert_eq!((grid[28].year, grid[28].month), (2100, 3));
        assert!(!grid[28].inside);
    }

    #[test]
    fn november_2026_starts_on_a_sunday() {
        let monday = month_grid(2026, 11, WeekStart::Monday);
        assert_eq!(numbers(&monday)[..8], [26, 27, 28, 29, 30, 31, 1, 2]);
        assert_eq!((monday[0].year, monday[0].month), (2026, 10));
        assert_eq!(inside(&monday), 30);
        assert_eq!(monday[6 + 29].day, 30);
        let sunday = month_grid(2026, 11, WeekStart::Sunday);
        assert_eq!(numbers(&sunday)[..3], [1, 2, 3]);
        assert!(sunday[0].inside);
        let saturday = month_grid(2026, 11, WeekStart::Saturday);
        assert_eq!(numbers(&saturday)[..3], [31, 1, 2]);
    }

    #[test]
    fn the_year_turns() {
        let december = month_grid(2026, 12, WeekStart::Monday);
        assert_eq!(december[CELLS - 1].year, 2027);
        assert_eq!(december[CELLS - 1].month, 1);
        let january = month_grid(2027, 1, WeekStart::Monday);
        assert_eq!(january[0].year, 2026);
        assert_eq!(january[0].month, 12);
        assert_eq!(step_month(2026, 12, 1), (2027, 1));
        assert_eq!(step_month(2026, 1, -1), (2025, 12));
        assert_eq!(step_month(2026, 11, -23), (2024, 12));
        assert_eq!(step_month(2026, 11, 0), (2026, 11));
        assert_eq!(step_month(0, 1, -1), (-1, 12));
    }

    #[test]
    fn every_month_of_many_years_fills_its_grid() {
        for year in [1899, 1900, 2000, 2024, 2100] {
            for month in 1..=12 {
                for start in [WeekStart::Monday, WeekStart::Sunday, WeekStart::Saturday] {
                    let grid = month_grid(year, month, start);
                    assert_eq!(inside(&grid), days_in_month(year, month) as usize);
                    // The days are in a row, with no gap and no repeat.
                    for pair in grid.windows(2) {
                        let next = pair[0].day == days_in_month(pair[0].year, pair[0].month);
                        assert_eq!(pair[1].day, if next { 1 } else { pair[0].day + 1 });
                    }
                }
            }
        }
    }

    #[test]
    fn the_week_start() {
        assert_eq!(WeekStart::parse("sunday"), WeekStart::Sunday);
        assert_eq!(WeekStart::parse("saturday"), WeekStart::Saturday);
        assert_eq!(WeekStart::parse("monday"), WeekStart::Monday);
        assert_eq!(WeekStart::parse("friday"), WeekStart::Monday);
        assert_eq!(weekday_names(WeekStart::Monday)[0], "Mo");
        assert_eq!(
            weekday_names(WeekStart::Sunday),
            ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"]
        );
        assert_eq!(weekday_names(WeekStart::Saturday)[..2], ["Sa", "Su"]);
    }
}
