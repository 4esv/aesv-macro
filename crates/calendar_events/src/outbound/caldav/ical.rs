//! A lossless RFC 5545 component tree, and its mapping to Macro's event model.
//!
//! Writes edit the tree the server returned and send it back, so properties
//! Macro does not model (VTIMEZONE, X- properties, alarms it did not touch)
//! survive a round trip.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::domain::models::{
    AttendeeResponseStatus, CalendarAttendee, CalendarAttendeeInput, CalendarEventPatch,
    EventReminderOverride, EventReminders, EventStart, EventStatus, EventTime, EventTransparency,
    EventVisibility, OccurrenceRange,
};

/// Recurrence properties, kept verbatim as `CalendarEvent::recurrence_lines`.
const RECURRENCE: [&str; 4] = ["RRULE", "RDATE", "EXDATE", "EXRULE"];
/// Occurrences one series may materialize in a window.
const MAX_OCCURRENCES: u16 = 2_000;

/// One content line: `NAME;PARAM=value:VALUE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Prop {
    pub(crate) name: String,
    pub(crate) params: Vec<(String, String)>,
    pub(crate) value: String,
}

impl Prop {
    pub(crate) fn new(name: &str, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            params: Vec::new(),
            value: value.into(),
        }
    }

    pub(crate) fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn set_param(&mut self, name: &str, value: &str) {
        self.params
            .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
        self.params.push((name.into(), value.into()));
    }

    /// Parse one unfolded content line.
    pub(crate) fn parse(line: &str) -> Option<Self> {
        let mut quoted = false;
        let colon = line.char_indices().find_map(|(index, char)| match char {
            '"' => {
                quoted = !quoted;
                None
            }
            ':' if !quoted => Some(index),
            _ => None,
        })?;
        let (head, value) = (&line[..colon], &line[colon + 1..]);
        let mut parts = split_unquoted(head, ';').into_iter();
        let name = parts.next()?.to_ascii_uppercase();
        let params = parts
            .filter_map(|part| {
                let (key, value) = part.split_once('=')?;
                Some((
                    key.to_ascii_uppercase(),
                    value.trim_matches('"').to_string(),
                ))
            })
            .collect();
        Some(Self {
            name,
            params,
            value: value.into(),
        })
    }

    fn render(&self) -> String {
        // NOTE: values reach here from invitations and from the agent toolset.
        // A CR or LF would start a new content line and a `"` would end a
        // parameter's quoting, so neither survives into the output.
        let one_line = |text: &str| text.replace(['\r', '\n'], "");
        let mut line = self.name.clone();
        for (key, value) in &self.params {
            line.push(';');
            line.push_str(key);
            line.push('=');
            let value = one_line(value).replace('"', "");
            let value = value.as_str();
            if value.contains([':', ';', ',']) {
                line.push('"');
                line.push_str(value);
                line.push('"');
            } else {
                line.push_str(value);
            }
        }
        line.push(':');
        line.push_str(&one_line(&self.value));
        line
    }

    /// The text value with RFC 5545 escapes removed.
    pub(crate) fn text(&self) -> String {
        let mut out = String::with_capacity(self.value.len());
        let mut chars = self.value.chars();
        while let Some(char) = chars.next() {
            if char != '\\' {
                out.push(char);
                continue;
            }
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        }
        out
    }
}

