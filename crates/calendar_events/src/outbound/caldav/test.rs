use chrono::{NaiveDate, TimeZone};

use super::*;
use crate::domain::{
    acting::OwnedInbox,
    models::{CalendarProvider, EventStart},
};

fn range() -> OccurrenceRange {
    OccurrenceRange {
        starts_at: Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap(),
        ends_at: Utc.with_ymd_and_hms(2027, 3, 1, 0, 0, 0).unwrap(),
        start_date: NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
        end_date: NaiveDate::from_ymd_opt(2027, 3, 1).unwrap(),
    }
}

fn target(calendar: &str) -> ProviderCalendarTarget {
    ProviderCalendarTarget {
        binding: None,
        provider: CalendarProvider::ICloud,
        owner_id: "macro|owner".into(),
        email_link_id: Uuid::nil(),
        account_id: Uuid::nil(),
        calendar_id: Uuid::nil(),
        provider_calendar_id: calendar.into(),
        observed_access_role: Some("owner".into()),
        is_read_only: false,
        range: range(),
    }
}

fn utc(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .unwrap()
        .with_timezone(&Utc)
}

const SERIES: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:test\r\nBEGIN:VEVENT\r\nUID:series@example.com\r\nDTSTAMP:20261201T000000Z\r\nDTSTART;TZID=America/New_York:20270104T090000\r\nDTEND;TZID=America/New_York:20270104T100000\r\nRRULE:FREQ=WEEKLY;COUNT=10\r\nEXDATE;TZID=America/New_York:20270118T090000\r\nSUMMARY:Standup\\, weekly\r\nORGANIZER;CN=Boss:mailto:boss@example.com\r\nATTENDEE;PARTSTAT=ACCEPTED:mailto:boss@example.com\r\nATTENDEE;CN=\"Me, Myself\";PARTSTAT=NEEDS-ACTION;ROLE=OPT-PARTICIPANT:mailto:Me@Example.com\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:series@example.com\r\nRECURRENCE-ID;TZID=America/New_York:20270111T090000\r\nDTSTART;TZID=America/New_York:20270111T130000\r\nDTEND;TZID=America/New_York:20270111T140000\r\nSUMMARY:Standup moved\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:series@example.com\r\nRECURRENCE-ID;TZID=America/New_York:20270125T090000\r\nSTATUS:CANCELLED\r\nDTSTART;TZID=America/New_York:20270125T090000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

#[test]
fn a_series_keeps_its_zone_exceptions_and_exclusions() {
    let href = Url::parse("https://p1-caldav.icloud.com/1/calendars/home/series.ics").unwrap();
    let mapped = map_resource(
        &target("https://p1-caldav.icloud.com/1/calendars/home/"),
        &href,
        Some("\"e1\"".into()),
        SERIES,
    )
    .unwrap()
    .unwrap()
    .live()
    .unwrap();
    assert_eq!(mapped.event.title, "Standup, weekly");
    assert_eq!(mapped.event.recurrence_lines.len(), 2);
    assert_eq!(
        mapped.event.organizer_email.as_deref(),
        Some("boss@example.com")
    );
    let me = &mapped.event.attendees[1];
    assert_eq!(
        (
            me.email.as_str(),
            me.display_name.as_deref(),
            me.is_optional
        ),
        ("me@example.com", Some("Me, Myself"), true)
    );
    assert_eq!(mapped.source.details().provider_event_id, href.as_str());
    assert_eq!(mapped.source.kind(), "icloud");
    // Eight weekly starts before March, less the EXDATE on the 18th and the cancelled 25th.
    let starts: Vec<_> = mapped
        .occurrences
        .iter()
        .map(|o| o.occurrence_key.as_str())
        .collect();
    assert_eq!(starts.len(), 6, "{starts:?}");
    assert!(!starts.contains(&"2027-01-18T14:00:00+00:00"));
    assert!(!starts.contains(&"2027-01-25T14:00:00+00:00"));
    // 09:00 New York is 14:00 UTC in winter; the moved instance keeps its key.
    let moved = mapped
        .occurrences
        .iter()
        .find(|o| o.occurrence_key == "2027-01-11T14:00:00+00:00")
        .unwrap();
    assert_eq!(
        moved.time,
        EventTime::Timed {
            starts_at: utc("2027-01-11T18:00:00Z"),
            ends_at: utc("2027-01-11T19:00:00Z"),
            time_zone: Some("America/New_York".into()),
        }
    );
    assert_eq!(mapped.overrides.len(), 1);
    assert_eq!(mapped.overrides[0].title.as_deref(), Some("Standup moved"));
}

