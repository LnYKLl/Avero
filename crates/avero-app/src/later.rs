//! Times for "Plus tard" and "Envoyer plus tard": the choices offered
//! (« Ce soir », « Demain matin »…), the date typed by hand, how a time
//! is shown, and the clock (which tests set by hand).
//!
//! Times are computed in the local time zone: « demain 8 h » is 8:00 on
//! the computer's clock, whatever the offset to UTC. The functions take
//! any `TimeZone` so that tests can pin one.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// Hour of « ce soir ».
const EVENING: u32 = 18;
/// Hour of « demain matin » and « lundi matin ».
const MORNING: u32 = 8;

/// A ready-made choice of the "Plus tard" and "Envoyer plus tard" menus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// Today at 18:00, or tomorrow at 18:00 once that is past.
    Tonight,
    /// Tomorrow at 8:00.
    Tomorrow,
    /// Next Monday at 8:00 (a week later on a Monday).
    Monday,
    /// The same time in seven days.
    NextWeek,
}

impl When {
    /// The choices of "Plus tard".
    pub const SNOOZE: [When; 4] = [When::Tonight, When::Tomorrow, When::Monday, When::NextWeek];
    /// The choices of "Envoyer plus tard".
    pub const SEND: [When; 3] = [When::Tonight, When::Tomorrow, When::Monday];
}

/// The time a choice stands for, seen from `now`.
pub fn resolve<Tz: TimeZone>(when: When, now: &DateTime<Tz>) -> DateTime<Tz> {
    let today = now.date_naive();
    match when {
        When::Tonight => {
            let tonight = at(&now.timezone(), today, EVENING);
            if tonight > *now {
                tonight
            } else {
                at(&now.timezone(), today + Duration::days(1), EVENING)
            }
        }
        When::Tomorrow => at(&now.timezone(), today + Duration::days(1), MORNING),
        When::Monday => {
            let ahead = 7 - i64::from(today.weekday().num_days_from_monday());
            at(&now.timezone(), today + Duration::days(ahead), MORNING)
        }
        When::NextWeek => now.clone() + Duration::days(7),
    }
}

/// `day` at `hour`:00 on the clock of `tz`. When that hour does not
/// exist (the clocks go forward), the first time after it; when it
/// happens twice (they go back), the first one.
fn at<Tz: TimeZone>(tz: &Tz, day: NaiveDate, hour: u32) -> DateTime<Tz> {
    let mut time = day.and_time(NaiveTime::from_hms_opt(hour, 0, 0).unwrap_or_default());
    for _ in 0..4 {
        if let Some(found) = tz.from_local_datetime(&time).earliest() {
            return found;
        }
        time += Duration::minutes(30);
    }
    tz.from_utc_datetime(&time)
}

/// The words of a choice in the menu: « Ce soir », « Demain soir » once
/// 18:00 is past, « Demain matin », « Lundi matin », « Dans une semaine ».
pub fn label<Tz: TimeZone>(when: When, now: &DateTime<Tz>) -> &'static str {
    match when {
        When::Tonight if resolve(when, now).date_naive() == now.date_naive() => "Ce soir",
        When::Tonight => "Demain soir",
        When::Tomorrow => "Demain matin",
        When::Monday => "Lundi matin",
        When::NextWeek => "Dans une semaine",
    }
}

/// French weekday names, Monday first.
const WEEKDAYS: [&str; 7] = [
    "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
];

/// French month names, January first.
const MONTHS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];

/// A time for people, from `now`: « aujourd'hui à 18:00 », « demain à
/// 08:00 », « lundi 3 novembre à 08:00 » (with the year when it is not
/// this one).
pub fn describe<Tz: TimeZone>(time: &DateTime<Tz>, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let hour = time.format("%H:%M");
    let days = (time.date_naive() - now.date_naive()).num_days();
    match days {
        0 => format!("aujourd'hui à {hour}"),
        1 => format!("demain à {hour}"),
        -1 => format!("hier à {hour}"),
        _ => {
            let weekday = WEEKDAYS[time.weekday().num_days_from_monday() as usize];
            let month = MONTHS[time.month0() as usize];
            if time.year() == now.year() {
                format!("{weekday} {} {month} à {hour}", time.day())
            } else {
                format!("{weekday} {} {month} {} à {hour}", time.day(), time.year())
            }
        }
    }
}

