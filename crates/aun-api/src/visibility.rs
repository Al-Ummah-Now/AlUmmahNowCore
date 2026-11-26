//! Visibility shared by video and playlist writes.

use serde::{Deserialize, Serialize};

/// Whether an item is listed, link-only, or restricted to its owner and managers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Unlisted,
    Private,
}

impl Visibility {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Unlisted => "unlisted",
            Self::Private => "private",
        }
    }

    /// Resolve a response's explicit visibility or its legacy flags.
    /// Older public items can omit `isPublic`; only a non-public item can be unlisted.
    pub fn from_wire(visibility: Option<&str>, is_public: Option<bool>, is_unlisted: Option<bool>) -> Self {
        match (visibility, is_public, is_unlisted) {
            (Some("public"), _, _) => Self::Public,
            (Some("unlisted"), _, _) => Self::Unlisted,
            (Some("private"), _, _) => Self::Private,
            (_, Some(false), Some(true)) => Self::Unlisted,
            (_, Some(false), _) => Self::Private,
            _ => Self::Public,
        }
    }
}

impl crate::GetVideoFetchResponse {
    pub fn visibility(&self) -> Visibility {
        Visibility::from_wire(None, Some(self.is_public), self.is_unlisted)
    }
}

impl crate::GetVideosListResponseDataItem {
    pub fn visibility(&self) -> Visibility {
        Visibility::from_wire(None, Some(self.is_public), self.is_unlisted)
    }
}

impl crate::GetChannelVideosListResponseDataItem {
    pub fn visibility(&self) -> Visibility {
        Visibility::from_wire(None, Some(self.is_public), self.is_unlisted)
    }
}

impl crate::PutVideoUpdateResponseResultVideo {
    pub fn visibility(&self) -> Visibility {
        Visibility::from_wire(None, Some(self.is_public), self.is_unlisted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_round_trips_its_wire_values() {
        for (visibility, value) in [
            (Visibility::Public, "public"),
            (Visibility::Unlisted, "unlisted"),
            (Visibility::Private, "private"),
        ] {
            let encoded = serde_json::json!(value);
            assert_eq!(serde_json::to_value(visibility).ok(), Some(encoded.clone()));
            assert_eq!(
                serde_json::from_value::<Visibility>(encoded).ok(),
                Some(visibility)
            );
            assert_eq!(visibility.as_str(), value);
        }
    }

    #[test]
    fn batch_initiate_keeps_per_video_and_shared_visibility() {
        use crate::{
            PostUploadBatchInitiateEndpointBody, PostUploadBatchInitiateEndpointBodySharedMetadata,
            PostUploadBatchInitiateEndpointBodyVideosItem, client::Body, requests,
        };

        let request = requests::post_upload_batch_initiate_endpoint(
            &Default::default(),
            &PostUploadBatchInitiateEndpointBody {
                videos: vec![
                    PostUploadBatchInitiateEndpointBodyVideosItem {
                        visibility: Some(Visibility::Public.as_str().into()),
                        ..Default::default()
                    },
                    PostUploadBatchInitiateEndpointBodyVideosItem {
                        visibility: Some(Visibility::Unlisted.as_str().into()),
                        ..Default::default()
                    },
                    PostUploadBatchInitiateEndpointBodyVideosItem::default(),
                ],
                shared_metadata: Some(PostUploadBatchInitiateEndpointBodySharedMetadata {
                    visibility: Some(Visibility::Private.as_str().into()),
                    ..Default::default()
                }),
            },
        )
        .unwrap();
        let Body::Json(body) = request.body else {
            panic!("expected a JSON request");
        };
        assert_eq!(body["videos"][0]["visibility"], "public");
        assert_eq!(body["videos"][1]["visibility"], "unlisted");
        assert!(body["videos"][2].get("visibility").is_none());
        assert_eq!(body["sharedMetadata"]["visibility"], "private");
    }
}