#[test]
fn a_series_crossing_daylight_saving_keeps_its_wall_clock() {
    let text = SERIES.replace("COUNT=10", "COUNT=12");
    let href = Url::parse("https://example.com/cal/series.ics").unwrap();
    let mut wide = target("https://example.com/cal/");
    wide.range.ends_at = Utc.with_ymd_and_hms(2027, 4, 1, 0, 0, 0).unwrap();
    wide.range.end_date = NaiveDate::from_ymd_opt(2027, 4, 1).unwrap();
    let mapped = map_resource(&wide, &href, None, &text)
        .unwrap()
        .unwrap()
        .live()
        .unwrap();
    // The 12th week, 22 March, is after the 14 March switch: 09:00 EDT is 13:00 UTC.
    assert!(
        mapped
            .occurrences
            .iter()
            .any(|o| o.occurrence_key == "2027-03-22T13:00:00+00:00")
    );
}

#[test]
fn rendering_keeps_what_macro_does_not_model() {
    let calendar = Component::parse(SERIES).unwrap();
    let again = Component::parse(&calendar.render()).unwrap();
    assert_eq!(calendar, again);
    let long = Prop::new("DESCRIPTION", "é".repeat(100));
    let mut event = Component::new("VEVENT");
    event.props.push(long.clone());
    let rendered = event.render();
    assert!(rendered.lines().all(|line| line.len() <= 75), "{rendered}");
    assert_eq!(
        Component::parse(&new_calendar(event).render())
            .unwrap()
            .master()
            .unwrap()
            .prop("DESCRIPTION"),
        Some(&long)
    );
}

#[test]
fn all_day_events_end_on_the_exclusive_day() {
    let text = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20270105\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let calendar = Component::parse(text).unwrap();
    let (time, _) = ical::event_time(calendar.master().unwrap()).unwrap();
    assert_eq!(
        time,
        EventTime::AllDay {
            start_date: NaiveDate::from_ymd_opt(2027, 1, 5).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2027, 1, 6).unwrap(),
        }
    );
}

#[test]
fn multistatus_reads_any_prefix_cdata_and_attributes() {
    let body = r#"<?xml version="1.0"?>
<multistatus xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <response><href>/cal/a.ics</href>
    <propstat><prop><getetag>"1"</getetag><C:calendar-data><![CDATA[BEGIN:VCALENDAR
END:VCALENDAR]]></C:calendar-data><C:supported-calendar-component-set><C:comp name="VEVENT"/></C:supported-calendar-component-set></prop><status>HTTP/1.1 200 OK</status></propstat>
    <propstat><prop><displayname/></prop><status>HTTP/1.1 404 Not Found</status></propstat>
  </response>
  <response><href>/cal/b%20c.ics</href><status>HTTP/1.1 404 Not Found</status></response>
  <sync-token>http://example.com/sync/2 &amp; more</sync-token>
</multistatus>"#;
    let root = xml::parse(body).unwrap();
    let responses: Vec<_> = root.children("response").collect();
    assert_eq!(ok_prop(responses[0], "getetag").unwrap().text, "\"1\"");
    assert!(
        ok_prop(responses[0], "calendar-data")
            .unwrap()
            .text
            .starts_with("BEGIN:VCALENDAR")
    );
    assert!(ok_prop(responses[0], "displayname").is_none());
    assert_eq!(
        ok_prop(responses[0], "comp").unwrap().attr("name"),
        Some("VEVENT")
    );
    assert_eq!(status_code(responses[1]), Some(404));
    assert_eq!(
        root.child("sync-token").unwrap().text,
        "http://example.com/sync/2 & more"
    );
}