/// `describe` for a Unix time, in the local time zone.
pub fn describe_local(time: i64, now: i64) -> String {
    match (
        Local.timestamp_opt(time, 0).single(),
        Local.timestamp_opt(now, 0).single(),
    ) {
        (Some(time), Some(now)) => describe(&time, &now),
        _ => String::new(),
    }
}

/// A short time for the list: « 18:00 » today, « demain 08:00 », else
/// « 3/11 08:00 ».
pub fn short<Tz: TimeZone>(time: &DateTime<Tz>, now: &DateTime<Tz>) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let hour = time.format("%H:%M");
    match (time.date_naive() - now.date_naive()).num_days() {
        0 => hour.to_string(),
        1 => format!("demain {hour}"),
        _ => format!("{}/{} {hour}", time.day(), time.month()),
    }
}

/// `short` for a Unix time, in the local time zone.
pub fn short_local(time: i64, now: i64) -> String {
    match (
        Local.timestamp_opt(time, 0).single(),
        Local.timestamp_opt(now, 0).single(),
    ) {
        (Some(time), Some(now)) => short(&time, &now),
        _ => String::new(),
    }
}

/// The time typed in the dialog: `JJ/MM/AAAA HH:MM`, or shorter (the
/// year left out means the next such day, the time left out means 8:00;
/// `18h`, `18h30` and `18:30` all work). It must be in the future.
pub fn parse_custom<Tz: TimeZone>(text: &str, now: &DateTime<Tz>) -> Result<DateTime<Tz>, String> {
    let wrong = || "Date incomprise : écrivez par exemple 31/12/2026 18:30".to_string();
    let mut parts = text.split_whitespace();
    let date = parts.next().ok_or_else(wrong)?;
    let time = parts.next();
    if parts.next().is_some() {
        return Err(wrong());
    }
    let numbers: Vec<&str> = date.split(['/', '.', '-']).collect();
    let number = |s: &str| s.parse::<u32>().map_err(|_| wrong());
    let (day, month) = match numbers.as_slice() {
        [day, month] | [day, month, _] => (number(day)?, number(month)?),
        _ => return Err(wrong()),
    };
    let (hour, minute) = match time {
        None => (MORNING, 0),
        Some(time) => {
            let time = time.to_lowercase();
            let (h, m) = match time.split_once(['h', ':']) {
                Some((h, "")) => (h, "0"),
                Some((h, m)) => (h, m),
                None => (time.as_str(), "0"),
            };
            (number(h)?, number(m)?)
        }
    };
    let clock = NaiveTime::from_hms_opt(hour, minute, 0).ok_or_else(wrong)?;
    let build = |year: i32| {
        NaiveDate::from_ymd_opt(year, month, day).and_then(|d| {
            now.timezone()
                .from_local_datetime(&d.and_time(clock))
                .earliest()
        })
    };
    let found = match numbers.as_slice() {
        [_, _, year] => {
            let mut year: i32 = year.parse().map_err(|_| wrong())?;
            if year < 100 {
                year += 2000;
            }
            build(year).ok_or_else(wrong)?
        }
        _ => match build(now.year()) {
            Some(time) if time > *now => time,
            _ => build(now.year() + 1).ok_or_else(wrong)?,
        },
    };
    if found <= *now {
        return Err("Cette date est déjà passée".to_string());
    }
    Ok(found)
}

/// Where the time comes from: the computer's clock, or a time set by
/// hand (tests). Unix time in seconds.
#[derive(Debug, Clone, Default)]
pub struct Clock(Option<Arc<AtomicI64>>);

impl Clock {
    /// A clock stopped at `time`, moved with `set`.
    #[cfg(test)]
    pub fn fixed(time: i64) -> Clock {
        Clock(Some(Arc::new(AtomicI64::new(time))))
    }

    pub fn now(&self) -> i64 {
        match &self.0 {
            Some(time) => time.load(Ordering::SeqCst),
            None => Local::now().timestamp(),
        }
    }

    /// Moves a fixed clock (nothing for the real one).
    #[cfg(test)]
    pub fn set(&self, time: i64) {
        if let Some(fixed) = &self.0 {
            fixed.store(time, Ordering::SeqCst);
        }
    }

