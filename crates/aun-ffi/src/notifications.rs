//! UniFFI facade for notifications (`Core#40`), quiet hours & prayer-time quiet windows (`E22-S2`),
//! push-device registration, and the online Event Calendar (`E22-S3`, `E22-S7`).

use std::collections::HashMap;

use aun_api::events as ev;
use aun_api::notifications as notif;

use crate::AunError;
use crate::api::ApiClient;

/// Platform bell-open mode (`Seen` clears only the badge; `Read` also marks shown items read).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NotificationMenuOpenMode {
    Seen,
    Read,
}

impl From<notif::NotificationMenuOpenMode> for NotificationMenuOpenMode {
    fn from(m: notif::NotificationMenuOpenMode) -> Self {
        match m {
            notif::NotificationMenuOpenMode::Seen => Self::Seen,
            notif::NotificationMenuOpenMode::Read => Self::Read,
        }
    }
}

/// Bell badge counts and menu-open mode (`Core#40`).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NotificationBadge {
    pub unread_count: u32,
    pub unseen_count: u32,
    pub menu_open: NotificationMenuOpenMode,
}

impl From<notif::NotificationBadge> for NotificationBadge {
    fn from(b: notif::NotificationBadge) -> Self {
        Self {
            unread_count: b.unread_count,
            unseen_count: b.unseen_count,
            menu_open: b.menu_open.into(),
        }
    }
}

/// Result of `POST /api/notifications/seen`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NotificationSeenResult {
    pub badge: NotificationBadge,
    pub read_ids: Vec<String>,
}

/// One in-app notification row.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NotificationItem {
    pub id: String,
    pub category: String,
    pub notification_type: String,
    pub notification_key: String,
    pub params_json: String,
    pub read_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

impl From<notif::NotificationItem> for NotificationItem {
    fn from(i: notif::NotificationItem) -> Self {
        Self {
            id: i.id,
            category: i.category,
            notification_type: i.notification_type,
            notification_key: i.notification_key,
            params_json: i.params_json,
            read_at_ms: i.read_at_ms,
            created_at_ms: i.created_at_ms,
        }
    }
}

/// Cursor-paginated notification list with badge counts.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NotificationPage {
    pub items: Vec<NotificationItem>,
    pub next_cursor: Option<String>,
    pub badge: NotificationBadge,
}

impl From<notif::NotificationPage> for NotificationPage {
    fn from(p: notif::NotificationPage) -> Self {
        Self {
            items: p.items.into_iter().map(Into::into).collect(),
            next_cursor: p.next_cursor,
            badge: p.badge.into(),
        }
    }
}

/// Local-day prayer quiet interval (`0..=1439` minutes from midnight).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PrayerQuietWindow {
    pub start_minute: u16,
    pub end_minute: u16,
}

impl From<PrayerQuietWindow> for notif::PrayerQuietWindow {
    fn from(w: PrayerQuietWindow) -> Self {
        Self {
            start_minute: w.start_minute,
            end_minute: w.end_minute,
        }
    }
}

impl From<notif::PrayerQuietWindow> for PrayerQuietWindow {
    fn from(w: notif::PrayerQuietWindow) -> Self {
        Self {
            start_minute: w.start_minute,
            end_minute: w.end_minute,
        }
    }
}

/// Daily quiet-hours window and optional prayer-time quiet intervals (`E22-D6`).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NotificationQuietHours {
    pub enabled: bool,
    pub timezone: String,
    pub start_minute: u16,
    pub end_minute: u16,
    pub prayer_quiet_enabled: bool,
    #[uniffi(default)]
    pub prayer_windows: Vec<PrayerQuietWindow>,
}

