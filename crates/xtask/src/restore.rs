//! The plan of a restore of the reference deployment's bundled database to a moment (OPS-17):
//! the backup it starts from, and the timeline it replays along. Each restore begins a timeline,
//! a history of the database that branches off an earlier one, so the database has had several
//! histories, and a moment belongs to the timeline that was live then: the newest one that had
//! begun. pgBackRest picks a backup by time alone, and Postgres replays along one timeline, the
//! latest unless told another, so neither default reaches every moment: the latest timeline
//! misses a moment that an earlier restore went back past, and a backup's own timeline misses a
//! moment on a restored timeline that no backup of it had ended before.
//!
//! The plan is a pure function of what the backup bucket records, which deploy.sh's
//! `restore-inputs` prints, so tests cover it without a bucket. `just restore` makes it on the
//! developer's machine, and the end-to-end tests through `restore-plan`.

use std::{
    io::{self, Read},
    process::ExitCode,
};

use serde_json::Value;

/// A backup that the bucket holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    label: String,
    /// When it ended, in seconds since the Unix epoch.
    stop: i64,
    /// The timeline it was taken on.
    timeline: u32,
    /// The position in the write-ahead log where it ended.
    lsn_stop: u64,
}

/// A timeline that a restore began, as its history file in the bucket records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timeline {
    id: u32,
    /// When it began: when its history file reached the bucket, in seconds since the epoch.
    began: i64,
    /// Each timeline it descends from, with the position in the write-ahead log where its
    /// history left that timeline.
    branches: Vec<(u32, u64)>,
}

/// The backup a restore to a moment starts from, and the timeline it replays along.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub set: String,
    pub timeline: u32,
}

/// The plan of a restore to `moment`, in seconds since the epoch: along the timeline that was
/// live at that moment, from the latest backup on that timeline's history that ended before it.
/// A backup on a timeline it descends from is on its history only if it ended before the branch.
pub fn plan(moment: i64, backups: &[Backup], timelines: &[Timeline]) -> Result<Plan, String> {
    let live = timelines
        .iter()
        .filter(|timeline| timeline.began <= moment)
        .max_by_key(|timeline| timeline.began);
    // Before the first restore, the database's own timeline, which no history file records.
    let (timeline, branches) = match live {
        Some(live) => (live.id, live.branches.as_slice()),
        None => (
            backups
                .iter()
                .map(|backup| backup.timeline)
                .min()
                .unwrap_or(1),
            [].as_slice(),
        ),
    };
    let on_history = |backup: &Backup| {
        backup.timeline == timeline
            || branches
                .iter()
                .any(|&(id, branch)| id == backup.timeline && backup.lsn_stop <= branch)
    };
    backups
        .iter()
        .filter(|backup| backup.stop < moment && on_history(backup))
        .max_by_key(|backup| backup.stop)
        .map(|backup| Plan {
            set: backup.label.clone(),
            timeline,
        })
        .ok_or_else(|| {
            "no backup on the history the database had then ended before it, so the backups \
             don't reach it"
                .to_owned()
        })
}

/// The plan of a restore to the moment `text` names, from `inputs`, which deploy.sh's
/// `restore-inputs` printed; or why there is none.
pub fn planned(text: &str, inputs: &str) -> Result<Plan, String> {
    let at = moment(text).ok_or_else(|| {
        format!(
            "{text} isn't a moment. A moment is a date and a time with its offset from UTC, as \
             Postgres writes one, such as 2026-10-25 10:41:31+00."
        )
    })?;
    let (backups, timelines) = read_inputs(inputs)?;
    plan(at, &backups, &timelines).map_err(|reason| format!("Can't restore to {text}: {reason}."))
}

/// What `restore-inputs` printed: a line `backups` with pgBackRest's info in JSON, a line
/// `histories` with the history files the bucket lists in JSON, and a line `branch` for each
/// line of each history file: its name, the timeline it descends from, and the branch's position.
fn read_inputs(inputs: &str) -> Result<(Vec<Backup>, Vec<Timeline>), String> {
    let mut backups = Vec::new();
    let mut timelines: Vec<(String, Timeline)> = Vec::new();
    let mut branches: Vec<(String, u32, u64)> = Vec::new();
    for line in inputs.lines() {
        let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
        match kind {
            "backups" => backups = read_backups(rest)?,
            "histories" => timelines = read_histories(rest)?,
            "branch" => {
                let fields: Vec<&str> = rest.split_whitespace().collect();
                let [name, id, at] = fields.as_slice() else {
                    return Err(format!("the bucket's history reads oddly: {line}"));
                };
                let id = id
                    .parse()
                    .map_err(|_| format!("the bucket's history reads oddly: {line}"))?;
                let at =
                    lsn(at).ok_or_else(|| format!("the bucket's history reads oddly: {line}"))?;
                branches.push(((*name).to_owned(), id, at));
            }
            _ => {}
        }
    }
    for (name, id, at) in branches {
        if let Some((_, timeline)) = timelines.iter_mut().find(|(file, _)| *file == name) {
            timeline.branches.push((id, at));
        }
    }
    Ok((
        backups,
        timelines
            .into_iter()
            .map(|(_, timeline)| timeline)
            .collect(),
    ))
}

