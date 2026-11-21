//! iCloud calendars over CalDAV (RFC 4791), signed in with an app-specific
//! password.
//!
//! The access token is the credential itself, `apple-id:app-password`, sent as
//! HTTP Basic. Ids are absolute URLs: a calendar is its collection URL and an
//! event is its resource URL, so every write addresses the resource it read.
//! Sync uses the collection's sync-token REPORT (RFC 6578) and falls back to
//! the ctag where a server has none. CalDAV has no push, so a calendar stays
//! current through the poll schedule.

mod ical;
#[cfg(test)]
mod test;
mod xml;

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use reqwest::{Client, Method, StatusCode, Url};
use uuid::Uuid;

use self::ical::{Component, Prop};
use crate::domain::{
    ics::EventSnapshot,
    models::{
        ActorInboxes, AttendeeResponseStatus, CalendarAttendeeInput, CalendarEvent,
        CalendarEventDraft, CalendarEventOverride, CalendarEventPatch, CalendarEventSource,
        CalendarEventUpsert, CalendarOccurrence, ConferenceChange, EventStart, EventStatus,
        EventTime, EventType, GoogleEventSyncBatch, GoogleSyncPlan, GoogleWatchChannel,
        GoogleWatchConfig, GuestPolicy, OccurrenceRange, ProviderCalendar, ProviderCalendarTarget,
        ProviderEventSource, ViewerCapabilities, creation_provider_id,
    },
    ports::{
        CalendarMutationProvider, CalendarProviderError, CalendarProviderErrorKind,
        CalendarRsvpScope, CalendarUpdateScope, GoogleCalendarProvider, GoogleEventSyncContext,
        ProviderImportOutcome, ProviderInstanceUpdateOutcome, ProviderInviteOutcome,
        ProviderRsvpOutcome, ProviderSeriesMutationOutcome,
    },
};

const NAMESPACES: &str = r#"xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:a="http://apple.com/ns/ical/""#;
const PRODID: &str = "-//Macro//CalDAV//EN";
/// Resources one calendar-multiget asks for.
const MULTIGET_BATCH: usize = 100;

/// CalDAV calendar adapter. iCloud is [`Self::icloud`]; any RFC 4791 server
/// with sync-collection or a ctag works.
#[derive(Clone)]
pub struct CalDavCalendarClient {
    client: Client,
    base_url: Url,
}

impl CalDavCalendarClient {
    /// iCloud's CalDAV entry point. It redirects discovery to the account's
    /// partition host, which the returned hrefs name.
    pub const ICLOUD_URL: &str = "https://caldav.icloud.com/";

    /// A client discovering calendars from `base_url`.
    pub fn new(client: Client, base_url: Url) -> Self {
        Self { client, base_url }
    }

    /// A client for iCloud.
    pub fn icloud(client: Client) -> Self {
        Self::new(
            client,
            Url::parse(Self::ICLOUD_URL).expect("the iCloud CalDAV URL is valid"),
        )
    }
}

fn error(kind: CalendarProviderErrorKind, message: impl Into<String>) -> CalendarProviderError {
    CalendarProviderError::new(kind, message)
}

fn permanent(message: impl Into<String>) -> CalendarProviderError {
    error(CalendarProviderErrorKind::Permanent, message)
}

struct DavResponse {
    status: StatusCode,
    etag: Option<String>,
    body: String,
}

/// What a PUT requires of the resource it replaces.
enum Precondition<'a> {
    /// The resource must not exist yet.
    Absent,
    /// The resource must still carry this etag, when the server gave one.
    Matches(Option<&'a str>),
}

impl CalDavCalendarClient {
    /// NOTE: every href the server returns is joined and then sent the Basic
    /// header, so an absolute href could carry the Apple ID and app password to
    /// another host or over plain http. Only the base URL's own origin, or an
    /// https iCloud partition host when the base is iCloud, gets the credential.
    fn may_receive_credential(&self, url: &Url) -> bool {
        let icloud = |url: &Url| {
            url.scheme() == "https"
                && url.port_or_known_default() == Some(443)
                && url
                    .host_str()
                    .is_some_and(|host| host == "icloud.com" || host.ends_with(".icloud.com"))
        };
        url.origin() == self.base_url.origin() || (icloud(&self.base_url) && icloud(url))
    }

    async fn send(
        &self,
        credential: &str,
        method: &str,
        url: &Url,
        headers: &[(&str, &str)],
        body: Option<(&str, String)>,
    ) -> Result<DavResponse, CalendarProviderError> {
        let (user, password) = credential.split_once(':').ok_or_else(|| {
            error(
                CalendarProviderErrorKind::ReauthRequired,
                "the CalDAV credential is not apple-id:app-password",
            )
        })?;
        if !self.may_receive_credential(url) {
            return Err(permanent(format!(
                "refusing to send the CalDAV credential to {}",
                url.origin().ascii_serialization()
            )));
        }
        let method = Method::from_bytes(method.as_bytes()).expect("DAV methods are valid tokens");
        let mut request = self
            .client
            .request(method, url.clone())
            .basic_auth(user, Some(password));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some((content_type, body)) = body {
            request = request.header("Content-Type", content_type).body(body);
        }
        let response = request.send().await.map_err(|error| {
            self::error(
                CalendarProviderErrorKind::Transient,
                format!("CalDAV request failed: {error}"),
            )
        })?;
        let status = response.status();
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = response.text().await.map_err(|error| {
            self::error(
                CalendarProviderErrorKind::Transient,
                format!("CalDAV response was cut off: {error}"),
            )
        })?;
        Ok(DavResponse { status, etag, body })
    }

    /// Send a request that must succeed.
    async fn expect(
        &self,
        credential: &str,
        method: &str,
        url: &Url,
        headers: &[(&str, &str)],
        body: Option<(&str, String)>,
    ) -> Result<DavResponse, CalendarProviderError> {
        let response = self.send(credential, method, url, headers, body).await?;
        if response.status.is_success() {
            Ok(response)
        } else {
            Err(classify(method, &response))
        }
    }