#[test]
fn truncating_ends_the_rule_before_the_cutoff() {
    let mut calendar = Component::parse(SERIES).unwrap();
    let master = calendar.master_mut().unwrap();
    ical::truncate_rrule(master, &EventStart::Timed(utc("2027-02-01T14:00:00Z")));
    assert_eq!(
        master.prop("RRULE").unwrap().value,
        "FREQ=WEEKLY;UNTIL=20270201T135959Z"
    );
}

/// A CalDAV server to run the round trip against, as
/// `CALDAV_TEST_URL=http://127.0.0.1:5232/` with `CALDAV_TEST_CREDENTIAL=user:password`.
/// Radicale with `auth.type = none` works.
// NOTE: the server is a developer's shell setting, never an app secret.
#[allow(clippy::disallowed_methods)]
fn server() -> Option<(CalDavCalendarClient, String)> {
    let url = std::env::var("CALDAV_TEST_URL").ok()?;
    let credential = std::env::var("CALDAV_TEST_CREDENTIAL").ok()?;
    Some((
        CalDavCalendarClient::new(Client::new(), Url::parse(&url).unwrap()),
        credential,
    ))
}

fn actor(email: &str) -> ActorInboxes {
    ActorInboxes::from_owned(vec![OwnedInbox {
        link_id: Uuid::nil(),
        email: email.into(),
        provider: CalendarProvider::ICloud,
        writable_calendar_ids: Vec::new(),
    }])
    .unwrap()
}