/// The backups of pgBackRest's `info --output=json`, which lists one stanza.
fn read_backups(json: &str) -> Result<Vec<Backup>, String> {
    let info: Value = serde_json::from_str(json)
        .map_err(|error| format!("pgBackRest's info reads oddly: {error}"))?;
    let odd = || "pgBackRest's info lists a backup that reads oddly".to_owned();
    info[0]["backup"]
        .as_array()
        .ok_or_else(|| "pgBackRest's info lists no backups".to_owned())?
        .iter()
        .map(|backup| {
            Ok(Backup {
                label: backup["label"].as_str().ok_or_else(odd)?.to_owned(),
                stop: backup["timestamp"]["stop"].as_i64().ok_or_else(odd)?,
                // A segment's name begins with its timeline, in 8 hexadecimal digits.
                timeline: backup["archive"]["start"]
                    .as_str()
                    .and_then(|segment| segment.get(..8))
                    .and_then(|id| u32::from_str_radix(id, 16).ok())
                    .ok_or_else(odd)?,
                lsn_stop: backup["lsn"]["stop"]
                    .as_str()
                    .and_then(lsn)
                    .ok_or_else(odd)?,
            })
        })
        .collect()
}

/// The timelines of the history files that `repo-ls --output=json` lists, by each file's name,
/// such as `18-1/00000002.history`, whose stem is the timeline in hexadecimal, and its `time`.
fn read_histories(json: &str) -> Result<Vec<(String, Timeline)>, String> {
    let listed: Value = serde_json::from_str(json)
        .map_err(|error| format!("the bucket's list of histories reads oddly: {error}"))?;
    let files = listed
        .as_object()
        .ok_or_else(|| "the bucket's list of histories reads oddly".to_owned())?;
    files
        .iter()
        .map(|(name, file)| {
            let odd = || format!("the bucket's history file {name} reads oddly");
            let stem = name
                .rsplit('/')
                .next()
                .and_then(|file| file.strip_suffix(".history"))
                .ok_or_else(odd)?;
            Ok((
                name.clone(),
                Timeline {
                    id: u32::from_str_radix(stem, 16).map_err(|_| odd())?,
                    began: file["time"].as_i64().ok_or_else(odd)?,
                    branches: Vec::new(),
                },
            ))
        })
        .collect()
}

/// A position in the write-ahead log as Postgres writes it, such as `0/D025D38`.
fn lsn(text: &str) -> Option<u64> {
    let (high, low) = text.split_once('/')?;
    Some((u64::from_str_radix(high, 16).ok()? << 32) | u64::from_str_radix(low, 16).ok()?)
}