    async fn multistatus(
        &self,
        credential: &str,
        method: &str,
        url: &Url,
        depth: Option<&str>,
        body: String,
    ) -> Result<xml::Node, CalendarProviderError> {
        let mut headers = vec![];
        if let Some(depth) = depth {
            headers.push(("Depth", depth));
        }
        let response = self
            .expect(
                credential,
                method,
                url,
                &headers,
                Some(("application/xml; charset=utf-8", body)),
            )
            .await?;
        xml::parse(&response.body).map_err(|reason| {
            error(
                CalendarProviderErrorKind::Transient,
                format!("unreadable CalDAV {method} response: {reason}"),
            )
        })
    }

    async fn propfind(
        &self,
        credential: &str,
        url: &Url,
        depth: &str,
        props: &str,
    ) -> Result<xml::Node, CalendarProviderError> {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:propfind {NAMESPACES}><d:prop>{props}</d:prop></d:propfind>"#
        );
        self.multistatus(credential, "PROPFIND", url, Some(depth), body)
            .await
    }

    /// The principal URL the server says the credential signs in as.
    async fn principal(&self, credential: &str, from: &Url) -> Result<Url, CalendarProviderError> {
        let found = self
            .propfind(credential, from, "0", "<d:current-user-principal/>")
            .await?;
        Ok(found
            .find("current-user-principal")
            .and_then(|node| node.find_text("href"))
            .and_then(|href| from.join(href).ok())
            .unwrap_or_else(|| from.clone()))
    }

    async fn calendar_home(&self, credential: &str) -> Result<Url, CalendarProviderError> {
        let principal = self.principal(credential, &self.base_url).await?;
        let found = self
            .propfind(credential, &principal, "0", "<c:calendar-home-set/>")
            .await?;
        found
            .find("calendar-home-set")
            .and_then(|node| node.find_text("href"))
            .and_then(|href| principal.join(href).ok())
            .ok_or_else(|| permanent("the CalDAV principal names no calendar home"))
    }

    /// The address the account organizes meetings as.
    async fn owner_address(
        &self,
        credential: &str,
        calendar: &Url,
    ) -> Result<String, CalendarProviderError> {
        let principal = self.principal(credential, calendar).await?;
        let found = self
            .propfind(
                credential,
                &principal,
                "0",
                "<c:calendar-user-address-set/>",
            )
            .await?;
        let listed = found.find("calendar-user-address-set").and_then(|set| {
            set.children("href")
                .map(|href| href.text.trim())
                .find(|href| href.len() > 7 && href[..7].eq_ignore_ascii_case("mailto:"))
                .map(ical::address)
        });
        let user = credential.split(':').next().unwrap_or_default();
        listed
            .or_else(|| user.contains('@').then(|| user.to_ascii_lowercase()))
            .ok_or_else(|| permanent("the CalDAV account names no email address"))
    }

    async fn collection_state(
        &self,
        credential: &str,
        calendar: &Url,
    ) -> Result<String, CalendarProviderError> {
        let found = self
            .propfind(credential, calendar, "0", "<d:sync-token/><cs:getctag/>")
            .await?;
        let response = found
            .children("response")
            .next()
            .ok_or_else(|| permanent("the calendar answered PROPFIND with no response"))?;
        Ok(
            match (
                ok_prop(response, "sync-token"),
                ok_prop(response, "getctag"),
            ) {
                (Some(token), _) if !token.text.trim().is_empty() => {
                    format!("{SYNC_PREFIX}{}", token.text.trim())
                }
                (_, Some(ctag)) if !ctag.text.trim().is_empty() => {
                    format!("{CTAG_PREFIX}{}", ctag.text.trim())
                }
                // NOTE(shortcut): a server with neither rebuilds every poll. Diff resource etags
                // when such a server holds calendars big enough for that to matter.
                _ => NO_TOKEN.to_string(),
            },
        )
    }

    async fn fetch(
        &self,
        credential: &str,
        href: &Url,
    ) -> Result<Option<(Option<String>, String)>, CalendarProviderError> {
        let response = self.send(credential, "GET", href, &[], None).await?;
        match response.status {
            StatusCode::NOT_FOUND | StatusCode::GONE => Ok(None),
            status if status.is_success() => Ok(Some((response.etag, response.body))),
            _ => Err(classify("GET", &response)),
        }
    }

    /// Read a resource back and map it, as the echo of a write.
    async fn echo(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        href: &Url,
    ) -> Result<Option<CalendarEventUpsert>, CalendarProviderError> {
        let Some((etag, text)) = self.fetch(credential, href).await? else {
            return Ok(None);
        };
        Ok(map_resource(target, href, etag, &text)
            .map_err(normalization)?
            .and_then(Mapped::live))
    }

    async fn put(
        &self,
        credential: &str,
        href: &Url,
        calendar: &mut Component,
        precondition: Precondition<'_>,
    ) -> Result<DavResponse, CalendarProviderError> {
        ical::ensure_timezones(calendar, Utc::now());
        let header = match precondition {
            Precondition::Absent => Some(("If-None-Match", "*")),
            Precondition::Matches(etag) => etag.map(|etag| ("If-Match", etag)),
        };
        self.send(
            credential,
            "PUT",
            href,
            header.as_slice(),
            Some(("text/calendar; charset=utf-8", calendar.render())),
        )
        .await
    }

    /// Replace a resource read at `etag`; a concurrent change retries.
    async fn replace(
        &self,
        credential: &str,
        href: &Url,
        calendar: &mut Component,
        etag: Option<&str>,
    ) -> Result<(), CalendarProviderError> {
        let response = self
            .put(credential, href, calendar, Precondition::Matches(etag))
            .await?;
        match response.status {
            status if status.is_success() => Ok(()),
            StatusCode::PRECONDITION_FAILED => Err(error(
                CalendarProviderErrorKind::Transient,
                "the event changed at the provider while it was being written",
            )),
            _ => Err(classify("PUT", &response)),
        }
    }

    /// Read a series for a write; `None` when it is gone.
    async fn load(
        &self,
        credential: &str,
        provider_event_id: &str,
    ) -> Result<Option<(Url, Option<String>, Component)>, CalendarProviderError> {
        let href = Url::parse(provider_event_id)
            .map_err(|_| permanent("an iCloud event id is its resource URL"))?;
        let Some((etag, text)) = self.fetch(credential, &href).await? else {
            return Ok(None);
        };
        let calendar = Component::parse(&text).map_err(normalization)?;
        if calendar.master().is_none() {
            return Ok(None);
        }
        Ok(Some((href, etag, calendar)))
    }

    async fn resources(
        &self,
        credential: &str,
        calendar: &Url,
        body: String,
    ) -> Result<Vec<Resource>, CalendarProviderError> {
        let found = self
            .multistatus(credential, "REPORT", calendar, Some("1"), body)
            .await?;
        Ok(found
            .children("response")
            .filter_map(|response| {
                let href = calendar.join(response.find_text("href")?).ok()?;
                let data = ok_prop(response, "calendar-data").map(|node| node.text.clone());
                Some(Resource {
                    etag: ok_prop(response, "getetag").map(|node| node.text.trim().to_string()),
                    gone: status_code(response) == Some(404) || data.is_none(),
                    data,
                    href,
                })
            })
            .collect())
    }

    async fn snapshot(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        calendar: &Url,
        next_sync_token: String,
    ) -> Result<GoogleEventSyncBatch, CalendarProviderError> {
        let range = &target.range;
        let from = range.starts_at.min(
            range
                .start_date
                .and_hms_opt(0, 0, 0)
                .expect("midnight")
                .and_utc(),
        ) - Duration::days(1);
        let until = range.ends_at.max(
            range
                .end_date
                .and_hms_opt(0, 0, 0)
                .expect("midnight")
                .and_utc(),
        ) + Duration::days(1);
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><c:calendar-query {NAMESPACES}><d:prop><d:getetag/><c:calendar-data/></d:prop><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"><c:time-range start="{}" end="{}"/></c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#,
            ical::utc_stamp(from),
            ical::utc_stamp(until),
        );
        let mut applied = Applied::default();
        for resource in self.resources(credential, calendar, body).await? {
            applied.apply(target, resource);
        }
        Ok(GoogleEventSyncBatch {
            observed_provider_event_ids: Some(
                applied
                    .upserts
                    .iter()
                    .map(|upsert| upsert.source.details().provider_event_id.clone())
                    .collect(),
            ),
            upserts: applied.upserts,
            next_sync_token,
            materialized_range: Some(target.range.clone()),
            cancelled_provider_event_ids: Vec::new(),
        })
    }

    /// Apply the sync-collection change feed since `token`.
    async fn changes(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        calendar: &Url,
        token: &str,
    ) -> Result<GoogleEventSyncBatch, CalendarProviderError> {
        let body = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:sync-collection {NAMESPACES}><d:sync-token>{}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#,
            xml::escape(token)
        );
        let found = self
            .multistatus(credential, "REPORT", calendar, None, body)
            .await?;
        let next = found
            .child("sync-token")
            .map(|node| node.text.trim().to_string())
            .filter(|token| !token.is_empty())
            .ok_or_else(|| permanent("the sync-collection REPORT returned no sync-token"))?;
        let mut applied = Applied::default();
        let mut changed = Vec::new();
        for response in found.children("response") {
            let Some(href) = response
                .find_text("href")
                .and_then(|href| calendar.join(href).ok())
            else {
                continue;
            };
            if href.path() == calendar.path() {
                continue;
            }
            if status_code(response) == Some(404) {
                applied.cancelled.insert(href.to_string());
            } else {
                changed.push(href);
            }
        }
        for batch in changed.chunks(MULTIGET_BATCH) {
            let hrefs: String = batch
                .iter()
                .map(|href| format!("<d:href>{}</d:href>", xml::escape(href.path())))
                .collect();
            let body = format!(
                r#"<?xml version="1.0" encoding="utf-8"?><c:calendar-multiget {NAMESPACES}><d:prop><d:getetag/><c:calendar-data/></d:prop>{hrefs}</c:calendar-multiget>"#
            );
            for resource in self.resources(credential, calendar, body).await? {
                applied.apply(target, resource);
            }
        }
        Ok(GoogleEventSyncBatch {
            upserts: applied.upserts,
            observed_provider_event_ids: None,
            next_sync_token: format!("{SYNC_PREFIX}{next}"),
            materialized_range: None,
            cancelled_provider_event_ids: applied.cancelled.into_iter().collect(),
        })
    }

    /// The writable copy of one occurrence: its exception, made from the
    /// master when there is none yet. `None` when the series has no such
    /// occurrence.
    fn instance<'a>(calendar: &'a mut Component, start: &EventStart) -> Option<&'a mut Component> {
        let key = start.occurrence_key();
        if let Some(index) = calendar.children.iter().position(|child| {
            child.name == "VEVENT" && child.recurrence_key().as_ref() == Some(&key)
        }) {
            return Some(&mut calendar.children[index]);
        }
        let master = calendar.master()?;
        let (time, zone) = ical::event_time(master)?;
        let at = ical::instant(start);
        let window = OccurrenceRange {
            starts_at: at - Duration::days(1),
            ends_at: at + Duration::days(1),
            start_date: at.date_naive() - Duration::days(1),
            end_date: at.date_naive() + Duration::days(1),
        };
        if !ical::expand(master, &time, &window)?.contains(start) {
            return None;
        }
        let mut exception = master.clone();
        exception
            .props
            .retain(|prop| !matches!(prop.name.as_str(), "RRULE" | "RDATE" | "EXDATE" | "EXRULE"));
        exception.set(ical::moment_prop("RECURRENCE-ID", start, zone));
        ical::set_event_time(&mut exception, &ical::shifted(&time, start));
        calendar.children.push(exception);
        calendar.children.last_mut()
    }
}

