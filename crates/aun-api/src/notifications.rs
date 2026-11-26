//! Notification inbox, seen/unseen bell badge (`Core#40`), quiet hours & prayer-time quiet windows
//! (`E22-S2`), and push-device registration (`PUT / DELETE /api/devices/:installId`).

use std::collections::BTreeMap;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::Result;
use crate::client::{ApiClient, AuthMode, Body, RequestSpec, encode_path};

/// Platform-wide bell-open behavior (`seen` clears only the badge; `read` also marks shown items read).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotificationMenuOpenMode {
    #[default]
    Seen,
    Read,
}

/// Unread and unseen counts returned by `/api/notifications/unread-count` and list/mutation calls.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationBadge {
    pub unread_count: u32,
    pub unseen_count: u32,
    pub menu_open: NotificationMenuOpenMode,
}

impl NotificationBadge {
    /// Formatted badge text (`None` when 0, `"9+"` / `"٩+"` past 9, Eastern Arabic digits in `ar`).
    pub fn label(&self, locale: &str) -> Option<String> {
        format_badge_count(self.unseen_count, locale)
    }
}

fn to_eastern_arabic_digits(n: u32) -> String {
    n.to_string()
        .chars()
        .map(|ch| {
            ch.to_digit(10)
                .and_then(|d| char::from_u32(0x0660 + d))
                .unwrap_or(ch)
        })
        .collect()
}

/// Formats an unseen notification count for the header bell (`None` for 0, capped at `9+` / `٩+`).
pub fn format_badge_count(unseen_count: u32, locale: &str) -> Option<String> {
    if unseen_count == 0 {
        return None;
    }
    let base = locale.split(['-', '_']).next().unwrap_or(locale);
    if base.eq_ignore_ascii_case("ar") {
        if unseen_count > 9 {
            Some("٩+".to_owned())
        } else {
            Some(to_eastern_arabic_digits(unseen_count))
        }
    } else if unseen_count > 9 {
        Some("9+".to_owned())
    } else {
        Some(unseen_count.to_string())
    }
}

/// One in-app notification item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationItem {
    pub id: String,
    pub category: String,
    pub notification_type: String,
    pub notification_key: String,
    pub params_json: String,
    pub read_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

/// Cursor-paginated notification list with live badge state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPage {
    pub items: Vec<NotificationItem>,
    pub next_cursor: Option<String>,
    pub badge: NotificationBadge,
}

/// Realtime `notifications_state` WebSocket payload (`user:<id>`, `Core#40`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NotificationRealtimeEvent {
    pub unread_count: Option<u32>,
    pub unseen_count: Option<u32>,
    pub read_ids: Vec<String>,
    pub unread_ids: Vec<String>,
    pub all_read: bool,
    pub deleted_id: Option<String>,
    pub now_ms: Option<i64>,
}

/// Applies a realtime `notifications_state` delta to an in-memory [`NotificationPage`] (`Core#40`).
pub fn apply_realtime_event(page: &mut NotificationPage, event: &NotificationRealtimeEvent) {
    let stamp = event.now_ms.unwrap_or(1);
    if let Some(deleted) = event.deleted_id.as_deref() {
        page.items.retain(|item| item.id != deleted);
    }
    if event.all_read {
        for item in &mut page.items {
            if item.read_at_ms.is_none() {
                item.read_at_ms = Some(stamp);
            }
        }
    }
    if !event.read_ids.is_empty() {
        for item in &mut page.items {
            if event.read_ids.iter().any(|id| id == &item.id) && item.read_at_ms.is_none() {
                item.read_at_ms = Some(stamp);
            }
        }
    }
    if !event.unread_ids.is_empty() {
        for item in &mut page.items {
            if event.unread_ids.iter().any(|id| id == &item.id) {
                item.read_at_ms = None;
            }
        }
    }
    if let Some(u) = event.unread_count {
        page.badge.unread_count = u;
    }
    if let Some(s) = event.unseen_count {
        page.badge.unseen_count = s;
    }
}

/// Local-day prayer quiet interval (`0..=1439` minutes from midnight; zero GPS stored on server).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrayerQuietWindow {
    pub start_minute: u16,
    pub end_minute: u16,
}

/// Daily quiet-hours schedule and optional prayer-time quiet windows (`E22-D6`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationQuietHours {
    pub enabled: bool,
    pub timezone: String,
    pub start_minute: u16,
    pub end_minute: u16,
    pub prayer_quiet_enabled: bool,
    #[serde(default)]
    pub prayer_windows: Vec<PrayerQuietWindow>,
}

impl Default for NotificationQuietHours {
    fn default() -> Self {
        Self {
            enabled: false,
            timezone: "UTC".to_owned(),
            start_minute: 1320,
            end_minute: 360,
            prayer_quiet_enabled: false,
            prayer_windows: Vec::new(),
        }
    }
}