/// The moment `text` names, in whole seconds since the Unix epoch, rounded down: a date and a
/// time, as Postgres writes a time, `YYYY-MM-DD HH:MM:SS`, with `T` or a space between them, the
/// seconds and a fraction of a second optional, then its offset from UTC: `Z`, or a sign and
/// hours, with minutes after them or after a colon.
pub fn moment(text: &str) -> Option<i64> {
    let text = text.trim();
    let date = text.get(..10)?;
    let rest = text.get(10..)?.strip_prefix([' ', 'T'])?;
    let split = rest.find(['+', '-', 'Z'])?;
    let (time, offset) = rest.split_at(split);
    let mut parts = date.split('-');
    let (year, month, day) = (
        number(parts.next()?, 4)?,
        number(parts.next()?, 2)?,
        number(parts.next()?, 2)?,
    );
    let time = time.split('.').next()?;
    let mut parts = time.split(':');
    let (hour, minute) = (number(parts.next()?, 2)?, number(parts.next()?, 2)?);
    let second = parts.next().map_or(Some(0), |second| number(second, 2))?;
    if parts.next().is_some()
        || date.split('-').count() != 3
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let offset = match offset {
        "Z" => 0,
        _ => {
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let digits = offset.get(1..)?.replace(':', "");
            let (hours, minutes) = match digits.len() {
                2 => (number(&digits, 2)?, 0),
                4 => (number(digits.get(..2)?, 2)?, number(digits.get(2..)?, 2)?),
                _ => return None,
            };
            sign * (hours * 3600 + minutes * 60)
        }
    };
    Some(days(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// `text` as a number, if it is exactly `width` decimal digits.
fn number(text: &str, width: usize) -> Option<i64> {
    if text.len() != width || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The days from the first day of the Unix epoch to the date, in the proleptic Gregorian calendar.
fn days(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let of_era_days = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_era_days - 719_468
}

/// `restore-plan`, which the end-to-end tests run as `just restore` makes its plan: reads what
/// `restore-inputs` printed from standard input, and prints the plan of a restore to `moment` as
/// the backup and the timeline, separated by a space.
pub fn run(moment: &str) -> ExitCode {
    let mut inputs = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut inputs) {
        return crate::deploy::refuse(&format!("Can't read what the bucket records: {error}"));
    }
    match planned(moment, &inputs) {
        Ok(plan) => report(&format!("{} {}", plan.set, plan.timeline)),
        Err(refused) => crate::deploy::refuse(&refused),
    }
}

#[expect(clippy::print_stdout, reason = "the plan is the task's output")]
fn report(message: &str) -> ExitCode {
    println!("{message}");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backup(label: &str, stop: i64, timeline: u32, lsn_stop: u64) -> Backup {
        Backup {
            label: label.to_owned(),
            stop,
            timeline,
            lsn_stop,
        }
    }

    /// A database restored once, at 1000, to a moment before 400, whose history left timeline 1
    /// at position 40: a backup before the branch, one after it on the abandoned history, and
    /// the restore's closing backup on timeline 2, which ended at 1100.
    fn restored_once() -> (Vec<Backup>, Vec<Timeline>) {
        (
            vec![
                backup("before-branch", 300, 1, 30),
                backup("abandoned", 700, 1, 70),
                backup("closing", 1100, 2, 110),
            ],
            vec![Timeline {
                id: 2,
                began: 1000,
                branches: vec![(1, 40)],
            }],
        )
    }

    #[test]
    fn ops_17_a_moment_before_any_restore_takes_the_latest_backup_before_it() {
        let backups = [backup("first", 100, 1, 10), backup("second", 500, 1, 50)];

        assert_eq!(
            plan(600, &backups, &[]),
            Ok(Plan {
                set: "second".to_owned(),
                timeline: 1
            })
        );
        assert_eq!(
            plan(400, &backups, &[]).map(|plan| plan.set),
            Ok("first".to_owned())
        );
        assert!(plan(50, &backups, &[]).is_err());
    }

    #[test]
    fn ops_17_a_moment_that_a_restore_went_back_past_replays_its_own_timeline() {
        let (backups, timelines) = restored_once();

        // Between the restore's target and the time it ran, on the history it abandoned.
        let abandoned = plan(800, &backups, &timelines);

        assert_eq!(
            abandoned,
            Ok(Plan {
                set: "abandoned".to_owned(),
                timeline: 1
            })
        );
    }

    #[test]
    fn ops_17_a_moment_before_the_closing_backup_ended_replays_the_restored_timeline() {
        let (backups, timelines) = restored_once();

        // The restored timeline is live, and its closing backup hasn't ended: the backup from
        // before the branch, not the later one on the history the restore abandoned.
        let during = plan(1050, &backups, &timelines);

        assert_eq!(
            during,
            Ok(Plan {
                set: "before-branch".to_owned(),
                timeline: 2
            })
        );
    }

    #[test]
    fn ops_17_a_moment_after_the_closing_backup_takes_it() {
        let (backups, timelines) = restored_once();

        assert_eq!(
            plan(1200, &backups, &timelines),
            Ok(Plan {
                set: "closing".to_owned(),
                timeline: 2
            })
        );
    }

    #[test]
    fn ops_17_the_bucket_s_records_are_read_as_pgbackrest_writes_them() {
        // The trial deployment's records: one backup, and two of its five history files.
        let inputs = concat!(
            r#"backups [{"backup":[{"label":"20261026-214504F","type":"full","timestamp":{"start":1793051104,"stop":1793051111},"lsn":{"start":"0/E000028","stop":"0/E000120"},"archive":{"start":"00000002000000000000000E","stop":"00000002000000000000000E"}}]}]"#,
            "\n",
            r#"histories {"18-1/00000002.history":{"type":"file","size":80,"time":1793051101},"18-1/00000003.history":{"type":"file","size":112,"time":1793140862}}"#,
            "\n",
            "branch 18-1/00000002.history 1 0/D025D38\n",
            "branch 18-1/00000003.history 1 0/D025D38\n",
            "branch 18-1/00000003.history 2 0/19000000\n",
        );

        let (backups, timelines) = read_inputs(inputs).unwrap();

        assert_eq!(
            backups,
            [backup("20261026-214504F", 1_793_051_111, 2, 0xE00_0120)]
        );
        assert!(timelines.contains(&Timeline {
            id: 3,
            began: 1_793_140_862,
            branches: vec![(1, 0xD02_5D38), (2, 0x1900_0000)],
        }));
    }

    #[test]
    fn ops_17_a_moment_is_read_with_its_offset_from_utc() {
        // The seconds since the epoch that each text below names, in UTC.
        let utc = 1_792_924_891;

        assert_eq!(moment("2026-10-25 10:41:31+00"), Some(utc));
        assert_eq!(moment("2026-10-25 10:41:31.913+00"), Some(utc));
        assert_eq!(moment("2026-10-25T10:41:31Z"), Some(utc));
        assert_eq!(moment("2026-10-25 12:41:31+02:00"), Some(utc));
        assert_eq!(moment("2026-10-25 05:11:31-0530"), Some(utc));
        assert_eq!(moment("2026-10-25 10:41+00"), Some(utc - 31));
        for odd in [
            "2026-10-25 10:41:31",
            "2026-13-25 10:41:31+00",
            "25/10/2026 10:41:31+00",
            "yesterday",
        ] {
            assert_eq!(moment(odd), None, "{odd}");
        }
    }
}