const SYNC_PREFIX: &str = "sync:";
const CTAG_PREFIX: &str = "ctag:";
const NO_TOKEN: &str = "none";

fn classify(method: &str, response: &DavResponse) -> CalendarProviderError {
    let status = response.status;
    let kind = if response.body.contains("valid-sync-token") {
        CalendarProviderErrorKind::SyncTokenExpired
    } else if status == StatusCode::UNAUTHORIZED {
        CalendarProviderErrorKind::ReauthRequired
    } else if status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
    {
        CalendarProviderErrorKind::Transient
    } else {
        CalendarProviderErrorKind::Permanent
    };
    error(kind, format!("CalDAV {method} returned {status}"))
}

fn normalization(reason: String) -> CalendarProviderError {
    permanent(format!("unreadable iCloud event: {reason}"))
}

fn status_code(response: &xml::Node) -> Option<u16> {
    response
        .child("status")?
        .text
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// A property a response reports under a 2xx propstat.
fn ok_prop<'a>(response: &'a xml::Node, name: &str) -> Option<&'a xml::Node> {
    response
        .children("propstat")
        .filter(|propstat| status_code(propstat).is_none_or(|code| (200..300).contains(&code)))
        .find_map(|propstat| propstat.find(name))
}

fn calendar_url(target: &ProviderCalendarTarget) -> Result<Url, CalendarProviderError> {
    let mut id = target.provider_calendar_id.clone();
    if !id.ends_with('/') {
        id.push('/');
    }
    Url::parse(&id).map_err(|_| permanent("an iCloud calendar id is its collection URL"))
}

