//! Personal and channel playlists for native clients.

use std::collections::HashMap;

use aun_api::playlists as core;

use crate::{ApiClient, AunError, Visibility};

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub title_localized: Option<HashMap<String, String>>,
    pub description_localized: Option<HashMap<String, String>>,
    pub thumbnail_url: Option<String>,
    pub user_id: String,
    /// Empty for a personal playlist.
    pub channel_id: String,
    pub videos: Vec<String>,
    pub visibility: Visibility,
    pub is_unlisted: bool,
}

impl From<core::Playlist> for Playlist {
    fn from(value: core::Playlist) -> Self {
        let visibility = value.visibility().into();
        let is_unlisted = value.is_unlisted();
        Self {
            id: value.id,
            title: value.title,
            description: value.description,
            title_localized: value.title_localized.map(|map| map.into_iter().collect()),
            description_localized: value.description_localized.map(|map| map.into_iter().collect()),
            thumbnail_url: value.thumbnail_url,
            user_id: value.user_id,
            channel_id: value.channel_id,
            videos: value.videos,
            visibility,
            is_unlisted,
        }
    }
}

#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct PlaylistQuery {
    #[uniffi(default = None)]
    pub limit: Option<u32>,
    #[uniffi(default = None)]
    pub cursor: Option<String>,
    /// Only return playlists that belong to this channel.
    #[uniffi(default = None)]
    pub channel_id: Option<String>,
    /// List the caller's personal playlists, including private and unlisted ones.
    #[uniffi(default = false)]
    pub mine: bool,
    /// Name search for personal or channel playlists.
    #[uniffi(default = None)]
    pub q: Option<String>,
    #[uniffi(default = None)]
    pub active_channel_id: Option<String>,
}

impl From<PlaylistQuery> for core::PlaylistQuery {
    fn from(value: PlaylistQuery) -> Self {
        Self {
            limit: value.limit,
            cursor: value.cursor,
            channel_id: value.channel_id,
            mine: value.mine,
            q: value.q,
            active_channel_id: value.active_channel_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PlaylistPage {
    pub data: Vec<Playlist>,
    /// Empty after the last page.
    pub cursor: String,
}

/// Metadata to set on create or update. Updates leave omitted properties alone.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct PlaylistMetadata {
    #[uniffi(default = None)]
    pub title: Option<String>,
    #[uniffi(default = None)]
    pub description: Option<String>,
    #[uniffi(default = None)]
    pub title_localized: Option<HashMap<String, String>>,
    #[uniffi(default = None)]
    pub description_localized: Option<HashMap<String, String>>,
    #[uniffi(default = None)]
    pub thumbnail_url: Option<String>,
    #[uniffi(default = None)]
    pub visibility: Option<Visibility>,
}

impl From<PlaylistMetadata> for core::PlaylistMetadata {
    fn from(value: PlaylistMetadata) -> Self {
        Self {
            title: value.title,
            description: value.description,
            title_localized: value.title_localized.map(|map| map.into_iter().collect()),
            description_localized: value.description_localized.map(|map| map.into_iter().collect()),
            thumbnail_url: value.thumbnail_url,
            visibility: value.visibility.map(Into::into),
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl ApiClient {
    pub async fn playlists_list(&self, query: PlaylistQuery) -> Result<PlaylistPage, AunError> {
        let page = self.inner.playlists_list(query.into()).await?;
        Ok(PlaylistPage {
            data: page.data.into_iter().map(Into::into).collect(),
            cursor: page.cursor,
        })
    }

    /// Gets a playlist by ID, including an unlisted playlist shared by its link.
    pub async fn playlist_fetch(&self, id: String, channel_id: Option<String>) -> Result<Playlist, AunError> {
        Ok(self.inner.playlist_fetch(id, channel_id).await?.into())
    }

    /// Omit `channel_id` to create a personal playlist.
    pub async fn playlist_create(
        &self,
        channel_id: Option<String>,
        metadata: PlaylistMetadata,
    ) -> Result<Playlist, AunError> {
        Ok(self
            .inner
            .playlist_create(channel_id, metadata.into())
            .await?
            .into())
    }

    /// Supply the active channel when editing as a channel manager.
    pub async fn playlist_update(
        &self,
        id: String,
        channel_id: Option<String>,
        metadata: PlaylistMetadata,
    ) -> Result<Playlist, AunError> {
        Ok(self
            .inner
            .playlist_update(id, channel_id, metadata.into())
            .await?
            .into())
    }

    pub async fn playlist_delete(&self, id: String, channel_id: Option<String>) -> Result<bool, AunError> {
        Ok(self.inner.playlist_delete(id, channel_id).await?)
    }

    pub async fn playlist_add_video(
        &self,
        id: String,
        video_id: String,
        channel_id: Option<String>,
    ) -> Result<Playlist, AunError> {
        Ok(self
            .inner
            .playlist_add_video(id, video_id, channel_id)
            .await?
            .into())
    }

    pub async fn playlist_remove_video(
        &self,
        id: String,
        video_id: String,
        channel_id: Option<String>,
    ) -> Result<Playlist, AunError> {
        Ok(self
            .inner
            .playlist_remove_video(id, video_id, channel_id)
            .await?
            .into())
    }
}
