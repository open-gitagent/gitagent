//! `schedules/*.yaml`: prompts the agent runs on a timetable, as in
//! gitagent 1.x.
//!
//! ```yaml
//! id: daily-triage            # or `name`; the file's name when absent
//! description: Morning ticket triage
//! cron: "0 8 * * 1-5"         # minute hour day-of-month month day-of-week
//! prompt: |
//!   Review the open tickets and save a summary to memory.
//! mode: repeat                # or once (fires one time, then enabled: false)
//! runAt: 2026-11-01T09:00:00Z # a one-time run at a moment instead of a cron
//! enabled: true
//! ```
//!
//! Cron expressions are the five classic fields, or six with seconds in
//! front, with `*`, lists, ranges, steps and month or weekday names. They
//! are read in local time, as 1.x did. The runner lives in the CLI
//! (`gitagent schedule`); this module reads, writes and times the files.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub id: String,
    pub description: String,
    pub prompt: String,
    pub cron: Option<Cron>,
    /// The cron expression as written.
    pub cron_text: String,
    /// A one-time run at this moment (unix seconds), from `runAt`.
    pub run_at: Option<i64>,
    pub run_at_text: String,
    /// `mode: once`: fires one time, then is disabled.
    pub once: bool,
    pub enabled: bool,
    pub last_run_at: Option<String>,
    pub last_result: Option<String>,
    pub path: PathBuf,
}

impl Schedule {
    /// When the schedule next fires after `now` (unix seconds), in a zone
    /// `offset` seconds east of UTC. `None` when it never will.
    pub fn next_after(&self, now: i64, offset: i64) -> Option<i64> {
        if !self.enabled {
            return None;
        }
        match (&self.run_at, &self.cron) {
            (Some(at), _) => (*at > now).then_some(*at),
            (None, Some(c)) => c.next_after(now, offset),
            (None, None) => None,
        }
    }

    /// One line on when it runs.
    pub fn when(&self) -> String {
        match (&self.run_at_text, &self.cron_text) {
            (at, _) if !at.is_empty() => format!("once at {at}"),
            (_, c) if self.once => format!("once, cron {c}"),
            (_, c) => format!("cron {c}"),
        }
    }
}

/// Reads every schedule under `<root>/schedules/`; problems are reported
/// as `<file>: <message>` and the file is skipped.
pub fn discover(root: &Path) -> (Vec<Schedule>, Vec<String>) {
    let dir = root.join("schedules");
    let mut out = Vec::new();
    let mut problems = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (out, problems);
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("yaml") | Some("yml")
                )
        })
        .collect();
    paths.sort();
    for path in paths {
        let rel = format!(
            "schedules/{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        match read(&path) {
            Ok(s) => {
                if out.iter().any(|o: &Schedule| o.id == s.id) {
                    problems.push(format!(
                        "{rel}: another schedule is already called {:?}",
                        s.id
                    ));
                } else {
                    out.push(s);
                }
            }
            Err(e) => problems.push(format!("{rel}: {e}")),
        }
    }
    (out, problems)
}

/// Reads one schedule file.
pub fn read(path: &Path) -> Result<Schedule, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let v: Value = serde_saphyr::from_str(&text).map_err(|e| format!("not valid YAML: {e}"))?;
    let m = v
        .as_object()
        .ok_or_else(|| "must be a mapping".to_string())?;
    from_value(m, path)
}