impl From<NotificationQuietHours> for notif::NotificationQuietHours {
    fn from(q: NotificationQuietHours) -> Self {
        Self {
            enabled: q.enabled,
            timezone: q.timezone,
            start_minute: q.start_minute,
            end_minute: q.end_minute,
            prayer_quiet_enabled: q.prayer_quiet_enabled,
            prayer_windows: q.prayer_windows.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<notif::NotificationQuietHours> for NotificationQuietHours {
    fn from(q: notif::NotificationQuietHours) -> Self {
        Self {
            enabled: q.enabled,
            timezone: q.timezone,
            start_minute: q.start_minute,
            end_minute: q.end_minute,
            prayer_quiet_enabled: q.prayer_quiet_enabled,
            prayer_windows: q.prayer_windows.into_iter().map(Into::into).collect(),
        }
    }
}

/// Notification settings snapshot for the signed-in account.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct NotificationSettings {
    pub categories: HashMap<String, bool>,
    pub always_on: Vec<String>,
    pub email_account: bool,
    pub email_locale: Option<String>,
    pub quiet_hours: NotificationQuietHours,
}

impl From<notif::NotificationSettings> for NotificationSettings {
    fn from(s: notif::NotificationSettings) -> Self {
        Self {
            categories: s.categories.into_iter().collect(),
            always_on: s.always_on,
            email_account: s.email_account,
            email_locale: s.email_locale,
            quiet_hours: s.quiet_hours.into(),
        }
    }
}

/// Native push device registration payload (`PUT /api/devices/:installId`).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PushDeviceRegistration {
    pub platform: String,
    pub provider: String,
    pub app: String,
    pub token: String,
    #[uniffi(default = None)]
    pub locale: Option<String>,
    #[uniffi(default = None)]
    pub app_version: Option<String>,
}

impl From<PushDeviceRegistration> for notif::PushDeviceRegistration {
    fn from(r: PushDeviceRegistration) -> Self {
        Self {
            platform: r.platform,
            provider: r.provider,
            app: r.app,
            token: r.token,
            locale: r.locale,
            app_version: r.app_version,
        }
    }
}

/// One upcoming online event from `GET /api/events`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
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

impl From<ev::CalendarEvent> for CalendarEvent {
    fn from(e: ev::CalendarEvent) -> Self {
        Self {
            stream_id: e.stream_id,
            channel_id: e.channel_id,
            channel_name: e.channel_name,
            channel_name_ar: e.channel_name_ar,
            channel_handle: e.channel_handle,
            channel_avatar_url: e.channel_avatar_url,
            title: e.title,
            description: e.description,
            thumbnail_url: e.thumbnail_url,
            kind: e.kind,
            event_type: e.event_type,
            status: e.status,
            scheduled_start_at: e.scheduled_start_at,
            started_at: e.started_at,
            ended_at: e.ended_at,
            premiere_video_id: e.premiere_video_id,
            reminder_count: e.reminder_count,
            viewer_reminded: e.viewer_reminded,
            watch_path: e.watch_path,
        }
    }
}

/// Cursor-paginated online Event Calendar page.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CalendarEventsPage {
    pub items: Vec<CalendarEvent>,
    pub next_cursor: Option<String>,
}

/// Query filters for `ApiClient::events_list`.
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct EventsQuery {
    #[uniffi(default = None)]
    pub channel_id: Option<String>,
    #[uniffi(default = None)]
    pub event_type: Option<String>,
    #[uniffi(default = None)]
    pub kind: Option<String>,
    #[uniffi(default = false)]
    pub subscribed_only: bool,
    #[uniffi(default = None)]
    pub from: Option<String>,
    #[uniffi(default = None)]
    pub to: Option<String>,
    #[uniffi(default = None)]
    pub cursor: Option<String>,
    #[uniffi(default = None)]
    pub limit: Option<u32>,
}

impl From<EventsQuery> for ev::EventsQuery {
    fn from(q: EventsQuery) -> Self {
        Self {
            channel_id: q.channel_id,
            event_type: q.event_type,
            kind: q.kind,
            subscribed_only: q.subscribed_only,
            from: q.from,
            to: q.to,
            cursor: q.cursor,
            limit: q.limit,
        }
    }
}

/// Formats an unseen notification count for the header bell (`None` for 0, `"9+"` / `"٩+"` past 9).
#[uniffi::export]
pub fn notification_badge_label(unseen_count: u32, locale: String) -> Option<String> {
    notif::format_badge_count(unseen_count, &locale)
}

#[uniffi::export(async_runtime = "tokio")]
impl ApiClient {
    /// Reads the notification bell badge (`unseenCount`, `unreadCount`, `menuOpen`).
    pub async fn notifications_unread_count(&self) -> Result<NotificationBadge, AunError> {
        Ok(self.inner.notifications_unread_count().await?.into())
    }