fn resource_url(calendar: &Url, name: &str) -> Url {
    calendar
        .join(&format!("{name}.ics"))
        .expect("a hex resource name joins")
}

struct Resource {
    href: Url,
    etag: Option<String>,
    data: Option<String>,
    gone: bool,
}

enum Mapped {
    Live(Box<CalendarEventUpsert>),
    Cancelled,
}

impl Mapped {
    fn live(self) -> Option<CalendarEventUpsert> {
        match self {
            Self::Live(upsert) => Some(*upsert),
            Self::Cancelled => None,
        }
    }
}

#[derive(Default)]
struct Applied {
    upserts: Vec<CalendarEventUpsert>,
    cancelled: BTreeSet<String>,
}

impl Applied {
    fn apply(&mut self, target: &ProviderCalendarTarget, resource: Resource) {
        let id = resource.href.to_string();
        let Some(data) = resource.data.filter(|_| !resource.gone) else {
            self.cancelled.insert(id);
            return;
        };
        match map_resource(target, &resource.href, resource.etag, &data) {
            Ok(Some(Mapped::Live(upsert))) => self.upserts.push(*upsert),
            Ok(Some(Mapped::Cancelled)) => {
                self.cancelled.insert(id);
            }
            // A task list or journal entry is not an event.
            Ok(None) => {}
            // One malformed resource must not stall the calendar's sync.
            Err(reason) => tracing::warn!(href = %id, %reason, "skipping unreadable CalDAV event"),
        }
    }
}

fn stamp(event: &Component, name: &str) -> Option<DateTime<Utc>> {
    let value = event
        .prop(name)?
        .value
        .trim()
        .trim_end_matches('Z')
        .to_string();
    NaiveDateTime::parse_from_str(&value, "%Y%m%dT%H%M%S")
        .ok()
        .map(|at| at.and_utc())
}