fn split_unquoted(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (index, char) in text.char_indices() {
        if char == '"' {
            quoted = !quoted;
        } else if char == separator && !quoted {
            parts.push(&text[start..index]);
            start = index + 1;
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Escape a text value.
pub(crate) fn escape_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace("\r\n", "\\n")
        .replace(['\r', '\n'], "\\n")
}

/// A `BEGIN:`/`END:` block with its properties and nested components.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Component {
    pub(crate) name: String,
    pub(crate) props: Vec<Prop>,
    pub(crate) children: Vec<Component>,
}

impl Component {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            props: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Parse a VCALENDAR object.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let unfolded = text
            .replace("\r\n ", "")
            .replace("\r\n\t", "")
            .replace("\n ", "")
            .replace("\n\t", "");
        let mut stack: Vec<Component> = Vec::new();
        let mut root = None;
        for line in unfolded.lines().map(|line| line.trim_end_matches('\r')) {
            if line.is_empty() {
                continue;
            }
            let prop = Prop::parse(line).ok_or_else(|| format!("malformed line {line:?}"))?;
            match prop.name.as_str() {
                "BEGIN" => stack.push(Component::new(&prop.value.to_ascii_uppercase())),
                "END" => {
                    let done = stack.pop().ok_or("END without BEGIN")?;
                    if !done.name.eq_ignore_ascii_case(&prop.value) {
                        return Err(format!("END:{} closes {}", prop.value, done.name));
                    }
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(done),
                        None => root = Some(done),
                    }
                }
                _ => stack
                    .last_mut()
                    .ok_or("property outside a component")?
                    .props
                    .push(prop),
            }
        }
        if !stack.is_empty() {
            return Err("unterminated component".into());
        }
        root.filter(|root| root.name == "VCALENDAR")
            .ok_or_else(|| "no VCALENDAR".into())
    }

    /// Serialize with CRLF line ends, folded at 75 octets.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out);
        out
    }

    fn render_into(&self, out: &mut String) {
        fold_into(out, &format!("BEGIN:{}", self.name));
        for prop in &self.props {
            fold_into(out, &prop.render());
        }
        for child in &self.children {
            child.render_into(out);
        }
        fold_into(out, &format!("END:{}", self.name));
    }

    pub(crate) fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|prop| prop.name == name)
    }

    pub(crate) fn props_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Prop> {
        self.props.iter().filter(move |prop| prop.name == name)
    }

    pub(crate) fn text(&self, name: &str) -> Option<String> {
        self.prop(name)
            .map(Prop::text)
            .filter(|text| !text.is_empty())
    }

    pub(crate) fn remove(&mut self, name: &str) {
        self.props.retain(|prop| prop.name != name);
    }

    /// Replace every `name` property with one text value; empty removes it.
    pub(crate) fn set_text(&mut self, name: &str, value: &str) {
        self.remove(name);
        if !value.is_empty() {
            self.props.push(Prop::new(name, escape_text(value)));
        }
    }

    pub(crate) fn set(&mut self, prop: Prop) {
        self.remove(&prop.name);
        self.props.push(prop);
    }

    /// The occurrence key this component's RECURRENCE-ID names, when it is an
    /// exception to a series.
    pub(crate) fn recurrence_key(&self) -> Option<String> {
        self.prop("RECURRENCE-ID")
            .and_then(parse_start)
            .map(|start| start.occurrence_key())
    }

    pub(crate) fn events(&self) -> impl Iterator<Item = &Component> {
        self.children.iter().filter(|child| child.name == "VEVENT")
    }

    /// The series master: the VEVENT without RECURRENCE-ID, else the first.
    pub(crate) fn master(&self) -> Option<&Component> {
        self.events()
            .find(|event| event.prop("RECURRENCE-ID").is_none())
            .or_else(|| self.events().next())
    }

    pub(crate) fn master_mut(&mut self) -> Option<&mut Component> {
        let index = self
            .children
            .iter()
            .position(|child| child.name == "VEVENT" && child.prop("RECURRENCE-ID").is_none())
            .or_else(|| {
                self.children
                    .iter()
                    .position(|child| child.name == "VEVENT")
            })?;
        Some(&mut self.children[index])
    }
}

fn fold_into(out: &mut String, line: &str) {
    let mut width = 0;
    for char in line.chars() {
        if width + char.len_utf8() > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(char);
        width += char.len_utf8();
    }
    out.push_str("\r\n");
}

/// A DTSTART-like value: its instant or date, and the zone it was written in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Moment {
    pub(crate) start: EventStart,
    pub(crate) zone: Option<Tz>,
}

const LOCAL: &str = "%Y%m%dT%H%M%S";

