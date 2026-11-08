//! The server side of sync: the [`SyncTransport`] trait and [`ApiSyncTransport`] over api-videos.
//!
//! Endpoint mapping (api-videos today):
//!
//! | Sync operation | Endpoint |
//! |---|---|
//! | pull history / positions | `GET /api/watch-history?sort=newest&limit=&cursor=` |
//! | push position | `POST /api/watch-history/{videoId}/progress {progress, duration}`, falling back on 404 to `POST /api/watch-history {videoId, progress, duration}` |
//! | remove one entry | `DELETE /api/watch-history/{videoId}` |
//! | clear history | `DELETE /api/watch-history` |
//! | pull / add / remove / reorder Watch later | `GET /api/library/watch-later`, `POST /api/library/watch-later`, `DELETE /api/library/watch-later/{videoId}`, `PUT /api/library/watch-later/order` |
//! | pull liked / like / unlike | `GET /api/library/liked`, `POST` / `DELETE /api/videos/{id}/likes` |
//!
//! Notes/bookmarks have no endpoint yet; they stay local (and merge through [`crate::SyncEngine::merge_remote`]).
//! Positions and durations are **seconds** (floats) on the wire and milliseconds in the core.

use async_trait::async_trait;
use aun_api::ApiClient;
use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::VideoMeta;
use crate::time::parse_rfc3339_ms;

/// One server history row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteHistoryEntry {
    pub video_id: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub completed: bool,
    /// Server time of the last update (Unix ms); the LWW stamp for remote values.
    pub last_watched_at_ms: i64,
    pub meta: Option<VideoMeta>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RemoteHistoryPage {
    pub entries: Vec<RemoteHistoryEntry>,
    pub next_cursor: Option<String>,
}

/// A saved or liked video on the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteLibraryItem {
    pub video_id: String,
    pub added_at_ms: Option<i64>,
    pub meta: Option<VideoMeta>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RemoteLibraryPage {
    pub items: Vec<RemoteLibraryItem>,
    pub next_cursor: Option<String>,
}

/// The operations sync needs from a server. Implemented over api-videos by [`ApiSyncTransport`];
/// tests use in-memory fakes. `Send + Sync` so it can be shared with background tasks.
#[async_trait]
pub trait SyncTransport: Send + Sync {
    async fn fetch_history(&self, cursor: Option<String>, limit: u32) -> Result<RemoteHistoryPage>;
    async fn push_progress(&self, video_id: &str, position_ms: u64, duration_ms: u64) -> Result<()>;
    async fn remove_history_entry(&self, video_id: &str) -> Result<()>;
    async fn clear_history(&self) -> Result<()>;
    async fn fetch_saved(&self, cursor: Option<String>, limit: u32) -> Result<RemoteLibraryPage>;
    async fn set_saved(&self, video_id: &str, saved: bool) -> Result<()>;
    async fn reorder_saved(&self, video_ids: &[String]) -> Result<()>;
    async fn fetch_liked(&self, cursor: Option<String>, limit: u32) -> Result<RemoteLibraryPage>;
    async fn set_liked(&self, video_id: &str, liked: bool) -> Result<()>;
}

/// [`SyncTransport`] over the generated api-videos client. The client must carry the signed-in
/// account's [`aun_api::AuthProvider`].
#[derive(Debug, Clone)]
pub struct ApiSyncTransport {
    client: ApiClient,
}

impl ApiSyncTransport {
    pub fn new(client: ApiClient) -> Self {
        Self { client }
    }
}

fn secs_to_ms(s: f64) -> u64 {
    if s.is_finite() && s > 0.0 {
        (s * 1000.0).round() as u64
    } else {
        0
    }
}

fn ms_to_secs(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

fn str_field<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}