fn from_value(m: &Map<String, Value>, path: &Path) -> Result<Schedule, String> {
    let s = |k: &str| {
        m.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
    };
    let stem = path
        .file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let id = s("id").or_else(|| s("name")).unwrap_or(&stem).to_string();
    if !is_kebab(&id) {
        return Err(format!(
            "the id {id:?} must be lowercase words joined by dashes"
        ));
    }
    let prompt = s("prompt").ok_or("`prompt` is required")?.to_string();
    let cron_text = s("cron").unwrap_or_default().to_string();
    let run_at_text = s("runAt")
        .or_else(|| s("run_at"))
        .unwrap_or_default()
        .to_string();
    if cron_text.is_empty() && run_at_text.is_empty() {
        return Err("give `cron` or `runAt`".into());
    }
    let cron = if cron_text.is_empty() {
        None
    } else {
        Some(Cron::parse(&cron_text).map_err(|e| format!("cron {cron_text:?}: {e}"))?)
    };
    let run_at = if run_at_text.is_empty() {
        None
    } else {
        Some(parse_rfc3339(&run_at_text).ok_or_else(|| {
            format!("runAt {run_at_text:?} is not a time like 2026-11-01T09:00:00Z")
        })?)
    };
    let mode = s("mode").unwrap_or("repeat");
    if !matches!(mode, "repeat" | "once") {
        return Err(format!("mode must be repeat or once (got {mode:?})"));
    }
    Ok(Schedule {
        id,
        description: s("description").unwrap_or_default().to_string(),
        prompt,
        cron,
        cron_text,
        run_at,
        run_at_text,
        once: mode == "once" || run_at.is_some(),
        enabled: m.get("enabled") != Some(&Value::Bool(false)),
        last_run_at: s("lastRunAt").map(str::to_string),
        last_result: s("lastResult").map(str::to_string),
        path: path.to_path_buf(),
    })
}

pub fn is_kebab(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|w| {
            !w.is_empty()
                && w.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// What `gitagent schedule add` writes.
pub struct NewSchedule {
    pub id: String,
    pub description: String,
    pub prompt: String,
    pub cron: Option<String>,
    pub run_at: Option<String>,
    pub once: bool,
}

/// Writes `<root>/schedules/<id>.yaml` for a new schedule.
pub fn save(root: &Path, new: &NewSchedule) -> Result<PathBuf, String> {
    if !is_kebab(&new.id) {
        return Err(format!(
            "the id {:?} must be lowercase words joined by dashes",
            new.id
        ));
    }
    if new.prompt.trim().is_empty() {
        return Err("a prompt is required".into());
    }
    match (&new.cron, &new.run_at) {
        (None, None) => return Err("give a cron expression or a time".into()),
        (Some(c), _) => {
            Cron::parse(c).map_err(|e| format!("cron {c:?}: {e}"))?;
        }
        (None, Some(at)) => {
            parse_rfc3339(at)
                .ok_or_else(|| format!("{at:?} is not a time like 2026-11-01T09:00:00Z"))?;
        }
    }
    let mut m = Map::new();
    m.insert("id".into(), Value::String(new.id.clone()));
    if !new.description.trim().is_empty() {
        m.insert(
            "description".into(),
            Value::String(new.description.trim().into()),
        );
    }
    m.insert("prompt".into(), Value::String(new.prompt.trim().into()));
    m.insert(
        "cron".into(),
        Value::String(new.cron.clone().unwrap_or_default()),
    );
    m.insert(
        "mode".into(),
        Value::String(
            if new.once || new.run_at.is_some() {
                "once"
            } else {
                "repeat"
            }
            .into(),
        ),
    );
    if let Some(at) = &new.run_at {
        m.insert("runAt".into(), Value::String(at.clone()));
    }
    m.insert("enabled".into(), Value::Bool(true));
    m.insert(
        "createdAt".into(),
        Value::String(gitagent_record_time::utc_now_rfc3339()),
    );
    let dir = root.join("schedules");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{}.yaml", new.id));
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    std::fs::write(&path, to_yaml(&Value::Object(m)))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Changes some top-level fields of a schedule file, keeping the others.
pub fn update(path: &Path, changes: &[(&str, Value)]) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut v: Value = serde_saphyr::from_str(&text).map_err(|e| format!("not valid YAML: {e}"))?;
    let m = v.as_object_mut().ok_or("must be a mapping")?;
    for (k, value) in changes {
        if value.is_null() {
            m.remove(*k);
        } else {
            m.insert((*k).to_string(), value.clone());
        }
    }
    std::fs::write(path, to_yaml(&v)).map_err(|e| format!("{}: {e}", path.display()))
}

/// A plain YAML rendering of a JSON value (block style, strings quoted
/// where YAML could read them otherwise).
pub fn to_yaml(v: &Value) -> String {
    let mut out = String::new();
    emit(v, 0, &mut out);
    out
}

fn emit(v: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                match val {
                    Value::Object(inner) if !inner.is_empty() => {
                        out.push_str(&format!("{pad}{}:\n", yaml_key(k)));
                        emit(val, indent + 2, out);
                    }
                    Value::Array(items) if !items.is_empty() => {
                        out.push_str(&format!("{pad}{}:\n", yaml_key(k)));
                        for item in items {
                            match item {
                                Value::Object(_) | Value::Array(_) => {
                                    out.push_str(&format!("{pad}  -\n"));
                                    emit(item, indent + 4, out);
                                }
                                _ => out.push_str(&format!("{pad}  - {}\n", scalar(item))),
                            }
                        }
                    }
                    Value::String(s) if s.contains('\n') => {
                        out.push_str(&format!("{pad}{}: |\n", yaml_key(k)));
                        for line in s.trim_end_matches('\n').split('\n') {
                            out.push_str(&format!("{pad}  {line}\n"));
                        }
                    }
                    _ => out.push_str(&format!("{pad}{}: {}\n", yaml_key(k), scalar(val))),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                out.push_str(&format!("{pad}- {}\n", scalar(item)));
            }
        }
        other => out.push_str(&format!("{pad}{}\n", scalar(other))),
    }
}