pub(crate) fn parse_moment(prop: &Prop) -> Option<Moment> {
    let value = prop.value.trim();
    if prop.param("VALUE") == Some("DATE") || value.len() == 8 {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        return Some(Moment {
            start: EventStart::AllDay(date),
            zone: None,
        });
    }
    if let Some(utc) = value.strip_suffix('Z') {
        let local = NaiveDateTime::parse_from_str(utc, LOCAL).ok()?;
        return Some(Moment {
            start: EventStart::Timed(local.and_utc()),
            zone: None,
        });
    }
    let local = NaiveDateTime::parse_from_str(value, LOCAL).ok()?;
    let zone = prop.param("TZID").and_then(zone_named);
    // NOTE(shortcut): a floating time, or a TZID chrono-tz cannot name, reads as UTC. Map
    // Windows zone names and read VTIMEZONE offsets when a server sends them.
    let instant = match zone {
        Some(zone) => zone
            .from_local_datetime(&local)
            .earliest()
            .map(|at| at.with_timezone(&Utc))?,
        None => local.and_utc(),
    };
    Some(Moment {
        start: EventStart::Timed(instant),
        zone,
    })
}

fn zone_named(name: &str) -> Option<Tz> {
    if let Ok(zone) = name.parse() {
        return Some(zone);
    }
    // Some servers prefix the Olson name with a vendor path.
    let mut parts = name.rsplitn(3, '/');
    let city = parts.next()?;
    let region = parts.next()?;
    format!("{region}/{city}").parse().ok()
}

pub(crate) fn parse_start(prop: &Prop) -> Option<EventStart> {
    parse_moment(prop).map(|moment| moment.start)
}

/// Parse an occurrence key (`EventStart::occurrence_key`) back into a start.
pub(crate) fn parse_occurrence_key(key: &str) -> Option<EventStart> {
    DateTime::parse_from_rfc3339(key)
        .map(|at| EventStart::Timed(at.with_timezone(&Utc)))
        .ok()
        .or_else(|| key.parse().ok().map(EventStart::AllDay))
}

/// A DTSTART-like property for `start`, written in `zone` when one is given.
pub(crate) fn moment_prop(name: &str, start: &EventStart, zone: Option<Tz>) -> Prop {
    match (start, zone) {
        (EventStart::AllDay(date), _) => Prop {
            name: name.into(),
            params: vec![("VALUE".into(), "DATE".into())],
            value: date.format("%Y%m%d").to_string(),
        },
        (EventStart::Timed(at), Some(zone)) => Prop {
            name: name.into(),
            params: vec![("TZID".into(), zone.name().into())],
            value: at.with_timezone(&zone).format(LOCAL).to_string(),
        },
        (EventStart::Timed(at), None) => Prop::new(name, utc_stamp(*at)),
    }
}

pub(crate) fn utc_stamp(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

fn parse_duration(value: &str) -> Option<Duration> {
    let (negative, value) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let mut rest = value.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut in_time = false;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('T') {
            in_time = true;
            rest = after;
            continue;
        }
        let digits = rest.find(|c: char| !c.is_ascii_digit())?;
        let amount: i64 = rest[..digits].parse().ok()?;
        // NOTE: the amount is any i64 an invitation carries, and chrono's
        // plain constructors and `+` panic out of range, so every step is checked.
        let step = match (rest.as_bytes()[digits], in_time) {
            (b'W', false) => Duration::try_weeks(amount),
            (b'D', false) => Duration::try_days(amount),
            (b'H', true) => Duration::try_hours(amount),
            (b'M', true) => Duration::try_minutes(amount),
            (b'S', true) => Duration::try_seconds(amount),
            _ => return None,
        }?;
        total = total.checked_add(&step)?;
        rest = &rest[digits + 1..];
    }
    Some(if negative { -total } else { total })
}