/// Parses the untyped library list responses (`watch-later`, `liked`): the array may be at the top
/// level or under `items`/`videos`/`entries`/`data`/`results` (optionally inside `result`), and each
/// element carries `videoId` or `id` (or a nested `video` object).
pub fn parse_library_page(body: &Value) -> RemoteLibraryPage {
    let root = body.get("result").filter(|r| r.is_object()).unwrap_or(body);
    let array = if let Some(a) = root.as_array() {
        Some(a)
    } else {
        ["items", "videos", "entries", "data", "results"]
            .iter()
            .find_map(|k| root.get(*k).and_then(Value::as_array))
    };
    let items = array
        .map(|a| {
            a.iter()
                .filter_map(|el| {
                    let video = el.get("video").filter(|v| v.is_object()).unwrap_or(el);
                    let id = str_field(el, &["videoId", "video_id"])
                        .or_else(|| str_field(video, &["id", "videoId"]))?;
                    let title = str_field(video, &["title", "videoTitle"]).map(str::to_owned);
                    let meta = title.map(|title| VideoMeta {
                        title,
                        thumbnail_url: str_field(video, &["thumbnailUrl", "thumbnail", "videoThumbnail"])
                            .map(str::to_owned),
                        channel_name: str_field(video, &["channelName"])
                            .or_else(|| video.get("channel").and_then(|c| str_field(c, &["name"])))
                            .map(str::to_owned),
                    });
                    Some(RemoteLibraryItem {
                        video_id: id.to_owned(),
                        added_at_ms: str_field(el, &["addedAt", "createdAt", "likedAt", "savedAt"])
                            .and_then(parse_rfc3339_ms),
                        meta,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let has_more = root.get("hasMore").and_then(Value::as_bool);
    let next_cursor = str_field(root, &["cursor", "nextCursor", "next_cursor"])
        .filter(|c| !c.is_empty() && has_more != Some(false))
        .map(str::to_owned);
    RemoteLibraryPage { items, next_cursor }
}

#[async_trait]
impl SyncTransport for ApiSyncTransport {
    async fn fetch_history(&self, cursor: Option<String>, limit: u32) -> Result<RemoteHistoryPage> {
        let params = aun_api::GetListWatchHistoryParams {
            limit: Some(f64::from(limit)),
            cursor,
            completed: None,
            sort: Some("newest".into()),
            header_channel_id: None,
        };
        let resp = self.client.get_list_watch_history(&params).await?;
        let entries = resp
            .entries
            .into_iter()
            .filter(|e| !e.video_id.is_empty())
            .map(|e| RemoteHistoryEntry {
                position_ms: secs_to_ms(e.progress),
                duration_ms: secs_to_ms(e.video_duration),
                completed: e.completed,
                last_watched_at_ms: parse_rfc3339_ms(&e.last_watched_at).unwrap_or(0),
                meta: (!e.video_title.is_empty()).then(|| VideoMeta {
                    title: e.video_title.clone(),
                    thumbnail_url: e.video_thumbnail.clone(),
                    channel_name: (!e.channel_name.is_empty()).then(|| e.channel_name.clone()),
                }),
                video_id: e.video_id,
            })
            .collect();
        Ok(RemoteHistoryPage {
            entries,
            next_cursor: resp.cursor.filter(|c| resp.has_more && !c.is_empty()),
        })
    }

    async fn push_progress(&self, video_id: &str, position_ms: u64, duration_ms: u64) -> Result<()> {
        let params = aun_api::PostUpdateWatchProgressParams {
            video_id: video_id.to_owned(),
            header_channel_id: None,
        };
        let body = aun_api::PostUpdateWatchProgressBody {
            progress: ms_to_secs(position_ms),
            duration: ms_to_secs(duration_ms),
        };
        match self.client.post_update_watch_progress(&params, &body).await {
            Ok(_) => Ok(()),
            // No history entry yet on the server: record the view (creates it with the progress).
            Err(Error::NotFound(_)) => {
                let body = aun_api::PostRecordWatchHistoryBody {
                    video_id: video_id.to_owned(),
                    progress: Some(ms_to_secs(position_ms)),
                    duration: ms_to_secs(duration_ms),
                };
                self.client
                    .post_record_watch_history(&aun_api::PostRecordWatchHistoryParams::default(), &body)
                    .await
                    .map(|_| ())
            }
            Err(e) => Err(e),
        }
    }

    async fn remove_history_entry(&self, video_id: &str) -> Result<()> {
        let params = aun_api::DeleteDeleteWatchHistoryParams {
            video_id: video_id.to_owned(),
            header_channel_id: None,
        };
        match self.client.delete_delete_watch_history(&params).await {
            Ok(()) | Err(Error::NotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn clear_history(&self) -> Result<()> {
        self.client.delete_clear_watch_history().await.map(|_| ())
    }

    async fn fetch_saved(&self, cursor: Option<String>, limit: u32) -> Result<RemoteLibraryPage> {
        let params = aun_api::GetWatchLaterListParams {
            limit: Some(i64::from(limit)),
            cursor,
            header_channel_id: None,
        };
        Ok(parse_library_page(
            &self.client.get_watch_later_list(&params).await?,
        ))
    }

    async fn set_saved(&self, video_id: &str, saved: bool) -> Result<()> {
        let r = if saved {
            self.client
                .post_watch_later_add(
                    &aun_api::PostWatchLaterAddParams::default(),
                    &aun_api::PostWatchLaterAddBody {
                        video_id: video_id.to_owned(),
                    },
                )
                .await
        } else {
            self.client
                .delete_watch_later_remove(&aun_api::DeleteWatchLaterRemoveParams {
                    video_id: video_id.to_owned(),
                    header_channel_id: None,
                })
                .await
        };
        r.map(|_| ())
    }

    async fn reorder_saved(&self, video_ids: &[String]) -> Result<()> {
        self.client
            .put_watch_later_reorder(
                &aun_api::PutWatchLaterReorderParams::default(),
                &aun_api::PutWatchLaterReorderBody {
                    video_ids: video_ids.to_vec(),
                },
            )
            .await
            .map(|_| ())
    }

    async fn fetch_liked(&self, cursor: Option<String>, limit: u32) -> Result<RemoteLibraryPage> {
        let params = aun_api::GetLikedVideosListParams {
            limit: Some(i64::from(limit)),
            cursor,
            header_channel_id: None,
        };
        Ok(parse_library_page(
            &self.client.get_liked_videos_list(&params).await?,
        ))
    }

    async fn set_liked(&self, video_id: &str, liked: bool) -> Result<()> {
        let r = if liked {
            self.client
                .post_like_create(&aun_api::PostLikeCreateParams {
                    id: video_id.to_owned(),
                    header_channel_id: None,
                })
                .await
        } else {
            self.client
                .delete_like_delete(&aun_api::DeleteLikeDeleteParams {
                    id: video_id.to_owned(),
                    header_channel_id: None,
                })
                .await
        };
        match r {
            // Already liked / not liked: the desired state holds.
            Ok(_) | Err(Error::Conflict(_)) => Ok(()),
            Err(Error::NotFound(_)) if !liked => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_shapes() {
        let a = parse_library_page(&serde_json::json!({
            "items": [{"videoId": "v1", "addedAt": "2026-01-01T00:00:00Z", "title": "T", "thumbnailUrl": "u"},
                      {"video": {"id": "v2", "title": "T2", "channel": {"name": "C"}}},
                      {"nothing": true}],
            "cursor": "c2", "hasMore": true
        }));
        assert_eq!(a.items.len(), 2);
        assert_eq!(a.items[0].added_at_ms, Some(1_767_225_600_000));
        assert_eq!(
            a.items[1].meta.as_ref().unwrap().channel_name.as_deref(),
            Some("C")
        );
        assert_eq!(a.next_cursor.as_deref(), Some("c2"));
        let b = parse_library_page(
            &serde_json::json!({"success": true, "result": {"videos": [{"id": "x"}], "nextCursor": "n", "hasMore": false}}),
        );
        assert_eq!(b.items[0].video_id, "x");
        assert_eq!(b.next_cursor, None, "hasMore=false wins over a cursor");
        let c = parse_library_page(&serde_json::json!([{"videoId": "y"}]));
        assert_eq!(c.items[0].video_id, "y");
        assert!(parse_library_page(&Value::Null).items.is_empty());
    }

    #[test]
    fn unit_conversion() {
        assert_eq!(secs_to_ms(12.3456), 12_346);
        assert_eq!(secs_to_ms(f64::NAN), 0);
        assert_eq!(secs_to_ms(-1.0), 0);
        assert_eq!(ms_to_secs(1_500), 1.5);
    }
}
