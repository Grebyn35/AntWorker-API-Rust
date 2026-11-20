//! The civil calendar, for the tasks that write a time: a new module's migration, whose name
//! begins with one, and a restore from a bucket as it stood at one.

/// The moment `seconds` after the Unix epoch, in UTC, as `YYYYMMDDhhmmss`, by the proleptic
/// Gregorian calendar.
pub fn civil(seconds: u64) -> String {
    let days = seconds / 86_400;
    let of_day = seconds % 86_400;
    // Howard Hinnant's algorithm from days to the civil calendar, whose years start in March, so
    // a leap day ends one.
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60
    )
}