/// The event's span: DTSTART to DTEND, or DTSTART plus DURATION.
pub(crate) fn event_time(event: &Component) -> Option<(EventTime, Option<Tz>)> {
    let start = parse_moment(event.prop("DTSTART")?)?;
    let end = event.prop("DTEND").and_then(parse_start);
    let duration = event
        .prop("DURATION")
        .and_then(|prop| parse_duration(&prop.value));
    let time = match start.start {
        EventStart::AllDay(start_date) => {
            let end_date = match (end, duration) {
                (Some(EventStart::AllDay(end)), _) => end,
                (_, Some(duration)) => start_date.checked_add_signed(duration)?,
                _ => start_date + Duration::days(1),
            };
            // An all-day event ending on its start day lasts that one day.
            EventTime::AllDay {
                start_date,
                end_date: end_date.max(start_date + Duration::days(1)),
            }
        }
        EventStart::Timed(starts_at) => {
            let ends_at = match (end, duration) {
                (Some(EventStart::Timed(end)), _) => end,
                (Some(EventStart::AllDay(date)), _) => date.and_hms_opt(0, 0, 0)?.and_utc(),
                (None, Some(duration)) => starts_at.checked_add_signed(duration)?,
                (None, None) => starts_at,
            };
            EventTime::Timed {
                starts_at,
                ends_at,
                time_zone: start.zone.map(|zone| zone.name().to_string()),
            }
        }
    };
    Some((time, start.zone))
}

/// Write `time` as DTSTART and DTEND, dropping any DURATION.
pub(crate) fn set_event_time(event: &mut Component, time: &EventTime) {
    event.remove("DURATION");
    match time {
        EventTime::AllDay {
            start_date,
            end_date,
        } => {
            event.set(moment_prop(
                "DTSTART",
                &EventStart::AllDay(*start_date),
                None,
            ));
            event.set(moment_prop("DTEND", &EventStart::AllDay(*end_date), None));
        }
        EventTime::Timed {
            starts_at,
            ends_at,
            time_zone,
        } => {
            let zone = time_zone.as_deref().and_then(|name| name.parse().ok());
            event.set(moment_prop("DTSTART", &EventStart::Timed(*starts_at), zone));
            event.set(moment_prop("DTEND", &EventStart::Timed(*ends_at), zone));
        }
    }
}

/// The address a `mailto:` calendar user value names, lowercased.
pub(crate) fn address(value: &str) -> String {
    let value = value.trim();
    value
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
        .map_or(value, |_| &value[7..])
        .to_ascii_lowercase()
}

pub(crate) fn partstat(status: AttendeeResponseStatus) -> &'static str {
    match status {
        AttendeeResponseStatus::NeedsAction => "NEEDS-ACTION",
        AttendeeResponseStatus::Accepted => "ACCEPTED",
        AttendeeResponseStatus::Declined => "DECLINED",
        AttendeeResponseStatus::Tentative => "TENTATIVE",
    }
}

fn response_status(partstat: Option<&str>) -> AttendeeResponseStatus {
    match partstat.map(str::to_ascii_uppercase).as_deref() {
        Some("ACCEPTED") => AttendeeResponseStatus::Accepted,
        Some("DECLINED") => AttendeeResponseStatus::Declined,
        Some("TENTATIVE") => AttendeeResponseStatus::Tentative,
        _ => AttendeeResponseStatus::NeedsAction,
    }
}

pub(crate) fn organizer(event: &Component) -> Option<String> {
    event
        .prop("ORGANIZER")
        .map(|prop| address(&prop.value))
        .filter(|email| !email.is_empty())
}

pub(crate) fn attendees(event: &Component) -> Vec<CalendarAttendee> {
    let organizer = organizer(event);
    event
        .props_named("ATTENDEE")
        .filter_map(|prop| {
            let email = address(&prop.value);
            (!email.is_empty()).then(|| CalendarAttendee {
                is_organizer: organizer.as_deref() == Some(email.as_str()),
                display_name: prop.param("CN").map(str::to_string),
                response_status: response_status(prop.param("PARTSTAT")),
                is_optional: prop
                    .param("ROLE")
                    .is_some_and(|role| role.eq_ignore_ascii_case("OPT-PARTICIPANT")),
                is_self: false,
                comment: None,
                email,
            })
        })
        .collect()
}

pub(crate) fn attendee_prop(input: &CalendarAttendeeInput) -> Prop {
    let mut prop = Prop::new("ATTENDEE", format!("mailto:{}", input.email));
    let role = if input.is_optional {
        "OPT-PARTICIPANT"
    } else {
        "REQ-PARTICIPANT"
    };
    prop.set_param("ROLE", role);
    let status = input.response_status.unwrap_or_default();
    prop.set_param("PARTSTAT", partstat(status));
    if status == AttendeeResponseStatus::NeedsAction {
        prop.set_param("RSVP", "TRUE");
    }
    prop
}