/// Map one calendar resource. `None` when it holds no VEVENT.
fn map_resource(
    target: &ProviderCalendarTarget,
    href: &Url,
    etag: Option<String>,
    text: &str,
) -> Result<Option<Mapped>, String> {
    let calendar = Component::parse(text)?;
    let Some(master) = calendar.master() else {
        return Ok(None);
    };
    if ical::status(master) == EventStatus::Cancelled {
        return Ok(Some(Mapped::Cancelled));
    }
    let (time, _) = ical::event_time(master).ok_or("the event has no DTSTART")?;
    if !time.is_valid() {
        return Err("the event ends before it starts".into());
    }
    let uid = master.text("UID").ok_or("the event has no UID")?;
    let event_id = Uuid::now_v7();
    let created_at = stamp(master, "CREATED")
        .or_else(|| stamp(master, "DTSTAMP"))
        .unwrap_or_else(Utc::now);
    let updated_at = stamp(master, "LAST-MODIFIED")
        .or_else(|| stamp(master, "DTSTAMP"))
        .unwrap_or(created_at);
    let is_series = master.prop("RECURRENCE-ID").is_none();
    let recurrence_lines = if is_series {
        ical::recurrence_lines(master)
    } else {
        Vec::new()
    };
    let event = CalendarEvent {
        id: event_id,
        owner_id: target.owner_id.clone(),
        ical_uid: uid,
        calendar_id: Some(target.calendar_id),
        sources: Vec::new(),
        title: master.text("SUMMARY").unwrap_or_default(),
        description: master.text("DESCRIPTION"),
        location: master.text("LOCATION"),
        status: ical::status(master),
        visibility: ical::visibility(master).unwrap_or_default(),
        transparency: ical::transparency(master).unwrap_or_default(),
        event_type: EventType::Default,
        time: time.clone(),
        recurrence_lines: recurrence_lines.clone(),
        organizer_email: ical::organizer(master),
        organizer_name: master
            .prop("ORGANIZER")
            .and_then(|prop| prop.param("CN"))
            .map(str::to_string),
        creator_email: None,
        creator_name: None,
        conference_url: None,
        conference_provider: None,
        sequence: master
            .prop("SEQUENCE")
            .and_then(|prop| prop.value.trim().parse().ok())
            .unwrap_or_default(),
        is_read_only: target.is_read_only,
        attendees: ical::attendees(master),
        reminders: ical::reminders(master),
        guest_policy: GuestPolicy::default(),
        is_snapshot: false,
        viewer_can: ViewerCapabilities::NONE,
        created_at,
        updated_at,
    };

    let mut overrides = Vec::new();
    let mut cancelled = BTreeSet::new();
    if is_series {
        for exception in calendar
            .events()
            .filter(|event| event.prop("RECURRENCE-ID").is_some())
        {
            let Some(original) = exception.prop("RECURRENCE-ID").and_then(ical::parse_start) else {
                continue;
            };
            let key = original.occurrence_key();
            if ical::status(exception) == EventStatus::Cancelled {
                cancelled.insert(key);
                continue;
            }
            let Some((time, _)) = ical::event_time(exception) else {
                continue;
            };
            overrides.push(CalendarEventOverride {
                reminders: None,
                automatic_decline: None,
                sequence: exception
                    .prop("SEQUENCE")
                    .and_then(|prop| prop.value.trim().parse().ok()),
                source_updated_at: stamp(exception, "LAST-MODIFIED"),
                recurrence_id: key,
                original_time: original,
                time,
                title: exception.text("SUMMARY"),
                description: exception.text("DESCRIPTION"),
                location: exception.text("LOCATION"),
                status: Some(ical::status(exception)),
                visibility: ical::visibility(exception),
                transparency: ical::transparency(exception),
                attendees: exception
                    .prop("ATTENDEE")
                    .map(|_| ical::attendees(exception)),
            });
        }
    }

    let mut occurrences = BTreeMap::<String, CalendarOccurrence>::new();
    let mut add = |key: String, recurrence_id: Option<String>, time: EventTime| {
        if time.overlaps(&target.range) {
            occurrences
                .entry(key.clone())
                .or_insert(CalendarOccurrence {
                    event_id,
                    occurrence_key: key,
                    recurrence_id,
                    time,
                    is_cancelled: false,
                });
        }
    };
    if recurrence_lines.is_empty() {
        add(time.occurrence_key(), None, time.clone());
    } else {
        let starts =
            ical::expand(master, &time, &target.range).unwrap_or_else(|| vec![time_start(&time)]);
        for start in &starts {
            let key = start.occurrence_key();
            if cancelled.contains(&key) {
                continue;
            }
            let time = overrides
                .iter()
                .find(|exception| exception.recurrence_id == key)
                .map_or_else(
                    || ical::shifted(&time, start),
                    |exception| exception.time.clone(),
                );
            add(key.clone(), Some(key), time);
        }
        // An exception moved into the window from a start outside it.
        for exception in &overrides {
            if !starts.contains(&exception.original_time) {
                let key = exception.recurrence_id.clone();
                add(key.clone(), Some(key), exception.time.clone());
            }
        }
    }

    let source = CalendarEventSource::ICloud(ProviderEventSource {
        automatic_decline: None,
        binding: None,
        email_link_id: target.email_link_id,
        account_id: target.account_id,
        calendar_id: target.calendar_id,
        observed_access_role: target.observed_access_role.clone(),
        provider_event_id: href.to_string(),
        provider_recurring_event_id: None,
        provider_etag: etag,
        raw_payload: serde_json::json!({ "ics": text }),
    });
    Ok(Some(Mapped::Live(Box::new(CalendarEventUpsert {
        event,
        source,
        overrides,
        occurrences: occurrences.into_values().collect(),
    }))))
}

fn time_start(time: &EventTime) -> EventStart {
    match time {
        EventTime::Timed { starts_at, .. } => EventStart::Timed(*starts_at),
        EventTime::AllDay { start_date, .. } => EventStart::AllDay(*start_date),
    }
}

fn reject_conference(change: Option<ConferenceChange>) -> Result<(), CalendarProviderError> {
    match change {
        Some(ConferenceChange::GoogleMeet | ConferenceChange::MicrosoftTeams) => {
            Err(permanent("iCloud calendars cannot generate a conference"))
        }
        _ => Ok(()),
    }
}

fn new_calendar(event: Component) -> Component {
    let mut calendar = Component::new("VCALENDAR");
    calendar.props.push(Prop::new("VERSION", "2.0"));
    calendar.props.push(Prop::new("PRODID", PRODID));
    calendar.children.push(event);
    calendar
}

fn new_event(uid: &str, now: DateTime<Utc>) -> Component {
    let mut event = Component::new("VEVENT");
    event.props.push(Prop::new("UID", ical::escape_text(uid)));
    event.props.push(Prop::new("CREATED", ical::utc_stamp(now)));
    ical::touch(&mut event, now);
    event
}

fn organizer_prop(email: &str, name: Option<&str>) -> Prop {
    let mut prop = Prop::new("ORGANIZER", format!("mailto:{email}"));
    if let Some(name) = name {
        prop.set_param("CN", name);
    }
    prop
}

fn rsvp_occurrence(scope: &CalendarRsvpScope) -> Option<&str> {
    match scope {
        CalendarRsvpScope::All => None,
        CalendarRsvpScope::ThisEvent { recurrence_id } => Some(recurrence_id),
    }
}

fn update_occurrence(scope: &CalendarUpdateScope) -> Option<&str> {
    match scope {
        CalendarUpdateScope::All => None,
        CalendarUpdateScope::ThisEvent { recurrence_id } => Some(recurrence_id),
    }
}

fn occurrence_start(key: &str) -> Result<EventStart, CalendarProviderError> {
    ical::parse_occurrence_key(key)
        .ok_or_else(|| permanent(format!("{key:?} is not an occurrence key")))
}