    /// `now` as a local date and time.
    pub fn local(&self) -> DateTime<Local> {
        Local
            .timestamp_opt(self.now(), 0)
            .single()
            .unwrap_or_else(Local::now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Timelike, Utc};

    /// Paris in winter (UTC+1).
    fn paris() -> FixedOffset {
        FixedOffset::east_opt(3600).unwrap()
    }

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<FixedOffset> {
        paris().with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn tonight_is_today_until_six_then_tomorrow() {
        // Wednesday 28 October 2026.
        let afternoon = local(2026, 10, 28, 14, 5);
        assert_eq!(
            resolve(When::Tonight, &afternoon),
            local(2026, 10, 28, 18, 0)
        );
        assert_eq!(label(When::Tonight, &afternoon), "Ce soir");
        let evening = local(2026, 10, 28, 18, 0);
        assert_eq!(resolve(When::Tonight, &evening), local(2026, 10, 29, 18, 0));
        assert_eq!(label(When::Tonight, &evening), "Demain soir");
        let night = local(2026, 10, 28, 23, 59);
        assert_eq!(resolve(When::Tonight, &night), local(2026, 10, 29, 18, 0));
    }

    #[test]
    fn times_are_on_the_local_clock() {
        // 07:30 UTC is 08:30 in Paris: "tomorrow 8:00" is Paris's.
        let now = Utc
            .with_ymd_and_hms(2026, 10, 28, 7, 30, 0)
            .unwrap()
            .with_timezone(&paris());
        let tomorrow = resolve(When::Tomorrow, &now);
        assert_eq!(tomorrow, local(2026, 10, 29, 8, 0));
        assert_eq!(tomorrow.with_timezone(&Utc).hour(), 7);
        // Across the new year.
        let eve = local(2026, 12, 31, 20, 0);
        assert_eq!(resolve(When::Tomorrow, &eve), local(2027, 1, 1, 8, 0));
    }

    #[test]
    fn monday_is_the_next_one() {
        // Wednesday → Monday 2 November.
        assert_eq!(
            resolve(When::Monday, &local(2026, 10, 28, 10, 0)),
            local(2026, 11, 2, 8, 0)
        );
        // Sunday → the next day.
        assert_eq!(
            resolve(When::Monday, &local(2026, 11, 1, 22, 0)),
            local(2026, 11, 2, 8, 0)
        );
        // Monday, even early → the Monday after.
        let monday = local(2026, 11, 2, 6, 0);
        assert_eq!(monday.weekday(), chrono::Weekday::Mon);
        assert_eq!(resolve(When::Monday, &monday), local(2026, 11, 9, 8, 0));
        assert_eq!(
            resolve(When::NextWeek, &local(2026, 10, 28, 10, 15)),
            local(2026, 11, 4, 10, 15)
        );
    }

    #[test]
    fn typed_dates_are_read_in_the_local_zone() {
        let now = local(2026, 10, 28, 10, 0);
        assert_eq!(
            parse_custom("31/12/2026 18:30", &now),
            Ok(local(2026, 12, 31, 18, 30))
        );
        assert_eq!(
            parse_custom("5/11 18h", &now),
            Ok(local(2026, 11, 5, 18, 0))
        );
        assert_eq!(
            parse_custom("05/11/26 7h15", &now),
            Ok(local(2026, 11, 5, 7, 15))
        );
        // No time: 8:00. No year, day already past: next year.
        assert_eq!(parse_custom("2/1", &now), Ok(local(2027, 1, 2, 8, 0)));
        assert_eq!(parse_custom("28/10", &now), Ok(local(2027, 10, 28, 8, 0)));
        assert!(parse_custom("28/10/2026 9:00", &now).is_err(), "past");
        assert!(parse_custom("31/02/2027", &now).is_err());
        assert!(parse_custom("demain", &now).is_err());
        assert!(parse_custom("", &now).is_err());
        assert!(parse_custom("1/1/2027 25:00", &now).is_err());
    }

    #[test]
    fn times_are_described_in_french() {
        let now = local(2026, 10, 28, 10, 0);
        assert_eq!(
            describe(&local(2026, 10, 28, 18, 0), &now),
            "aujourd'hui à 18:00"
        );
        assert_eq!(describe(&local(2026, 10, 29, 8, 0), &now), "demain à 08:00");
        assert_eq!(
            describe(&local(2026, 11, 2, 8, 0), &now),
            "lundi 2 novembre à 08:00"
        );
        assert_eq!(
            describe(&local(2027, 1, 4, 8, 0), &now),
            "lundi 4 janvier 2027 à 08:00"
        );
    }

    #[test]
    fn fixed_clock_moves_by_hand() {
        let clock = Clock::fixed(100);
        let copy = clock.clone();
        copy.set(250);
        assert_eq!(clock.now(), 250);
        assert!(Clock::default().now() > 1_700_000_000);
    }
}