pub(crate) fn status(event: &Component) -> EventStatus {
    match event
        .prop("STATUS")
        .map(|prop| prop.value.to_ascii_uppercase())
        .as_deref()
    {
        Some("TENTATIVE") => EventStatus::Tentative,
        Some("CANCELLED") => EventStatus::Cancelled,
        _ => EventStatus::Confirmed,
    }
}

pub(crate) fn visibility(event: &Component) -> Option<EventVisibility> {
    Some(
        match event.prop("CLASS")?.value.to_ascii_uppercase().as_str() {
            "PUBLIC" => EventVisibility::Public,
            "PRIVATE" => EventVisibility::Private,
            "CONFIDENTIAL" => EventVisibility::Confidential,
            _ => EventVisibility::Default,
        },
    )
}

pub(crate) fn transparency(event: &Component) -> Option<EventTransparency> {
    Some(
        match event.prop("TRANSP")?.value.to_ascii_uppercase().as_str() {
            "TRANSPARENT" => EventTransparency::Transparent,
            _ => EventTransparency::Opaque,
        },
    )
}

/// Alarms as explicit reminders; an event without one keeps the default.
pub(crate) fn reminders(event: &Component) -> EventReminders {
    let overrides: Vec<_> = event
        .children
        .iter()
        .filter(|child| child.name == "VALARM")
        .filter_map(|alarm| {
            let before = -parse_duration(&alarm.prop("TRIGGER")?.value)?;
            let method = match alarm.prop("ACTION").map(|prop| prop.value.as_str()) {
                Some("EMAIL") => "email",
                _ => "popup",
            };
            Some(EventReminderOverride {
                method: method.into(),
                minutes: u32::try_from(before.num_minutes()).ok()?,
            })
        })
        .collect();
    if overrides.is_empty() {
        EventReminders::default()
    } else {
        EventReminders {
            use_default: false,
            overrides,
        }
    }
}

fn set_reminders(event: &mut Component, reminders: &EventReminders) {
    event.children.retain(|child| child.name != "VALARM");
    if reminders.use_default {
        return;
    }
    for reminder in &reminders.overrides {
        let mut alarm = Component::new("VALARM");
        let action = if reminder.method == "email" {
            "EMAIL"
        } else {
            "DISPLAY"
        };
        alarm.props.push(Prop::new("ACTION", action));
        alarm
            .props
            .push(Prop::new("TRIGGER", format!("-PT{}M", reminder.minutes)));
        alarm.props.push(Prop::new("DESCRIPTION", "Reminder"));
        event.children.push(alarm);
    }
}

pub(crate) fn recurrence_lines(event: &Component) -> Vec<String> {
    event
        .props
        .iter()
        .filter(|prop| RECURRENCE.contains(&prop.name.as_str()))
        .map(Prop::render)
        .collect()
}

pub(crate) fn set_recurrence_lines(event: &mut Component, lines: &[String]) {
    event
        .props
        .retain(|prop| !RECURRENCE.contains(&prop.name.as_str()));
    event
        .props
        .extend(lines.iter().filter_map(|line| Prop::parse(line.trim())));
}