impl GoogleCalendarProvider for CalDavCalendarClient {
    #[tracing::instrument(skip(self, credential), err)]
    async fn list_calendars(
        &self,
        credential: &str,
        email_link_id: Uuid,
    ) -> Result<Vec<ProviderCalendar>, CalendarProviderError> {
        let home = self.calendar_home(credential).await?;
        let found = self
            .propfind(
                credential,
                &home,
                "1",
                "<d:resourcetype/><d:displayname/><a:calendar-color/><c:calendar-description/>\
                 <c:calendar-timezone/><c:supported-calendar-component-set/>\
                 <d:current-user-privilege-set/>",
            )
            .await?;
        let mut calendars: Vec<ProviderCalendar> = found
            .children("response")
            .filter_map(|response| {
                let href = home.join(response.find_text("href")?).ok()?;
                ok_prop(response, "resourcetype")?.child("calendar")?;
                let holds_events = ok_prop(response, "supported-calendar-component-set")
                    .is_none_or(|set| {
                        set.children("comp")
                            .any(|comp| comp.attr("name") == Some("VEVENT"))
                    });
                if !holds_events {
                    return None;
                }
                let writable = ok_prop(response, "current-user-privilege-set").is_none_or(|set| {
                    ["all", "write", "write-content"]
                        .iter()
                        .any(|privilege| set.find(privilege).is_some())
                });
                let name = ok_prop(response, "displayname")
                    .map(|node| node.text.trim().to_string())
                    .filter(|name| !name.is_empty())
                    .or_else(|| {
                        href.path_segments()?
                            .rfind(|segment| !segment.is_empty())
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                Some(ProviderCalendar {
                    provider_calendar_id: href.to_string(),
                    name,
                    description: ok_prop(response, "calendar-description")
                        .map(|node| node.text.trim().to_string())
                        .filter(|text| !text.is_empty()),
                    time_zone: ok_prop(response, "calendar-timezone").and_then(|node| {
                        node.text
                            .lines()
                            .find_map(|line| line.trim().strip_prefix("TZID:"))
                            .map(str::to_string)
                    }),
                    // Apple writes #RRGGBBAA; Macro shows #RRGGBB.
                    color: ok_prop(response, "calendar-color")
                        .map(|node| node.text.trim().chars().take(7).collect()),
                    access_role: Some(if writable { "owner" } else { "reader" }.to_string()),
                    is_primary: false,
                    is_selected: true,
                    default_reminders: Vec::new(),
                })
            })
            .collect();
        // NOTE(shortcut): iCloud names the default calendar `home`; elsewhere the first is
        // primary. Read the inbox's schedule-default-calendar-URL when a server differs.
        let primary = calendars
            .iter()
            .position(|calendar| calendar.provider_calendar_id.ends_with("/home/"))
            .unwrap_or(0);
        if let Some(calendar) = calendars.get_mut(primary) {
            calendar.is_primary = true;
        }
        tracing::debug!(%email_link_id, calendars = calendars.len(), "listed CalDAV calendars");
        Ok(calendars)
    }

    #[tracing::instrument(
        skip(self, credential, context),
        fields(provider_calendar_id = %context.target.provider_calendar_id),
        err
    )]
    async fn sync_events(
        &self,
        credential: &str,
        context: GoogleEventSyncContext,
    ) -> Result<GoogleEventSyncBatch, CalendarProviderError> {
        let target = &context.target;
        let calendar = calendar_url(target)?;
        // Read the state before the events, so a change in between shows next poll.
        let current = self.collection_state(credential, &calendar).await?;
        if context.plan == GoogleSyncPlan::Incremental {
            match context.sync_token.as_deref() {
                Some(stored) if stored == current && current != NO_TOKEN => {
                    return Ok(GoogleEventSyncBatch {
                        upserts: Vec::new(),
                        observed_provider_event_ids: None,
                        next_sync_token: current,
                        materialized_range: None,
                        cancelled_provider_event_ids: Vec::new(),
                    });
                }
                Some(stored) if stored.starts_with(SYNC_PREFIX) => {
                    let token = &stored[SYNC_PREFIX.len()..];
                    match self.changes(credential, target, &calendar, token).await {
                        Err(error)
                            if error.kind() == CalendarProviderErrorKind::SyncTokenExpired =>
                        {
                            tracing::info!("CalDAV sync-token expired, rebuilding the snapshot");
                        }
                        result => return result,
                    }
                }
                _ => {}
            }
        }
        self.snapshot(credential, target, &calendar, current).await
    }

    async fn watch_calendar(
        &self,
        _credential: &str,
        _email_link_id: Uuid,
        _provider_calendar_id: &str,
        _channel_id: Uuid,
        _config: &GoogleWatchConfig,
    ) -> Result<GoogleWatchChannel, CalendarProviderError> {
        Err(error(
            CalendarProviderErrorKind::PushUnsupported,
            "CalDAV has no push channel; the poll keeps the calendar current",
        ))
    }
}

impl CalendarMutationProvider for CalDavCalendarClient {
    async fn delete_created_event(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        creation_key: Uuid,
    ) -> Result<Vec<String>, CalendarProviderError> {
        let uid = creation_provider_id(creation_key, &target.owner_id);
        let href = resource_url(&calendar_url(target)?, &uid).to_string();
        self.delete_event(credential, target, &href).await?;
        Ok(vec![href])
    }

    async fn create_event(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        draft: &CalendarEventDraft,
    ) -> Result<CalendarEventUpsert, CalendarProviderError> {
        reject_conference(draft.conference)?;
        if draft.out_of_office.is_some() {
            return Err(permanent("iCloud calendars have no out-of-office events"));
        }
        let calendar = calendar_url(target)?;
        let uid = draft.idempotency_key.map_or_else(
            || Uuid::new_v4().simple().to_string(),
            |key| creation_provider_id(key, &target.owner_id),
        );
        let href = resource_url(&calendar, &uid);
        let now = Utc::now();
        let mut event = new_event(&uid, now);
        event.props.push(Prop::new("SEQUENCE", "0"));
        event.set_text("SUMMARY", &draft.title);
        event.set_text(
            "DESCRIPTION",
            draft.description.as_deref().unwrap_or_default(),
        );
        event.set_text("LOCATION", draft.location.as_deref().unwrap_or_default());
        ical::set_event_time(&mut event, &draft.time);
        ical::set_recurrence_lines(&mut event, &draft.recurrence_lines);
        if !draft.attendees.is_empty() {
            let organizer = self.owner_address(credential, &calendar).await?;
            event.props.push(organizer_prop(&organizer, None));
            event
                .props
                .extend(draft.attendees.iter().map(ical::attendee_prop));
        }
        let patch = CalendarEventPatch {
            visibility: draft.visibility,
            transparency: draft.transparency,
            ..CalendarEventPatch::default()
        };
        ical::apply_patch(&mut event, &patch, now);
        if let Some(reminders) = &draft.reminders {
            ical::write_reminders(&mut event, reminders);
        }
        let response = self
            .put(
                credential,
                &href,
                &mut new_calendar(event),
                Precondition::Absent,
            )
            .await?;
        // A retried creation finds its own earlier write.
        if !response.status.is_success() && response.status != StatusCode::PRECONDITION_FAILED {
            return Err(classify("PUT", &response));
        }
        self.echo(credential, target, &href).await?.ok_or_else(|| {
            error(
                CalendarProviderErrorKind::Transient,
                "the created event was gone when read back",
            )
        })
    }