/// User notification settings including category switches, email preferences, and quiet hours.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationSettings {
    pub categories: BTreeMap<String, bool>,
    pub always_on: Vec<String>,
    pub email_account: bool,
    pub email_locale: Option<String>,
    pub quiet_hours: NotificationQuietHours,
}

/// Native push device registration payload (`PUT /api/devices/:installId`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushDeviceRegistration {
    /// `"ios"`, `"ipados"`, `"tvos"`, `"macos"`, `"android"`, `"androidtv"`, or `"windows"`.
    pub platform: String,
    /// `"apns"`, `"fcm"`, or `"wns"`.
    pub provider: String,
    /// `"viewer"`, `"studio"`, or `"admin"`.
    pub app: String,
    /// Provider token (APNs hex token, FCM registration token, or WNS channel HTTPS URI).
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    #[serde(default)]
    result: Option<T>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawCounts {
    unread_count: u32,
    unseen_count: Option<u32>,
    menu_open: Option<NotificationMenuOpenMode>,
    read_ids: Vec<String>,
}

impl RawCounts {
    fn into_badge(self) -> (NotificationBadge, Vec<String>) {
        // Fallback to unread_count only against an older server that lacks unseen_count (Core#40)
        let unseen = self.unseen_count.unwrap_or(self.unread_count);
        (
            NotificationBadge {
                unread_count: self.unread_count,
                unseen_count: unseen,
                menu_open: self.menu_open.unwrap_or_default(),
            },
            self.read_ids,
        )
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawNotificationItem {
    id: String,
    category: String,
    notification_type: String,
    notification_key: String,
    params: serde_json::Value,
    read_at: Option<i64>,
    created_at: i64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawNotificationPage {
    items: Vec<RawNotificationItem>,
    next_cursor: Option<String>,
    unread_count: u32,
    unseen_count: Option<u32>,
    menu_open: Option<NotificationMenuOpenMode>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawEmailPrefs {
    account: bool,
    locale: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RawSettingsResult {
    settings: BTreeMap<String, bool>,
    always_on: Vec<String>,
    email: RawEmailPrefs,
    quiet_hours: Option<NotificationQuietHours>,
}

impl From<RawSettingsResult> for NotificationSettings {
    fn from(r: RawSettingsResult) -> Self {
        Self {
            categories: r.settings,
            always_on: r.always_on,
            email_account: r.email.account,
            email_locale: r.email.locale,
            quiet_hours: r.quiet_hours.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawDeviceRegisterResult {
    #[serde(default)]
    created: bool,
}

impl ApiClient {
    /// Reads the bell badge (`unseenCount` + `unreadCount` + `menuOpen`).
    pub async fn notifications_unread_count(&self) -> Result<NotificationBadge> {
        let mut req = RequestSpec::new(
            Method::GET,
            "/api/notifications/unread-count".to_owned(),
            "get_notifications_unread_count",
        );
        req.auth = AuthMode::Required;
        let env: Envelope<RawCounts> = self.execute_json(req).await?;
        Ok(env.result.unwrap_or_default().into_badge().0)
    }

    /// Lists notifications newest first (`limit` clamped to `1..=50`).
    pub async fn notifications_list(
        &self,
        limit: u32,
        cursor: Option<&str>,
        unread_only: bool,
    ) -> Result<NotificationPage> {
        let mut req = RequestSpec::new(
            Method::GET,
            "/api/notifications".to_owned(),
            "get_notifications_list",
        );
        req.auth = AuthMode::Required;
        req.query
            .push(("limit".to_owned(), limit.clamp(1, 50).to_string()));
        if let Some(c) = cursor.filter(|c| !c.is_empty()) {
            req.query.push(("cursor".to_owned(), c.to_owned()));
        }
        if unread_only {
            req.query.push(("unreadOnly".to_owned(), "true".to_owned()));
        }
        let env: Envelope<RawNotificationPage> = self.execute_json(req).await?;
        let raw = env.result.unwrap_or_default();
        let unseen = raw.unseen_count.unwrap_or(raw.unread_count);
        let items = raw
            .items
            .into_iter()
            .map(|it| NotificationItem {
                id: it.id,
                category: it.category,
                notification_type: it.notification_type,
                notification_key: it.notification_key,
                params_json: serde_json::to_string(&it.params).unwrap_or_else(|_| "{}".to_owned()),
                read_at_ms: it.read_at,
                created_at_ms: it.created_at,
            })
            .collect();
        Ok(NotificationPage {
            items,
            next_cursor: raw.next_cursor,
            badge: NotificationBadge {
                unread_count: raw.unread_count,
                unseen_count: unseen,
                menu_open: raw.menu_open.unwrap_or_default(),
            },
        })
    }

    /// Marks the bell menu opened (`POST /api/notifications/seen`), returning updated badge and any `readIds`.
    pub async fn notifications_mark_seen(&self, ids: &[String]) -> Result<(NotificationBadge, Vec<String>)> {
        let capped: Vec<&String> = ids.iter().take(100).collect();
        let mut req = RequestSpec::new(
            Method::POST,
            "/api/notifications/seen".to_owned(),
            "post_notifications_seen",
        );
        req.auth = AuthMode::Required;
        req.body = Body::Json(json!({ "ids": capped }));
        let env: Envelope<RawCounts> = self.execute_json(req).await?;
        Ok(env.result.unwrap_or_default().into_badge())
    }

    /// Marks specific notifications read (`POST /api/notifications/read`).
    pub async fn notifications_mark_read(&self, ids: &[String]) -> Result<NotificationBadge> {
        let mut req = RequestSpec::new(
            Method::POST,
            "/api/notifications/read".to_owned(),
            "post_notifications_mark_read",
        );
        req.auth = AuthMode::Required;
        req.body = Body::Json(json!({ "ids": ids }));
        let env: Envelope<RawCounts> = self.execute_json(req).await?;
        Ok(env.result.unwrap_or_default().into_badge().0)
    }

    /// Marks specific notifications unread (`POST /api/notifications/unread`).
    pub async fn notifications_mark_unread(&self, ids: &[String]) -> Result<NotificationBadge> {
        let mut req = RequestSpec::new(
            Method::POST,
            "/api/notifications/unread".to_owned(),
            "post_notifications_mark_unread",
        );
        req.auth = AuthMode::Required;
        req.body = Body::Json(json!({ "ids": ids }));
        let env: Envelope<RawCounts> = self.execute_json(req).await?;
        Ok(env.result.unwrap_or_default().into_badge().0)
    }

    /// Marks all notifications read (`POST /api/notifications/read-all`).
    pub async fn notifications_mark_all_read(&self) -> Result<NotificationBadge> {
        let mut req = RequestSpec::new(
            Method::POST,
            "/api/notifications/read-all".to_owned(),
            "post_notifications_mark_all_read",
        );
        req.auth = AuthMode::Required;
        let env: Envelope<RawCounts> = self.execute_json(req).await?;
        Ok(env.result.unwrap_or_default().into_badge().0)
    }

    /// Reads notification category switches, email preferences, and quiet hours.
    pub async fn notifications_settings(&self) -> Result<NotificationSettings> {
        let mut req = RequestSpec::new(
            Method::GET,
            "/api/notifications/settings".to_owned(),
            "get_notifications_settings",
        );
        req.auth = AuthMode::Required;
        let env: Envelope<RawSettingsResult> = self.execute_json(req).await?;
        Ok(env.result.unwrap_or_default().into())
    }

    /// Updates quiet-hours and prayer-window preferences (`PUT /api/notifications/settings`).
    pub async fn notifications_update_quiet_hours(
        &self,
        quiet_hours: &NotificationQuietHours,
    ) -> Result<NotificationSettings> {
        let mut req = RequestSpec::new(
            Method::PUT,
            "/api/notifications/settings".to_owned(),
            "put_notifications_settings",
        );
        req.auth = AuthMode::Required;
        req.body = Body::Json(json!({ "quietHours": quiet_hours }));
        let env: Envelope<RawSettingsResult> = self.execute_json(req).await?;
        Ok(env.result.unwrap_or_default().into())
    }

    /// Registers or refreshes this install's push token (`PUT /api/devices/:installId`).
    pub async fn device_register(
        &self,
        install_id: &str,
        registration: &PushDeviceRegistration,
    ) -> Result<bool> {
        let encoded = encode_path(install_id)?;
        let mut req = RequestSpec::new(
            Method::PUT,
            format!("/api/devices/{encoded}"),
            "put_device_register",
        );
        req.auth = AuthMode::Required;
        let body = serde_json::to_value(registration)
            .map_err(|e| aun_common::Error::invalid_input(e.to_string()))?;
        req.body = Body::Json(body);
        let env: Envelope<RawDeviceRegisterResult> = self.execute_json(req).await?;
        Ok(env.result.unwrap_or_default().created)
    }

    /// Unregisters this install on sign-out (`DELETE /api/devices/:installId`).
    pub async fn device_unregister(&self, install_id: &str) -> Result<()> {
        let encoded = encode_path(install_id)?;
        let mut req = RequestSpec::new(
            Method::DELETE,
            format!("/api/devices/{encoded}"),
            "delete_device_unregister",
        );
        req.auth = AuthMode::Required;
        self.execute_empty(req).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::client::{ApiConfig, AuthProvider, RetryPolicy};

    struct StaticToken;
    #[async_trait::async_trait]
    impl AuthProvider for StaticToken {
        async fn access_token(&self) -> Option<String> {
            Some("tok_test".to_owned())
        }
        async fn refresh(&self) -> Result<Option<String>> {
            Ok(None)
        }
    }

    #[test]
    fn formats_badge_in_english_and_eastern_arabic() {
        assert_eq!(format_badge_count(0, "en"), None);
        assert_eq!(format_badge_count(5, "en").as_deref(), Some("5"));
        assert_eq!(format_badge_count(14, "en").as_deref(), Some("9+"));
        assert_eq!(format_badge_count(5, "ar").as_deref(), Some("٥"));
        assert_eq!(format_badge_count(12, "ar-SA").as_deref(), Some("٩+"));
    }

    #[test]
    fn applies_realtime_patch_for_seen_read_and_delete() {
        let mut page = NotificationPage {
            items: vec![
                NotificationItem {
                    id: "n1".into(),
                    category: "live".into(),
                    notification_type: "stream".into(),
                    notification_key: "liveStarted".into(),
                    params_json: "{}".into(),
                    read_at_ms: None,
                    created_at_ms: 100,
                },
                NotificationItem {
                    id: "n2".into(),
                    category: "comments".into(),
                    notification_type: "comment".into(),
                    notification_key: "newComment".into(),
                    params_json: "{}".into(),
                    read_at_ms: None,
                    created_at_ms: 200,
                },
            ],
            next_cursor: None,
            badge: NotificationBadge {
                unread_count: 2,
                unseen_count: 2,
                menu_open: NotificationMenuOpenMode::Seen,
            },
        };

        apply_realtime_event(
            &mut page,
            &NotificationRealtimeEvent {
                unread_count: Some(1),
                unseen_count: Some(0),
                read_ids: vec!["n1".into()],
                deleted_id: Some("n2".into()),
                now_ms: Some(999),
                ..Default::default()
            },
        );
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "n1");
        assert_eq!(page.items[0].read_at_ms, Some(999));
        assert_eq!(page.badge.unread_count, 1);
        assert_eq!(page.badge.unseen_count, 0);
    }

    #[tokio::test]
    async fn marks_seen_updates_quiet_hours_and_registers_device() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/notifications/seen"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "result": {
                    "unreadCount": 3,
                    "unseenCount": 0,
                    "menuOpen": "read",
                    "readIds": ["n1", "n2"]
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("PUT"))
            .and(path("/api/notifications/settings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "result": {
                    "settings": { "live": true, "new_videos": true },
                    "alwaysOn": ["account"],
                    "email": { "account": true, "locale": "ar" },
                    "quietHours": {
                        "enabled": true,
                        "timezone": "Asia/Riyadh",
                        "startMinute": 1320,
                        "endMinute": 360,
                        "prayerQuietEnabled": true,
                        "prayerWindows": [{ "startMinute": 300, "endMinute": 325 }]
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("PUT"))
            .and(path("/api/devices/inst_ios_12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "result": { "created": true }
            })))
            .mount(&server)
            .await;

        let mut cfg = ApiConfig::new(server.uri());
        cfg.retry = RetryPolicy::none();
        let client = ApiClient::new(cfg).unwrap().with_auth(Arc::new(StaticToken));

        let (badge, read_ids) = client
            .notifications_mark_seen(&["n1".into(), "n2".into()])
            .await
            .unwrap();
        assert_eq!(badge.unread_count, 3);
        assert_eq!(badge.unseen_count, 0);
        assert_eq!(badge.menu_open, NotificationMenuOpenMode::Read);
        assert_eq!(read_ids, vec!["n1", "n2"]);

        let updated = client
            .notifications_update_quiet_hours(&NotificationQuietHours {
                enabled: true,
                timezone: "Asia/Riyadh".into(),
                start_minute: 1320,
                end_minute: 360,
                prayer_quiet_enabled: true,
                prayer_windows: vec![PrayerQuietWindow {
                    start_minute: 300,
                    end_minute: 325,
                }],
            })
            .await
            .unwrap();
        assert!(updated.quiet_hours.enabled);
        assert_eq!(updated.quiet_hours.timezone, "Asia/Riyadh");
        assert_eq!(updated.quiet_hours.prayer_windows.len(), 1);

        let created = client
            .device_register(
                "inst_ios_12345",
                &PushDeviceRegistration {
                    platform: "ios".into(),
                    provider: "apns".into(),
                    app: "viewer".into(),
                    token: "a".repeat(64),
                    locale: Some("ar".into()),
                    app_version: Some("1.0.0".into()),
                },
            )
            .await
            .unwrap();
        assert!(created);
    }
}