fn yaml_key(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        k.to_string()
    } else {
        serde_json::to_string(k).unwrap_or_default()
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => {
            let plain = !s.is_empty()
                && !s.starts_with([
                    ' ', '-', '?', ':', ',', '[', ']', '{', '}', '#', '&', '*', '!', '|', '>',
                    '\'', '"', '%', '@', '`',
                ])
                && !s.ends_with(' ')
                && !s.contains(": ")
                && !s.contains(" #")
                && !s.contains('\n')
                && !matches!(
                    s.to_ascii_lowercase().as_str(),
                    "true" | "false" | "yes" | "no" | "on" | "off" | "null" | "~"
                )
                && s.parse::<f64>().is_err();
            if plain {
                s.clone()
            } else {
                serde_json::to_string(s).unwrap_or_default()
            }
        }
        Value::Object(m) if m.is_empty() => "{}".into(),
        Value::Array(a) if a.is_empty() => "[]".into(),
        other => other.to_string(),
    }
}

// ------------------------------------------------------------------ cron

/// A parsed cron expression: seconds, minutes, hours, days of the month,
/// months and days of the week that match.
#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    seconds: Vec<u32>,
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days: Vec<u32>,
    /// `*` for days of the month: then days of the week alone decide (and
    /// the other way round), as cron does.
    any_day: bool,
    months: Vec<u32>,
    weekdays: Vec<u32>,
    any_weekday: bool,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

impl Cron {
    pub fn parse(text: &str) -> Result<Cron, String> {
        let fields: Vec<&str> = text.split_whitespace().collect();
        let (seconds, rest) = match fields.len() {
            5 => (vec![0], &fields[..]),
            6 => (field(fields[0], 0, 59, &[])?, &fields[1..]),
            n => return Err(format!("expected 5 or 6 fields, got {n}")),
        };
        let (days, any_day) = (field(rest[2], 1, 31, &[])?, rest[2].starts_with('*'));
        let mut weekdays = field(rest[4], 0, 7, &WEEKDAYS)?;
        // 7 is Sunday too.
        for d in &mut weekdays {
            if *d == 7 {
                *d = 0;
            }
        }
        weekdays.sort_unstable();
        weekdays.dedup();
        Ok(Cron {
            seconds,
            minutes: field(rest[0], 0, 59, &[])?,
            hours: field(rest[1], 0, 23, &[])?,
            days,
            any_day,
            months: field(rest[3], 1, 12, &MONTHS)?,
            weekdays,
            any_weekday: rest[4].starts_with('*'),
        })
    }