    async fn update_event(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        provider_event_id: &str,
        patch: &CalendarEventPatch,
    ) -> Result<Option<CalendarEventUpsert>, CalendarProviderError> {
        reject_conference(patch.conference)?;
        let Some((href, etag, mut calendar)) = self.load(credential, provider_event_id).await?
        else {
            return Ok(None);
        };
        let master = calendar.master_mut().expect("load checked the master");
        ical::apply_patch(master, patch, Utc::now());
        if let Some(lines) = &patch.recurrence_lines {
            ical::set_recurrence_lines(master, lines);
        }
        self.replace(credential, &href, &mut calendar, etag.as_deref())
            .await?;
        self.echo(credential, target, &href).await
    }

    async fn update_event_instance(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        master_provider_event_id: &str,
        original_start: &str,
        patch: &CalendarEventPatch,
    ) -> Result<ProviderInstanceUpdateOutcome, CalendarProviderError> {
        reject_conference(patch.conference)?;
        let start = occurrence_start(original_start)?;
        let Some((href, etag, mut calendar)) =
            self.load(credential, master_provider_event_id).await?
        else {
            return Ok(ProviderInstanceUpdateOutcome::SeriesGone);
        };
        let Some(instance) = Self::instance(&mut calendar, &start) else {
            return Ok(match self.echo(credential, target, &href).await? {
                Some(series) => ProviderInstanceUpdateOutcome::OccurrenceGone(Box::new(series)),
                None => ProviderInstanceUpdateOutcome::SeriesGone,
            });
        };
        let patch = CalendarEventPatch {
            recurrence_lines: None,
            ..patch.clone()
        };
        ical::apply_patch(instance, &patch, Utc::now());
        self.replace(credential, &href, &mut calendar, etag.as_deref())
            .await?;
        Ok(match self.echo(credential, target, &href).await? {
            Some(series) => ProviderInstanceUpdateOutcome::Applied(Box::new(series)),
            None => ProviderInstanceUpdateOutcome::SeriesGone,
        })
    }

    async fn refresh_event(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        master_provider_event_id: &str,
    ) -> Result<Option<CalendarEventUpsert>, CalendarProviderError> {
        let href = Url::parse(master_provider_event_id)
            .map_err(|_| permanent("an iCloud event id is its resource URL"))?;
        self.echo(credential, target, &href).await
    }

    async fn delete_event(
        &self,
        credential: &str,
        _target: &ProviderCalendarTarget,
        provider_event_id: &str,
    ) -> Result<(), CalendarProviderError> {
        let href = Url::parse(provider_event_id)
            .map_err(|_| permanent("an iCloud event id is its resource URL"))?;
        let response = self.send(credential, "DELETE", &href, &[], None).await?;
        match response.status {
            status if status.is_success() => Ok(()),
            StatusCode::NOT_FOUND | StatusCode::GONE => Ok(()),
            _ => Err(classify("DELETE", &response)),
        }
    }

    async fn delete_event_instance(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        master_provider_event_id: &str,
        original_start: &str,
    ) -> Result<ProviderSeriesMutationOutcome, CalendarProviderError> {
        let start = occurrence_start(original_start)?;
        let Some((href, etag, mut calendar)) =
            self.load(credential, master_provider_event_id).await?
        else {
            return Ok(ProviderSeriesMutationOutcome::Gone);
        };
        let key = start.occurrence_key();
        calendar.children.retain(|child| {
            child.name != "VEVENT" || child.recurrence_key().as_ref() != Some(&key)
        });
        let now = Utc::now();
        let master = calendar.master_mut().expect("load checked the master");
        let zone = master
            .prop("DTSTART")
            .and_then(ical::parse_moment)
            .and_then(|moment| moment.zone);
        master.props.push(ical::moment_prop("EXDATE", &start, zone));
        ical::touch(master, now);
        self.replace(credential, &href, &mut calendar, etag.as_deref())
            .await?;
        Ok(match self.echo(credential, target, &href).await? {
            Some(series) => ProviderSeriesMutationOutcome::Applied(Box::new(series)),
            None => ProviderSeriesMutationOutcome::SeriesDeleted,
        })
    }

    async fn truncate_recurring_event(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        master_provider_event_id: &str,
        original_start: &str,
    ) -> Result<ProviderSeriesMutationOutcome, CalendarProviderError> {
        let cutoff = occurrence_start(original_start)?;
        let Some((href, etag, mut calendar)) =
            self.load(credential, master_provider_event_id).await?
        else {
            return Ok(ProviderSeriesMutationOutcome::Gone);
        };
        let master = calendar.master_mut().expect("load checked the master");
        let first = master.prop("DTSTART").and_then(ical::parse_start);
        if first.is_none_or(|first| ical::instant(&cutoff) <= ical::instant(&first)) {
            self.delete_event(credential, target, href.as_str()).await?;
            return Ok(ProviderSeriesMutationOutcome::SeriesDeleted);
        }
        ical::truncate_rrule(master, &cutoff);
        ical::touch(master, Utc::now());
        calendar.children.retain(|child| {
            child.name != "VEVENT"
                || child
                    .prop("RECURRENCE-ID")
                    .and_then(ical::parse_start)
                    .is_none_or(|start| ical::instant(&start) < ical::instant(&cutoff))
        });
        self.replace(credential, &href, &mut calendar, etag.as_deref())
            .await?;
        Ok(match self.echo(credential, target, &href).await? {
            Some(series) => ProviderSeriesMutationOutcome::Applied(Box::new(series)),
            None => ProviderSeriesMutationOutcome::SeriesDeleted,
        })
    }