/// Apply a patch's meeting fields to one VEVENT. Recurrence is left to the
/// caller, which only writes it on a master.
pub(crate) fn apply_patch(event: &mut Component, patch: &CalendarEventPatch, now: DateTime<Utc>) {
    if let Some(title) = &patch.title {
        event.set_text("SUMMARY", title);
    }
    if let Some(description) = &patch.description {
        event.set_text("DESCRIPTION", description);
    }
    if let Some(location) = &patch.location {
        event.set_text("LOCATION", location);
    }
    if let Some(time) = &patch.time {
        set_event_time(event, time);
    }
    if let Some(inputs) = &patch.attendees {
        // A kept attendee keeps their answer and name.
        let existing: BTreeMap<String, Prop> = event
            .props_named("ATTENDEE")
            .map(|prop| (address(&prop.value), prop.clone()))
            .collect();
        event.remove("ATTENDEE");
        for input in inputs {
            let mut prop = attendee_prop(input);
            if let Some(kept) = existing.get(&input.email.to_ascii_lowercase())
                && input.response_status.is_none()
            {
                prop = kept.clone();
                let role = if input.is_optional {
                    "OPT-PARTICIPANT"
                } else {
                    "REQ-PARTICIPANT"
                };
                prop.set_param("ROLE", role);
            }
            event.props.push(prop);
        }
    }
    if let Some(visibility) = patch.visibility {
        match visibility {
            EventVisibility::Default => event.remove("CLASS"),
            EventVisibility::Public => event.set(Prop::new("CLASS", "PUBLIC")),
            EventVisibility::Private => event.set(Prop::new("CLASS", "PRIVATE")),
            EventVisibility::Confidential => event.set(Prop::new("CLASS", "CONFIDENTIAL")),
        }
    }
    if let Some(transparency) = patch.transparency {
        let value = match transparency {
            EventTransparency::Opaque => "OPAQUE",
            EventTransparency::Transparent => "TRANSPARENT",
        };
        event.set(Prop::new("TRANSP", value));
    }
    if let Some(reminders) = &patch.reminders {
        set_reminders(event, reminders);
    }
    if patch.reaches_meeting() {
        bump_sequence(event);
    }
    touch(event, now);
}

/// Mark a change attendees must see as a new revision.
pub(crate) fn bump_sequence(event: &mut Component) {
    let sequence = event
        .prop("SEQUENCE")
        .and_then(|prop| prop.value.trim().parse::<u32>().ok())
        .unwrap_or(0);
    event.set(Prop::new("SEQUENCE", (sequence + 1).to_string()));
}

pub(crate) fn touch(event: &mut Component, now: DateTime<Utc>) {
    event.set(Prop::new("DTSTAMP", utc_stamp(now)));
    event.set(Prop::new("LAST-MODIFIED", utc_stamp(now)));
}

/// Reminders a draft or patch writes as VALARMs.
pub(crate) fn write_reminders(event: &mut Component, reminders: &EventReminders) {
    set_reminders(event, reminders);
}

/// A VTIMEZONE naming `zone`, so a TZID reference resolves.
///
/// NOTE(shortcut): one STANDARD block with the offset at `at`; Apple and Macro resolve the
/// TZID by its Olson name. Emit the zone's DST rules when a client that trusts the
/// definition shows a shifted time.
pub(crate) fn vtimezone(zone: Tz, at: DateTime<Utc>) -> Component {
    use chrono::Offset;
    let offset = zone.offset_from_utc_datetime(&at.naive_utc()).fix();
    let seconds = offset.local_minus_utc();
    let sign = if seconds < 0 { '-' } else { '+' };
    let hhmm = format!(
        "{sign}{:02}{:02}",
        seconds.abs() / 3600,
        seconds.abs() % 3600 / 60
    );
    let mut standard = Component::new("STANDARD");
    standard.props.push(Prop::new("DTSTART", "19700101T000000"));
    standard.props.push(Prop::new("TZOFFSETFROM", hhmm.clone()));
    standard.props.push(Prop::new("TZOFFSETTO", hhmm));
    let mut timezone = Component::new("VTIMEZONE");
    timezone.props.push(Prop::new("TZID", zone.name()));
    timezone.children.push(standard);
    timezone
}

/// Add a VTIMEZONE for every TZID the calendar references but does not define.
pub(crate) fn ensure_timezones(calendar: &mut Component, now: DateTime<Utc>) {
    let defined: Vec<String> = calendar
        .children
        .iter()
        .filter(|child| child.name == "VTIMEZONE")
        .filter_map(|child| child.prop("TZID").map(|prop| prop.value.clone()))
        .collect();
    let mut wanted: Vec<Tz> = Vec::new();
    for event in calendar.events() {
        for prop in &event.props {
            if let Some(zone) = prop.param("TZID").and_then(|name| name.parse::<Tz>().ok())
                && !defined.iter().any(|name| name == zone.name())
                && !wanted.contains(&zone)
            {
                wanted.push(zone);
            }
        }
    }
    for (index, zone) in wanted.into_iter().enumerate() {
        calendar.children.insert(index, vtimezone(zone, now));
    }
}