    /// The first matching second strictly after `now`, in a zone `offset`
    /// seconds east of UTC; `None` when nothing matches within five years.
    pub fn next_after(&self, now: i64, offset: i64) -> Option<i64> {
        let mut t = now + 1;
        let limit = now + 5 * 366 * 86_400;
        while t < limit {
            let local = t + offset;
            let days = local.div_euclid(86_400);
            let sod = local.rem_euclid(86_400) as u32;
            let (_, month, day) = civil_from_days(days);
            let weekday = (days + 4).rem_euclid(7) as u32; // 1970-01-01 was a Thursday
            let day_ok = match (self.any_day, self.any_weekday) {
                (true, true) => true,
                (false, true) => self.days.contains(&day),
                (true, false) => self.weekdays.contains(&weekday),
                (false, false) => self.days.contains(&day) || self.weekdays.contains(&weekday),
            };
            if !self.months.contains(&month) || !day_ok {
                // Skip to the next local midnight.
                t = (days + 1) * 86_400 - offset;
                continue;
            }
            let hour = sod / 3600;
            if !self.hours.contains(&hour) {
                t = days * 86_400 + i64::from(hour + 1) * 3600 - offset;
                continue;
            }
            let minute = (sod % 3600) / 60;
            if !self.minutes.contains(&minute) {
                t = days * 86_400 + i64::from(hour) * 3600 + i64::from(minute + 1) * 60 - offset;
                continue;
            }
            if !self.seconds.contains(&(sod % 60)) {
                t += 1;
                continue;
            }
            return Some(t);
        }
        None
    }
}

fn field(text: &str, min: u32, max: u32, names: &[&str]) -> Result<Vec<u32>, String> {
    let mut out = Vec::new();
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (
                r,
                s.parse::<u32>()
                    .ok()
                    .filter(|s| *s > 0)
                    .ok_or_else(|| format!("bad step in {part:?}"))?,
            ),
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (value(a, min, max, names)?, value(b, min, max, names)?)
        } else {
            let v = value(range, min, max, names)?;
            if step > 1 {
                (v, max)
            } else {
                (v, v)
            }
        };
        if lo > hi {
            return Err(format!("{range:?} runs backwards"));
        }
        let mut v = lo;
        while v <= hi {
            out.push(v);
            v += step;
        }
    }
    out.sort_unstable();
    out.dedup();
    if out.is_empty() {
        return Err(format!("{text:?} matches nothing"));
    }
    Ok(out)
}

fn value(text: &str, min: u32, max: u32, names: &[&str]) -> Result<u32, String> {
    let lower = text.to_ascii_lowercase();
    let v = match names.iter().position(|n| *n == lower) {
        Some(i) if names.len() == 12 => i as u32 + 1,
        Some(i) => i as u32,
        None => text
            .parse::<u32>()
            .map_err(|_| format!("{text:?} is not a number"))?,
    };
    if v < min || v > max {
        return Err(format!("{v} is outside {min}-{max}"));
    }
    Ok(v)
}

// ------------------------------------------------------------------ time

/// Days since 1970-01-01 to (year, month, day).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// (year, month, day) to days since 1970-01-01.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-11-01T09:00:00Z`, `2026-11-01T09:00:00+02:00`, `2026-11-01T09:00`
/// (read as UTC) or `2026-11-01` to unix seconds.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, rest) = match text.split_once(['T', ' ']) {
        Some((d, r)) => (d, r),
        None => (text, "00:00:00Z"),
    };
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let (clock, zone) = if let Some(c) = rest.strip_suffix(['Z', 'z']) {
        (c, 0)
    } else if let Some(i) = rest.rfind(['+', '-']) {
        let (c, z) = rest.split_at(i);
        let sign = if z.starts_with('-') { -1 } else { 1 };
        let mut zp = z[1..].split(':');
        let zh: i64 = zp.next()?.parse().ok()?;
        let zm: i64 = zp.next().unwrap_or("0").parse().ok()?;
        (c, sign * (zh * 3600 + zm * 60))
    } else {
        (rest, 0)
    };
    let clock = clock.split('.').next()?;
    let mut cp = clock.split(':');
    let hh: i64 = cp.next()?.parse().ok()?;
    let mm: i64 = cp.next().unwrap_or("0").parse().ok()?;
    let ss: i64 = cp.next().unwrap_or("0").parse().ok()?;
    if hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss - zone)
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format_unix(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Unix seconds as local `YYYY-MM-DD HH:MM` for a zone `offset` seconds
/// east of UTC.
pub fn format_local(secs: i64, offset: i64) -> String {
    let local = secs + offset;
    let days = local.div_euclid(86_400);
    let sod = local.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        sod / 3600,
        (sod % 3600) / 60
    )
}