    async fn rsvp_event(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        master_provider_event_id: &str,
        actor: &ActorInboxes,
        response: AttendeeResponseStatus,
        scope: &CalendarRsvpScope,
    ) -> Result<ProviderRsvpOutcome, CalendarProviderError> {
        let Some((href, etag, mut calendar)) =
            self.load(credential, master_provider_event_id).await?
        else {
            return Ok(ProviderRsvpOutcome::Gone);
        };
        let event = match rsvp_occurrence(scope) {
            None => calendar.master_mut(),
            Some(key) => Self::instance(&mut calendar, &occurrence_start(key)?),
        };
        let Some(event) = event else {
            return Ok(ProviderRsvpOutcome::Gone);
        };
        let Some(attendee) = event
            .props
            .iter_mut()
            .find(|prop| prop.name == "ATTENDEE" && actor.matches(&ical::address(&prop.value)))
        else {
            return Ok(ProviderRsvpOutcome::NotAttendee);
        };
        attendee.set_param("PARTSTAT", ical::partstat(response));
        attendee.params.retain(|(key, _)| key != "RSVP");
        ical::touch(event, Utc::now());
        self.replace(credential, &href, &mut calendar, etag.as_deref())
            .await?;
        Ok(match self.echo(credential, target, &href).await? {
            Some(series) => ProviderRsvpOutcome::Applied(Box::new(series)),
            None => ProviderRsvpOutcome::Gone,
        })
    }

    async fn add_attendees(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        master_provider_event_id: &str,
        actor: &ActorInboxes,
        guests: &[CalendarAttendeeInput],
        scope: &CalendarUpdateScope,
    ) -> Result<ProviderInviteOutcome, CalendarProviderError> {
        let Some((href, etag, mut calendar)) =
            self.load(credential, master_provider_event_id).await?
        else {
            return Ok(ProviderInviteOutcome::Gone);
        };
        // The organizer address is needed before the event is borrowed.
        let needs_organizer = calendar
            .master()
            .is_some_and(|master| ical::organizer(master).is_none());
        let owner = if needs_organizer {
            Some(
                self.owner_address(credential, &calendar_url(target)?)
                    .await?,
            )
        } else {
            None
        };
        let event = match update_occurrence(scope) {
            None => calendar.master_mut(),
            Some(key) => Self::instance(&mut calendar, &occurrence_start(key)?),
        };
        let Some(event) = event else {
            return Ok(match self.echo(credential, target, &href).await? {
                Some(series) => ProviderInviteOutcome::OccurrenceGone(Box::new(series)),
                None => ProviderInviteOutcome::Gone,
            });
        };
        let organizer = ical::organizer(event);
        let present: Vec<String> = event
            .props_named("ATTENDEE")
            .map(|prop| ical::address(&prop.value))
            .collect();
        let allowed = organizer
            .as_deref()
            .is_none_or(|email| actor.matches(email))
            || present.iter().any(|email| actor.matches(email));
        if !allowed {
            return Ok(ProviderInviteOutcome::NotAttendee);
        }
        let new: Vec<&CalendarAttendeeInput> = guests
            .iter()
            .filter(|guest| !present.contains(&guest.email.to_ascii_lowercase()))
            .collect();
        if !new.is_empty() {
            if organizer.is_none()
                && let Some(owner) = &owner
            {
                event.set(organizer_prop(owner, None));
            }
            event.props.extend(new.into_iter().map(ical::attendee_prop));
            ical::bump_sequence(event);
            ical::touch(event, Utc::now());
            self.replace(credential, &href, &mut calendar, etag.as_deref())
                .await?;
        }
        Ok(match self.echo(credential, target, &href).await? {
            Some(series) => ProviderInviteOutcome::Applied(Box::new(series)),
            None => ProviderInviteOutcome::Gone,
        })
    }

    async fn import_event(
        &self,
        credential: &str,
        target: &ProviderCalendarTarget,
        source: &EventSnapshot,
    ) -> Result<ProviderImportOutcome, CalendarProviderError> {
        let calendar = calendar_url(target)?;
        let name = Uuid::new_v5(&Uuid::NAMESPACE_URL, source.ical_uid.as_bytes())
            .simple()
            .to_string();
        let href = resource_url(&calendar, &name);
        let mut event = new_event(&source.ical_uid, Utc::now());
        event
            .props
            .push(Prop::new("SEQUENCE", source.sequence.to_string()));
        event.set_text("SUMMARY", &source.title);
        event.set_text(
            "DESCRIPTION",
            source.description.as_deref().unwrap_or_default(),
        );
        event.set_text("LOCATION", source.location.as_deref().unwrap_or_default());
        ical::set_event_time(&mut event, &source.time);
        ical::set_recurrence_lines(&mut event, &source.recurrence_lines);
        if let Some(organizer) = &source.organizer_email {
            event
                .props
                .push(organizer_prop(organizer, source.organizer_name.as_deref()));
        }
        let response = self
            .put(
                credential,
                &href,
                &mut new_calendar(event),
                Precondition::Absent,
            )
            .await?;
        if response.status == StatusCode::PRECONDITION_FAILED
            || response.body.contains("no-uid-conflict")
        {
            return Ok(ProviderImportOutcome::AlreadyHeld);
        }
        if !response.status.is_success() {
            return Ok(ProviderImportOutcome::Refused(classify("PUT", &response)));
        }
        Ok(match self.echo(credential, target, &href).await? {
            Some(upsert) => ProviderImportOutcome::Imported(Box::new(upsert)),
            None => ProviderImportOutcome::Refused(error(
                CalendarProviderErrorKind::Transient,
                "the imported event was gone when read back",
            )),
        })
    }

    async fn stop_watch_channel(
        &self,
        _credential: &str,
        _email_link_id: Uuid,
        _channel_id: &str,
        _resource_id: &str,
    ) -> Result<(), CalendarProviderError> {
        Ok(())
    }
}