/// Occurrence starts of a series that can overlap `range`, as keys, in order.
/// `None` when the recurrence cannot be expanded.
pub(crate) fn expand(
    master: &Component,
    time: &EventTime,
    range: &OccurrenceRange,
) -> Option<Vec<EventStart>> {
    let lines = recurrence_lines(master);
    let start = master.prop("DTSTART")?;
    let moment = parse_moment(start)?;
    let start_line = match (&moment.start, moment.zone) {
        (EventStart::AllDay(date), _) => format!("DTSTART;VALUE=DATE:{}", date.format("%Y%m%d")),
        (EventStart::Timed(at), Some(zone)) => format!(
            "DTSTART;TZID={}:{}",
            zone.name(),
            at.with_timezone(&zone).format(LOCAL)
        ),
        (EventStart::Timed(at), None) => format!("DTSTART:{}", utc_stamp(*at)),
    };
    let set: rrule::RRuleSet = format!("{start_line}\n{}", lines.join("\n"))
        .parse()
        .map_err(|error| tracing::warn!(?error, "CalDAV recurrence does not expand"))
        .ok()?;
    let length = match time {
        EventTime::Timed {
            starts_at, ends_at, ..
        } => *ends_at - *starts_at,
        EventTime::AllDay {
            start_date,
            end_date,
        } => *end_date - *start_date,
    };
    let margin = length + Duration::days(2);
    let from = range
        .starts_at
        .min(range.start_date.and_hms_opt(0, 0, 0)?.and_utc())
        - margin;
    let until = range
        .ends_at
        .max(range.end_date.and_hms_opt(0, 0, 0)?.and_utc())
        + margin;
    let set = set
        .after(from.with_timezone(&rrule::Tz::UTC))
        .before(until.with_timezone(&rrule::Tz::UTC));
    Some(
        set.all(MAX_OCCURRENCES)
            .dates
            .into_iter()
            .map(|at| match moment.start {
                EventStart::AllDay(_) => EventStart::AllDay(at.date_naive()),
                EventStart::Timed(_) => EventStart::Timed(at.with_timezone(&Utc)),
            })
            .collect(),
    )
}

/// The span of the occurrence starting at `start`, keeping the series length.
pub(crate) fn shifted(time: &EventTime, start: &EventStart) -> EventTime {
    match (time, start) {
        (
            EventTime::Timed {
                starts_at,
                ends_at,
                time_zone,
            },
            EventStart::Timed(at),
        ) => EventTime::Timed {
            starts_at: *at,
            ends_at: *at + (*ends_at - *starts_at),
            time_zone: time_zone.clone(),
        },
        (
            EventTime::AllDay {
                start_date,
                end_date,
            },
            EventStart::AllDay(date),
        ) => EventTime::AllDay {
            start_date: *date,
            end_date: *date + (*end_date - *start_date),
        },
        // A start of the other shape cannot come from this series' rule.
        (time, _) => time.clone(),
    }
}

/// The instant an occurrence key names, for ordering against a cutoff.
pub(crate) fn instant(start: &EventStart) -> DateTime<Utc> {
    match start {
        EventStart::Timed(at) => *at,
        EventStart::AllDay(date) => date
            .and_hms_opt(0, 0, 0)
            .expect("midnight is valid")
            .and_utc(),
    }
}

/// End a series before `cutoff`: UNTIL just before it, COUNT dropped.
pub(crate) fn truncate_rrule(master: &mut Component, cutoff: &EventStart) {
    let until = match cutoff {
        EventStart::Timed(at) => utc_stamp(*at - Duration::seconds(1)),
        EventStart::AllDay(date) => (*date - Duration::days(1)).format("%Y%m%d").to_string(),
    };
    for prop in master.props.iter_mut().filter(|prop| prop.name == "RRULE") {
        let mut parts: Vec<String> = prop
            .value
            .split(';')
            .filter(|part| {
                let key = part.split('=').next().unwrap_or_default();
                !key.eq_ignore_ascii_case("UNTIL") && !key.eq_ignore_ascii_case("COUNT")
            })
            .map(str::to_string)
            .collect();
        parts.push(format!("UNTIL={until}"));
        prop.value = parts.join(";");
    }
}
