//! Playlist operations shared by viewer and Studio clients.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{
    ApiClient, AuthMode, DeletePlaylistDeleteParams, DeletePlaylistDeleteResponse,
    DeletePlaylistRemoveVideoParams, GetPlaylistFetchParams, GetPlaylistsListParams, PatchPlaylistUpdateBody,
    PatchPlaylistUpdateParams, PostPlaylistAddVideoBody, PostPlaylistAddVideoParams, PostPlaylistCreateBody,
    PostPlaylistCreateParams, RequestSpec, Result, Visibility, requests,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub title_localized: Option<BTreeMap<String, String>>,
    pub description_localized: Option<BTreeMap<String, String>>,
    pub thumbnail_url: Option<String>,
    pub user_id: String,
    /// Empty for a personal playlist.
    pub channel_id: String,
    #[serde(default)]
    pub videos: Vec<String>,
    is_public: Option<bool>,
    is_unlisted: Option<bool>,
    visibility: Option<String>,
}

impl Playlist {
    pub fn visibility(&self) -> Visibility {
        Visibility::from_wire(self.visibility.as_deref(), self.is_public, self.is_unlisted)
    }

    pub fn is_unlisted(&self) -> bool {
        self.visibility() == Visibility::Unlisted
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaylistQuery {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    /// Only return playlists that belong to this channel.
    pub channel_id: Option<String>,
    /// List the signed-in user's personal playlists, including private and unlisted ones.
    pub mine: bool,
    /// Name search for personal or channel playlists.
    pub q: Option<String>,
    /// The caller's active channel, if present.
    pub active_channel_id: Option<String>,
}

/// Metadata to set on create or update. Updates leave omitted properties alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaylistMetadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub title_localized: Option<BTreeMap<String, String>>,
    pub description_localized: Option<BTreeMap<String, String>>,
    pub thumbnail_url: Option<String>,
    pub visibility: Option<Visibility>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PlaylistPage {
    pub data: Vec<Playlist>,
    /// Empty after the last page.
    pub cursor: String,
}

#[derive(Deserialize)]
struct PlaylistWriteResponse {
    result: PlaylistWriteResult,
}

#[derive(Deserialize)]
struct PlaylistWriteResult {
    playlist: Playlist,
}

impl ApiClient {
    pub async fn playlists_list(&self, query: PlaylistQuery) -> Result<PlaylistPage> {
        let mut request = requests::get_playlists_list(&GetPlaylistsListParams {
            limit: query.limit.map(f64::from),
            cursor: query.cursor,
            channel_id: query.channel_id,
            mine: query.mine.then(|| "true".into()),
            q: query.q,
            header_channel_id: query.active_channel_id,
        })?;
        if query.mine {
            request.auth = AuthMode::Required;
        }
        self.execute_json(request).await
    }

    /// Gets a playlist by ID, including an unlisted playlist shared by its link.
    pub async fn playlist_fetch(&self, id: String, channel_id: Option<String>) -> Result<Playlist> {
        self.execute_json(requests::get_playlist_fetch(&GetPlaylistFetchParams {
            id,
            include_videos: None,
            header_channel_id: channel_id,
        })?)
        .await
    }

    /// Omit `channel_id` to create a personal playlist.
    pub async fn playlist_create(
        &self,
        channel_id: Option<String>,
        metadata: PlaylistMetadata,
    ) -> Result<Playlist> {
        let request = requests::post_playlist_create(
            &PostPlaylistCreateParams {
                header_channel_id: channel_id,
            },
            &PostPlaylistCreateBody {
                title: metadata.title,
                description: metadata.description,
                title_localized: metadata.title_localized,
                description_localized: metadata.description_localized,
                thumbnail_url: metadata.thumbnail_url,
                visibility: metadata.visibility.map(|value| value.as_str().into()),
                ..Default::default()
            },
        )?;
        self.execute_playlist_write(request).await
    }

    pub async fn playlist_update(
        &self,
        id: String,
        channel_id: Option<String>,
        metadata: PlaylistMetadata,
    ) -> Result<Playlist> {
        let request = requests::patch_playlist_update(
            &PatchPlaylistUpdateParams {
                id,
                header_channel_id: channel_id,
            },
            &PatchPlaylistUpdateBody {
                title: metadata.title,
                description: metadata.description,
                title_localized: metadata.title_localized,
                description_localized: metadata.description_localized,
                thumbnail_url: metadata.thumbnail_url,
                visibility: metadata.visibility.map(|value| value.as_str().into()),
                ..Default::default()
            },
        )?;
        self.execute_playlist_write(request).await
    }

    pub async fn playlist_delete(&self, id: String, channel_id: Option<String>) -> Result<bool> {
        let mut request = requests::delete_playlist_delete(&DeletePlaylistDeleteParams {
            id,
            header_channel_id: channel_id,
        })?;
        request.auth = AuthMode::Required;
        let response: DeletePlaylistDeleteResponse = self.execute_json(request).await?;
        Ok(response.success)
    }

    pub async fn playlist_add_video(
        &self,
        id: String,
        video_id: String,
        channel_id: Option<String>,
    ) -> Result<Playlist> {
        let request = requests::post_playlist_add_video(
            &PostPlaylistAddVideoParams {
                id,
                header_channel_id: channel_id,
            },
            &PostPlaylistAddVideoBody { video_id },
        )?;
        self.execute_playlist_write(request).await
    }

    pub async fn playlist_remove_video(
        &self,
        id: String,
        video_id: String,
        channel_id: Option<String>,
    ) -> Result<Playlist> {
        let request = requests::delete_playlist_remove_video(&DeletePlaylistRemoveVideoParams {
            id,
            video_id,
            header_channel_id: channel_id,
        })?;
        self.execute_playlist_write(request).await
    }

    async fn execute_playlist_write(&self, mut request: RequestSpec) -> Result<Playlist> {
        request.auth = AuthMode::Required;
        let response: PlaylistWriteResponse = self.execute_json(request).await?;
        Ok(response.result.playlist)
    }
}
