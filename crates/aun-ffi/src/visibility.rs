//! Typed video visibility for native clients.

use aun_api::{
    AuthMode, GetVideoFetchParams, PostVideoBulkUpdateBody, PostVideoBulkUpdateBodyChanges,
    PostVideoBulkUpdateParams, PutVideoUpdateBody, PutVideoUpdateParams, requests,
};

use crate::{ApiClient, AunError};

/// Whether a video or playlist is listed, opens by link, or is private.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Visibility {
    Public,
    Unlisted,
    Private,
}

impl From<Visibility> for aun_api::Visibility {
    fn from(value: Visibility) -> Self {
        match value {
            Visibility::Public => Self::Public,
            Visibility::Unlisted => Self::Unlisted,
            Visibility::Private => Self::Private,
        }
    }
}

impl From<aun_api::Visibility> for Visibility {
    fn from(value: aun_api::Visibility) -> Self {
        match value {
            aun_api::Visibility::Public => Self::Public,
            aun_api::Visibility::Unlisted => Self::Unlisted,
            aun_api::Visibility::Private => Self::Private,
        }
    }
}

impl Visibility {
    pub(crate) fn wire_value(self) -> String {
        aun_api::Visibility::from(self).as_str().into()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VideoVisibilityUpdate {
    pub video_id: String,
    pub success: bool,
    pub changed: Option<bool>,
    pub replayed: Option<bool>,
    pub code: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BulkVideoVisibilityUpdate {
    pub success: bool,
    pub operation_id: Option<String>,
    pub results: Vec<VideoVisibilityUpdate>,
}

#[uniffi::export(async_runtime = "tokio")]
impl ApiClient {
    /// Reads a video's visibility by ID, including when opened directly by its link.
    pub async fn video_visibility(
        &self,
        id: String,
        channel_id: Option<String>,
    ) -> Result<Visibility, AunError> {
        let video = self
            .inner
            .get_video_fetch(&GetVideoFetchParams {
                id,
                header_channel_id: channel_id,
                ..Default::default()
            })
            .await?;
        Ok(video.visibility().into())
    }

    /// Changes visibility without editing the video's other metadata.
    pub async fn video_update_visibility(
        &self,
        id: String,
        channel_id: Option<String>,
        visibility: Visibility,
    ) -> Result<Visibility, AunError> {
        let response = self
            .inner
            .put_video_update(
                &PutVideoUpdateParams {
                    id,
                    header_channel_id: channel_id,
                },
                &PutVideoUpdateBody {
                    visibility: Some(visibility.wire_value()),
                    ..Default::default()
                },
            )
            .await?;
        Ok(response.result.video.visibility().into())
    }

    /// Changes visibility for multiple videos, returning each video's outcome.
    pub async fn videos_bulk_update_visibility(
        &self,
        ids: Vec<String>,
        channel_id: Option<String>,
        visibility: Visibility,
        operation_id: Option<String>,
    ) -> Result<BulkVideoVisibilityUpdate, AunError> {
        let mut request = requests::post_video_bulk_update(
            &PostVideoBulkUpdateParams {
                header_channel_id: channel_id,
            },
            &PostVideoBulkUpdateBody {
                ids: Some(ids),
                operation_id,
                changes: Some(PostVideoBulkUpdateBodyChanges {
                    visibility: Some(visibility.wire_value()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )?;
        request.auth = AuthMode::Required;
        let response: aun_api::PostVideoBulkUpdateResponse = self.inner.execute_json(request).await?;
        Ok(BulkVideoVisibilityUpdate {
            success: response.success,
            operation_id: response.operation_id,
            results: response
                .results
                .into_iter()
                .map(|result| VideoVisibilityUpdate {
                    video_id: result.video_id,
                    success: result.success,
                    changed: result.changed,
                    replayed: result.replayed,
                    code: result.code,
                    error: result.error,
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uniffi::{Lift, Lower};

    #[test]
    fn visibility_round_trips_through_uniffi_and_core() {
        for value in [Visibility::Public, Visibility::Unlisted, Visibility::Private] {
            let mut wire = Vec::new();
            <Visibility as Lower<crate::UniFfiTag>>::write(value, &mut wire);
            let decoded = <Visibility as Lift<crate::UniFfiTag>>::try_read(&mut wire.as_slice()).ok();
            assert_eq!(decoded, Some(value));
            assert_eq!(Visibility::from(aun_api::Visibility::from(value)), value);
        }
    }
}