    /// Lists in-app notifications newest first.
    pub async fn notifications_list(
        &self,
        limit: u32,
        cursor: Option<String>,
        unread_only: bool,
    ) -> Result<NotificationPage, AunError> {
        Ok(self
            .inner
            .notifications_list(limit, cursor.as_deref(), unread_only)
            .await?
            .into())
    }

    /// Marks the notification menu opened (`POST /api/notifications/seen`).
    pub async fn notifications_mark_seen(
        &self,
        ids: Vec<String>,
    ) -> Result<NotificationSeenResult, AunError> {
        let (badge, read_ids) = self.inner.notifications_mark_seen(&ids).await?;
        Ok(NotificationSeenResult {
            badge: badge.into(),
            read_ids,
        })
    }

    /// Marks notifications read (`POST /api/notifications/read`).
    pub async fn notifications_mark_read(&self, ids: Vec<String>) -> Result<NotificationBadge, AunError> {
        Ok(self.inner.notifications_mark_read(&ids).await?.into())
    }

    /// Marks notifications unread (`POST /api/notifications/unread`).
    pub async fn notifications_mark_unread(&self, ids: Vec<String>) -> Result<NotificationBadge, AunError> {
        Ok(self.inner.notifications_mark_unread(&ids).await?.into())
    }

    /// Marks all notifications read (`POST /api/notifications/read-all`).
    pub async fn notifications_mark_all_read(&self) -> Result<NotificationBadge, AunError> {
        Ok(self.inner.notifications_mark_all_read().await?.into())
    }

    /// Reads notification settings including quiet hours and prayer-time quiet windows.
    pub async fn notifications_settings(&self) -> Result<NotificationSettings, AunError> {
        Ok(self.inner.notifications_settings().await?.into())
    }

    /// Updates quiet hours and prayer-time quiet windows (`PUT /api/notifications/settings`).
    pub async fn notifications_update_quiet_hours(
        &self,
        quiet_hours: NotificationQuietHours,
    ) -> Result<NotificationSettings, AunError> {
        Ok(self
            .inner
            .notifications_update_quiet_hours(&quiet_hours.into())
            .await?
            .into())
    }

    /// Registers or refreshes this install's push token (`PUT /api/devices/:installId`).
    pub async fn device_register(
        &self,
        install_id: String,
        registration: PushDeviceRegistration,
    ) -> Result<bool, AunError> {
        Ok(self
            .inner
            .device_register(&install_id, &registration.into())
            .await?)
    }

    /// Unregisters this install on sign-out (`DELETE /api/devices/:installId`).
    pub async fn device_unregister(&self, install_id: String) -> Result<(), AunError> {
        Ok(self.inner.device_unregister(&install_id).await?)
    }

    /// Reads user-visible feature flags (`GET /api/me/features`), including `eventCalendar`.
    pub async fn user_features(&self, channel_id: Option<String>) -> Result<HashMap<String, bool>, AunError> {
        Ok(self
            .inner
            .user_features(channel_id.as_deref())
            .await?
            .into_iter()
            .collect())
    }

    /// Lists upcoming online events (`GET /api/events`).
    pub async fn events_list(&self, query: EventsQuery) -> Result<CalendarEventsPage, AunError> {
        let p = self.inner.events_list(&query.into()).await?;
        Ok(CalendarEventsPage {
            items: p.items.into_iter().map(Into::into).collect(),
            next_cursor: p.next_cursor,
        })
    }

    /// Toggles a viewer reminder on an upcoming online event (`POST` / `DELETE /api/live-streams/:id/reminders`).
    pub async fn event_set_reminder(&self, stream_id: String, remind: bool) -> Result<(), AunError> {
        Ok(self.inner.event_set_reminder(&stream_id, remind).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_ffi_badge_labels() {
        assert_eq!(notification_badge_label(0, "en".into()), None);
        assert_eq!(notification_badge_label(7, "ar".into()).as_deref(), Some("٧"));
        assert_eq!(notification_badge_label(15, "ar".into()).as_deref(), Some("٩+"));
    }
}