async fn sync(
    client: &CalDavCalendarClient,
    credential: &str,
    target: &ProviderCalendarTarget,
    token: Option<&str>,
) -> GoogleEventSyncBatch {
    let plan = if token.is_some() {
        GoogleSyncPlan::Incremental
    } else {
        GoogleSyncPlan::FullSnapshot
    };
    client
        .sync_events(
            credential,
            GoogleEventSyncContext {
                target: target.clone(),
                sync_token: token.map(str::to_string),
                plan,
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn syncs_and_round_trips_an_event_against_a_caldav_server() {
    let Some((client, credential)) = server() else {
        eprintln!("CALDAV_TEST_URL is unset; skipping the CalDAV server round trip");
        return;
    };
    let user = credential.split(':').next().unwrap();
    let name = Uuid::new_v4().simple().to_string();
    let calendar = client.base_url.join(&format!("{user}/{name}/")).unwrap();
    let made = client
        .send(&credential, "MKCALENDAR", &calendar, &[], None)
        .await
        .unwrap();
    assert!(
        made.status.is_success(),
        "MKCALENDAR returned {}",
        made.status
    );

    // Discovery lists the new calendar, under the href the server spells.
    let listed = client
        .list_calendars(&credential, Uuid::nil())
        .await
        .unwrap();
    let found = listed
        .iter()
        .find(|c| c.provider_calendar_id.ends_with(&format!("/{name}/")))
        .unwrap_or_else(|| panic!("{listed:?}"));
    assert_eq!(found.access_role.as_deref(), Some("owner"));
    let target = target(&found.provider_calendar_id);

    // An empty calendar gives a sync token and nothing else.
    let first = sync(&client, &credential, &target, None).await;
    assert!(first.next_sync_token.starts_with(SYNC_PREFIX));
    assert_eq!(first.observed_provider_event_ids, Some(vec![]));

    // Create a weekly series with a guest.
    let draft = CalendarEventDraft {
        idempotency_key: Some(Uuid::new_v4()),
        title: "Planning".into(),
        description: Some("Line one\nline two; with, punctuation".into()),
        location: None,
        time: EventTime::Timed {
            starts_at: utc("2027-01-05T15:00:00Z"),
            ends_at: utc("2027-01-05T16:00:00Z"),
            time_zone: Some("Europe/Paris".into()),
        },
        attendees: vec![CalendarAttendeeInput {
            email: "guest@example.com".into(),
            is_optional: false,
            response_status: None,
        }],
        recurrence_lines: vec!["RRULE:FREQ=WEEKLY;COUNT=6".into()],
        visibility: None,
        transparency: None,
        reminders: None,
        conference: None,
        out_of_office: None,
        guest_policy: None,
    };
    let created = client
        .create_event(&credential, &target, &draft)
        .await
        .unwrap();
    let id = created.source.details().provider_event_id.clone();
    assert_eq!(created.event.description, draft.description);
    assert_eq!(created.occurrences.len(), 6);
    assert_eq!(created.event.organizer_email.as_deref(), Some(user));
    // A retried creation with the same key finds the first.
    let retried = client
        .create_event(&credential, &target, &draft)
        .await
        .unwrap();
    assert_eq!(retried.source.details().provider_event_id, id);

    // The change feed carries it.
    let second = sync(&client, &credential, &target, Some(&first.next_sync_token)).await;
    assert_eq!(second.observed_provider_event_ids, None);
    assert_eq!(second.upserts.len(), 1);
    assert_eq!(second.upserts[0].event.title, "Planning");
    // An unchanged calendar is a no-op poll.
    let idle = sync(&client, &credential, &target, Some(&second.next_sync_token)).await;
    assert!(idle.upserts.is_empty() && idle.cancelled_provider_event_ids.is_empty());

    // Update the series.
    let patch = CalendarEventPatch {
        title: Some("Planning v2".into()),
        ..CalendarEventPatch::default()
    };
    let updated = client
        .update_event(&credential, &target, &id, &patch)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (updated.event.title.as_str(), updated.event.sequence),
        ("Planning v2", 1)
    );

    // Move the second occurrence.
    let second_start = "2027-01-12T15:00:00+00:00";
    let moved = CalendarEventPatch {
        time: Some(EventTime::Timed {
            starts_at: utc("2027-01-12T17:00:00Z"),
            ends_at: utc("2027-01-12T18:00:00Z"),
            time_zone: Some("Europe/Paris".into()),
        }),
        ..CalendarEventPatch::default()
    };
    let ProviderInstanceUpdateOutcome::Applied(series) = client
        .update_event_instance(&credential, &target, &id, second_start, &moved)
        .await
        .unwrap()
    else {
        panic!("the occurrence was not patched");
    };
    assert_eq!(series.overrides.len(), 1);
    let occurrence = series
        .occurrences
        .iter()
        .find(|o| o.occurrence_key == second_start)
        .unwrap();
    assert_eq!(occurrence.time, moved.time.clone().unwrap());
    // An occurrence the rule never produced writes nothing.
    assert!(matches!(
        client
            .update_event_instance(
                &credential,
                &target,
                &id,
                "2027-01-13T15:00:00+00:00",
                &moved
            )
            .await
            .unwrap(),
        ProviderInstanceUpdateOutcome::OccurrenceGone(_)
    ));

    // The guest answers.
    let ProviderRsvpOutcome::Applied(answered) = client
        .rsvp_event(
            &credential,
            &target,
            &id,
            &actor("guest@example.com"),
            AttendeeResponseStatus::Accepted,
            &CalendarRsvpScope::All,
        )
        .await
        .unwrap()
    else {
        panic!("the RSVP was not applied");
    };
    let guest = answered
        .event
        .attendees
        .iter()
        .find(|a| a.email == "guest@example.com")
        .unwrap();
    assert_eq!(guest.response_status, AttendeeResponseStatus::Accepted);
    assert!(matches!(
        client
            .rsvp_event(
                &credential,
                &target,
                &id,
                &actor("stranger@example.com"),
                AttendeeResponseStatus::Accepted,
                &CalendarRsvpScope::All,
            )
            .await
            .unwrap(),
        ProviderRsvpOutcome::NotAttendee
    ));

    // The organizer invites one more; the guest keeps their answer.
    let ProviderInviteOutcome::Applied(invited) = client
        .add_attendees(
            &credential,
            &target,
            &id,
            &actor(user),
            &[CalendarAttendeeInput {
                email: "late@example.com".into(),
                is_optional: true,
                response_status: None,
            }],
            &CalendarUpdateScope::All,
        )
        .await
        .unwrap()
    else {
        panic!("the guest was not added");
    };
    assert_eq!(invited.event.attendees.len(), 2);
    assert!(
        invited
            .event
            .attendees
            .iter()
            .any(|a| a.email == "guest@example.com"
                && a.response_status == AttendeeResponseStatus::Accepted)
    );

    // Delete the third occurrence, then end the series before the fifth.
    let ProviderSeriesMutationOutcome::Applied(series) = client
        .delete_event_instance(&credential, &target, &id, "2027-01-19T15:00:00+00:00")
        .await
        .unwrap()
    else {
        panic!("the occurrence was not deleted");
    };
    assert_eq!(series.occurrences.len(), 5);
    let ProviderSeriesMutationOutcome::Applied(series) = client
        .truncate_recurring_event(&credential, &target, &id, "2027-02-02T15:00:00+00:00")
        .await
        .unwrap()
    else {
        panic!("the series was not truncated");
    };
    let keys: Vec<_> = series
        .occurrences
        .iter()
        .map(|o| o.occurrence_key.as_str())
        .collect();
    assert_eq!(
        keys,
        [
            "2027-01-05T15:00:00+00:00",
            second_start,
            "2027-01-26T15:00:00+00:00"
        ]
    );

    // A snapshot import lands once.
    let snapshot = EventSnapshot {
        ical_uid: "shared-meeting@example.com".into(),
        sequence: 3,
        title: "Shared".into(),
        description: None,
        location: Some("Room 1".into()),
        time: EventTime::AllDay {
            start_date: NaiveDate::from_ymd_opt(2027, 1, 20).unwrap(),
            end_date: NaiveDate::from_ymd_opt(2027, 1, 21).unwrap(),
        },
        recurrence_lines: Vec::new(),
        organizer_email: Some("host@example.com".into()),
        organizer_name: Some("Host".into()),
    };
    let ProviderImportOutcome::Imported(imported) = client
        .import_event(&credential, &target, &snapshot)
        .await
        .unwrap()
    else {
        panic!("the snapshot was not imported");
    };
    assert_eq!(imported.event.ical_uid, snapshot.ical_uid);
    assert!(matches!(
        client
            .import_event(&credential, &target, &snapshot)
            .await
            .unwrap(),
        ProviderImportOutcome::AlreadyHeld
    ));

    // A deletion reaches the change feed as a cancellation.
    let before_delete = sync(&client, &credential, &target, Some(&idle.next_sync_token)).await;
    client
        .delete_event(&credential, &target, &id)
        .await
        .unwrap();
    client
        .delete_event(&credential, &target, &id)
        .await
        .unwrap();
    let after = sync(
        &client,
        &credential,
        &target,
        Some(&before_delete.next_sync_token),
    )
    .await;
    assert_eq!(after.cancelled_provider_event_ids, vec![id.clone()]);

    // A token the server no longer knows rebuilds the snapshot.
    let rebuilt = sync(
        &client,
        &credential,
        &target,
        Some("sync:http://radicale.org/ns/sync/stale"),
    )
    .await;
    assert_eq!(
        rebuilt.observed_provider_event_ids,
        Some(vec![imported.source.details().provider_event_id.clone()])
    );

    client
        .send(&credential, "DELETE", &calendar, &[], None)
        .await
        .unwrap();
}

#[test]
fn an_out_of_range_duration_or_trigger_skips_the_event_instead_of_panicking() {
    // The first two parse but overflow a date, so the event is skipped. The
    // last is out of range even as a duration, so it is ignored like any
    // unreadable DURATION and the event keeps its start as its end.
    for (duration, skipped) in [
        ("P1000000000D", true),
        ("P9999999999W", true),
        ("PT99999999999999H", false),
    ] {
        let text = format!(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260101T000000Z\r\nDURATION:{duration}\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-{duration}\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        let calendar = Component::parse(&text).unwrap();
        let event = calendar.master().unwrap();
        assert_eq!(ical::event_time(event).is_none(), skipped, "{duration}");
        assert_eq!(
            ical::reminders(event),
            crate::domain::models::EventReminders::default(),
            "{duration}"
        );
    }
    let text = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20260101\r\nDURATION:P1000000000D\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let calendar = Component::parse(text).unwrap();
    assert!(ical::event_time(calendar.master().unwrap()).is_none());
}

#[test]
fn a_line_break_in_a_value_or_parameter_cannot_add_a_content_line() {
    let mut event = Component::new("VEVENT");
    event
        .props
        .push(ical::attendee_prop(&CalendarAttendeeInput {
            email: "a@b.com\r\nATTENDEE:mailto:x@evil.example".into(),
            is_optional: false,
            response_status: None,
        }));
    let mut organizer = ical::Prop::new("ORGANIZER", "mailto:boss@example.com");
    organizer.set_param("CN", "Boss\"\r\nATTENDEE:mailto:y@evil.example");
    event.props.push(organizer);
    event
        .props
        .push(ical::Prop::new("SUMMARY", ical::escape_text("one\rtwo")));
    let text = event.render();
    let lines: Vec<_> = text.split("\r\n").filter(|line| !line.is_empty()).collect();
    // Folding can split a long line, so count the unfolded content lines.
    let unfolded: Vec<_> = lines.iter().filter(|line| !line.starts_with(' ')).collect();
    assert_eq!(
        unfolded
            .iter()
            .filter(|line| line.starts_with("ATTENDEE"))
            .count(),
        1,
        "{text}"
    );
    assert_eq!(
        unfolded
            .iter()
            .filter(|line| line.starts_with("ORGANIZER"))
            .count(),
        1,
        "{text}"
    );
    assert!(
        !text.contains("\r\nATTENDEE:mailto:x@") && !text.contains("\r\nATTENDEE:mailto:y@"),
        "{text}"
    );
    assert!(text.contains("SUMMARY:one\\ntwo"), "{text}");
    assert_eq!(
        text.matches('\r').count(),
        text.matches("\r\n").count(),
        "{text}"
    );
}

#[test]
fn the_credential_goes_only_to_the_base_origin_or_an_https_icloud_host() {
    let icloud = CalDavCalendarClient::icloud(Client::new());
    for (url, allowed) in [
        ("https://caldav.icloud.com/123/principal/", true),
        ("https://p01-caldav.icloud.com/123/calendars/home/", true),
        ("http://p01-caldav.icloud.com/123/calendars/home/", false),
        ("https://p01-caldav.icloud.com:8443/123/", false),
        ("https://evil.example/123/", false),
        ("https://icloud.com.evil.example/123/", false),
    ] {
        let url = Url::parse(url).unwrap();
        assert_eq!(icloud.may_receive_credential(&url), allowed, "{url}");
    }
    let local =
        CalDavCalendarClient::new(Client::new(), Url::parse("http://127.0.0.1:5232/").unwrap());
    assert!(local.may_receive_credential(&Url::parse("http://127.0.0.1:5232/user/cal/").unwrap()));
    assert!(!local.may_receive_credential(&Url::parse("http://127.0.0.1:5233/user/cal/").unwrap()));
    assert!(!local.may_receive_credential(&Url::parse("https://p01-caldav.icloud.com/").unwrap()));
}