mod gitagent_record_time {
    /// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
    pub fn utc_now_rfc3339() -> String {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        super::format_unix(secs)
    }
}

pub use gitagent_record_time::utc_now_rfc3339;

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> i64 {
        parse_rfc3339(text).unwrap()
    }

    #[test]
    fn dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2000, 2, 29)), (2000, 2, 29));
        assert_eq!(at("2026-10-14T13:54:56Z"), 1_791_986_096);
        assert_eq!(at("2026-10-14T15:54:56+02:00"), 1_791_986_096);
        assert_eq!(at("2026-10-14T13:54:56.789Z"), 1_791_986_096);
        assert_eq!(at("2026-10-14"), 1_791_936_000);
        assert_eq!(format_unix(1_791_986_096), "2026-10-14T13:54:56Z");
        assert!(parse_rfc3339("tomorrow").is_none());
        assert!(parse_rfc3339("2026-13-01").is_none());
    }

    #[test]
    fn cron_fields_parse_and_fire() {
        // 2026-10-23 is a Friday.
        let friday_noon = at("2026-10-23T12:00:00Z");
        let c = Cron::parse("0 8 * * 1-5").unwrap();
        // The next weekday 08:00 after Friday noon is Monday.
        assert_eq!(
            format_unix(c.next_after(friday_noon, 0).unwrap()),
            "2026-10-26T08:00:00Z"
        );
        // In a zone two hours east, 08:00 local is 06:00 UTC.
        assert_eq!(
            format_unix(c.next_after(friday_noon, 7200).unwrap()),
            "2026-10-26T06:00:00Z"
        );
        let c = Cron::parse("*/15 9-17 1,15 * *").unwrap();
        assert_eq!(c.minutes, vec![0, 15, 30, 45]);
        assert_eq!(
            format_unix(c.next_after(friday_noon, 0).unwrap()),
            "2026-11-01T09:00:00Z"
        );
        let c = Cron::parse("30 2 * jan,Dec sun").unwrap();
        assert_eq!(c.months, vec![1, 12]);
        assert_eq!(c.weekdays, vec![0]);
        assert_eq!(
            format_unix(c.next_after(friday_noon, 0).unwrap()),
            "2026-12-06T02:30:00Z"
        );
        // Six fields: seconds first.
        let c = Cron::parse("*/20 * * * * *").unwrap();
        assert_eq!(c.next_after(friday_noon, 0), Some(friday_noon + 20));
        assert_eq!(c.next_after(friday_noon + 1, 0), Some(friday_noon + 20));
        // Day of month or weekday, as cron does.
        let c = Cron::parse("0 0 13 * fri").unwrap();
        assert_eq!(
            format_unix(c.next_after(friday_noon, 0).unwrap()),
            "2026-10-30T00:00:00Z"
        );
        // Sunday as 7.
        assert_eq!(Cron::parse("0 0 * * 7").unwrap().weekdays, vec![0]);
        for bad in [
            "* * *",
            "60 * * * *",
            "* * * * 8",
            "a b c d e",
            "5-1 * * * *",
            "*/0 * * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
        // Nothing matches: 31 February.
        assert!(Cron::parse("0 0 31 2 *")
            .unwrap()
            .next_after(friday_noon, 0)
            .is_none());
    }

    #[test]
    fn files_are_read_written_and_updated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("schedules")).unwrap();
        // 1.x's docs example has `name`, not `id`.
        std::fs::write(
            root.join("schedules/daily-triage.yaml"),
            "name: daily-triage\ndescription: Morning triage\ncron: \"0 8 * * 1-5\"\nprompt: |\n  Review the tickets.\n  Save a summary.\nenabled: true\n",
        )
        .unwrap();
        std::fs::write(
            root.join("schedules/later.yml"),
            "prompt: Say hello.\nrunAt: 2030-01-01T09:00:00Z\n",
        )
        .unwrap();
        std::fs::write(
            root.join("schedules/broken.yaml"),
            "prompt: x\ncron: \"a b\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("schedules/Bad Name.yaml"),
            "prompt: x\ncron: \"* * * * *\"\n",
        )
        .unwrap();
        std::fs::write(root.join("schedules/notes.txt"), "ignored\n").unwrap();
        let (found, problems) = discover(root);
        assert_eq!(
            found.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["daily-triage", "later"]
        );
        assert_eq!(found[0].prompt, "Review the tickets.\nSave a summary.");
        assert!(!found[0].once && found[0].enabled);
        assert_eq!(found[0].when(), "cron 0 8 * * 1-5");
        assert!(found[1].once);
        assert_eq!(found[1].run_at, Some(at("2030-01-01T09:00:00Z")));
        assert_eq!(found[1].when(), "once at 2030-01-01T09:00:00Z");
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("schedules/Bad Name.yaml") && problems[0].contains("dashes"));
        assert!(problems[1].contains("schedules/broken.yaml") && problems[1].contains("cron"));

        // A run is recorded without losing the other fields.
        update(
            &found[0].path,
            &[
                ("lastRunAt", Value::String("2026-10-23T08:00:00Z".into())),
                ("lastResult", Value::String("success".into())),
            ],
        )
        .unwrap();
        let again = read(&found[0].path).unwrap();
        assert_eq!(again.last_result.as_deref(), Some("success"));
        assert_eq!(again.description, "Morning triage");
        assert_eq!(again.prompt, found[0].prompt);
        update(&found[1].path, &[("enabled", Value::Bool(false))]).unwrap();
        assert!(!read(&found[1].path).unwrap().enabled);

        let path = save(
            root,
            &NewSchedule {
                id: "nightly".into(),
                description: String::new(),
                prompt: "Tidy: the notes".into(),
                cron: Some("0 2 * * *".into()),
                run_at: None,
                once: false,
            },
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("prompt: \"Tidy: the notes\"\n"), "{text}");
        assert!(
            text.contains("cron: 0 2 * * *\n") && text.contains("mode: repeat\n"),
            "{text}"
        );
        assert_eq!(read(&path).unwrap().prompt, "Tidy: the notes");
        assert!(save(
            root,
            &NewSchedule {
                id: "nightly".into(),
                description: String::new(),
                prompt: "x".into(),
                cron: Some("* * * * *".into()),
                run_at: None,
                once: false
            }
        )
        .unwrap_err()
        .contains("already exists"));
        assert!(save(
            root,
            &NewSchedule {
                id: "x".into(),
                description: String::new(),
                prompt: "x".into(),
                cron: None,
                run_at: None,
                once: false
            }
        )
        .unwrap_err()
        .contains("cron"));
    }

    #[test]
    fn yaml_scalars_are_quoted_when_needed() {
        let v = serde_json::json!({"a": "plain", "b": "yes", "c": "12", "d": "x: y", "e": "", "f": true, "g": 3, "h": ["p", "q r"], "i": {"j": "- dash"}});
        let text = to_yaml(&v);
        assert_eq!(
            text,
            "a: plain\nb: \"yes\"\nc: \"12\"\nd: \"x: y\"\ne: \"\"\nf: true\ng: 3\nh:\n  - p\n  - q r\ni:\n  j: \"- dash\"\n"
        );
        let back: Value = serde_saphyr::from_str(&text).unwrap();
        assert_eq!(back, v);
    }
}
