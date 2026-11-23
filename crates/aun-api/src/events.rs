//! Unified online Event Calendar client (`GET /api/events`, `GET /api/me/features`, and Epic 17
//! stream reminder toggles; Epic 22 `E22-S3` & `E22-S7`).

use std::collections::BTreeMap;

use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::client::{ApiClient, AuthMode, RequestSpec, encode_path};

/// One scheduled or live online event from `GET /api/events` (`E22-D7`: online events only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CalendarEvent {
    pub stream_id: String,
    pub channel_id: String,
    pub channel_name: String,
    pub channel_name_ar: Option<String>,
    pub channel_handle: Option<String>,
    pub channel_avatar_url: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub thumbnail_url: Option<String>,
    pub kind: String,
    pub event_type: Option<String>,
    pub status: String,
    pub scheduled_start_at: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub premiere_video_id: Option<String>,
    pub reminder_count: u32,
    pub viewer_reminded: bool,
    pub watch_path: String,
}

/// Cursor-paginated list of upcoming online events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CalendarEventsPage {
    pub items: Vec<CalendarEvent>,
    pub next_cursor: Option<String>,
}

/// Filter parameters for [`ApiClient::events_list`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventsQuery {
    pub channel_id: Option<String>,
    pub event_type: Option<String>,
    pub kind: Option<String>,
    pub subscribed_only: bool,
    pub from: Option<String>,
    pub to: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    #[serde(default)]
    result: Option<T>,
}

#[derive(Debug, Default, Deserialize)]
struct FeaturesEnvelope {
    #[serde(default)]
    features: BTreeMap<String, bool>,
    #[serde(default)]
    result: Option<NestedFeatures>,
}

#[derive(Debug, Default, Deserialize)]
struct NestedFeatures {
    #[serde(default)]
    features: BTreeMap<String, bool>,
}

impl ApiClient {
    /// Reads user-visible feature flags (`GET /api/me/features`), including `eventCalendar`.
    pub async fn user_features(&self, channel_id: Option<&str>) -> Result<BTreeMap<String, bool>> {
        let mut req = RequestSpec::new(Method::GET, "/api/me/features".to_owned(), "get_me_features");
        req.auth = AuthMode::Optional;
        if let Some(ch) = channel_id.filter(|s| !s.is_empty()) {
            req.query.push(("channelId".to_owned(), ch.to_owned()));
        }
        let body: FeaturesEnvelope = self.execute_json(req).await?;
        if !body.features.is_empty() {
            Ok(body.features)
        } else {
            Ok(body.result.map(|r| r.features).unwrap_or_default())
        }
    }

    /// Lists upcoming online events (`GET /api/events`). Throws `NotFound` (`feature_off`) when
    /// `eventCalendar` is disabled on the server.
    pub async fn events_list(&self, query: &EventsQuery) -> Result<CalendarEventsPage> {
        let mut req = RequestSpec::new(Method::GET, "/api/events".to_owned(), "get_events_list");
        req.auth = AuthMode::Optional;
        let limit = query.limit.unwrap_or(24).clamp(1, 50);
        req.query.push(("limit".to_owned(), limit.to_string()));
        if let Some(ch) = query.channel_id.as_deref().filter(|s| !s.is_empty()) {
            req.query.push(("channelId".to_owned(), ch.to_owned()));
        }
        if let Some(et) = query.event_type.as_deref().filter(|s| !s.is_empty()) {
            req.query.push(("eventType".to_owned(), et.to_owned()));
        }
        if let Some(k) = query.kind.as_deref().filter(|s| !s.is_empty()) {
            req.query.push(("kind".to_owned(), k.to_owned()));
        }
        if query.subscribed_only {
            req.query.push(("subscribedOnly".to_owned(), "true".to_owned()));
        }
        if let Some(from) = query.from.as_deref().filter(|s| !s.is_empty()) {
            req.query.push(("from".to_owned(), from.to_owned()));
        }
        if let Some(to) = query.to.as_deref().filter(|s| !s.is_empty()) {
            req.query.push(("to".to_owned(), to.to_owned()));
        }
        if let Some(c) = query.cursor.as_deref().filter(|s| !s.is_empty()) {
            req.query.push(("cursor".to_owned(), c.to_owned()));
        }
        let env: Envelope<CalendarEventsPage> = self.execute_json(req).await?;
        Ok(env.result.unwrap_or_default())
    }

    /// Toggles a viewer reminder on an upcoming live stream (`POST` / `DELETE /api/live-streams/:id/reminders`).
    pub async fn event_set_reminder(&self, stream_id: &str, remind: bool) -> Result<()> {
        let encoded = encode_path(stream_id)?;
        let mut req = RequestSpec::new(
            if remind { Method::POST } else { Method::DELETE },
            format!("/api/live-streams/{encoded}/reminders"),
            if remind {
                "post_live_stream_reminder"
            } else {
                "delete_live_stream_reminder"
            },
        );
        req.auth = AuthMode::Required;
        self.execute_empty(req).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::client::{ApiConfig, RetryPolicy};

    #[tokio::test]
    async fn lists_events_and_reads_event_calendar_feature() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/me/features"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "result": {
                    "features": { "eventCalendar": true, "live": true }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/events"))
            .and(query_param("eventType", "halqa"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "result": {
                    "items": [{
                        "streamId": "str_1",
                        "channelId": "ch_1",
                        "channelName": "Al-Manar",
                        "channelNameAr": "المنار",
                        "title": "Weekly Halqa",
                        "kind": "live",
                        "eventType": "halqa",
                        "status": "scheduled",
                        "scheduledStartAt": "2026-11-28T19:00:00.000Z",
                        "reminderCount": 7,
                        "viewerReminded": false,
                        "watchPath": "/live/str_1"
                    }],
                    "nextCursor": null
                }
            })))
            .mount(&server)
            .await;

        let mut cfg = ApiConfig::new(server.uri());
        cfg.retry = RetryPolicy::none();
        let client = ApiClient::new(cfg).unwrap();

        let features = client.user_features(None).await.unwrap();
        assert_eq!(features.get("eventCalendar"), Some(&true));

        let page = client
            .events_list(&EventsQuery {
                event_type: Some("halqa".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].stream_id, "str_1");
        assert_eq!(page.items[0].channel_name_ar.as_deref(), Some("المنار"));
    }
}
