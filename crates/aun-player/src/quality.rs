//! Quality policy: the ABR cap for a device class, network and user settings.
//! See `docs/PLAYER_BEHAVIOUR.md` §5 for the table.

use crate::model::{DeviceClass, NetworkKind, QualityCap, QualitySettings};

/// Data saver: 480p at about 1 Mbps on every network.
pub const DATA_SAVER_MAX_HEIGHT: u32 = 480;
pub const DATA_SAVER_MAX_BITRATE_BPS: u64 = 1_000_000;
/// Cellular without data saver: 720p at about 2.5 Mbps.
pub const CELLULAR_MAX_HEIGHT: u32 = 720;
pub const CELLULAR_MAX_BITRATE_BPS: u64 = 2_500_000;

/// The largest resolution worth fetching on a device class.
pub fn device_max_height(device: DeviceClass) -> u32 {
    match device {
        DeviceClass::Phone => 1080,
        DeviceClass::Tablet => 1440,
        DeviceClass::Desktop | DeviceClass::Tv => 2160,
        DeviceClass::Car => 480,
    }
}

/// Combines the three limits; the smallest wins. `Offline` and `Unknown` impose no network cap
/// (offline playback uses the downloaded rendition anyway).
pub fn quality_cap(device: DeviceClass, network: NetworkKind, settings: QualitySettings) -> QualityCap {
    let mut height = device_max_height(device);
    let mut bitrate = None;
    if settings.data_saver {
        height = height.min(DATA_SAVER_MAX_HEIGHT);
        bitrate = Some(DATA_SAVER_MAX_BITRATE_BPS);
    } else if network == NetworkKind::Cellular {
        height = height.min(CELLULAR_MAX_HEIGHT);
        bitrate = Some(CELLULAR_MAX_BITRATE_BPS);
    }
    if let Some(user) = settings.user_max_height {
        height = height.min(user.max(1));
    }
    QualityCap {
        max_height: Some(height),
        max_bitrate_bps: bitrate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_limits() {
        let s = QualitySettings::default();
        assert_eq!(
            quality_cap(DeviceClass::Tv, NetworkKind::Wifi, s).max_height,
            Some(2160)
        );
        assert_eq!(
            quality_cap(DeviceClass::Phone, NetworkKind::Wifi, s).max_height,
            Some(1080)
        );
        assert_eq!(
            quality_cap(DeviceClass::Phone, NetworkKind::Wifi, s).max_bitrate_bps,
            None
        );
    }

    #[test]
    fn cellular_and_data_saver() {
        let cell = quality_cap(
            DeviceClass::Tablet,
            NetworkKind::Cellular,
            QualitySettings::default(),
        );
        assert_eq!(cell.max_height, Some(720));
        assert_eq!(cell.max_bitrate_bps, Some(CELLULAR_MAX_BITRATE_BPS));
        let saver = QualitySettings {
            data_saver: true,
            user_max_height: None,
        };
        let wifi = quality_cap(DeviceClass::Tv, NetworkKind::Wifi, saver);
        assert_eq!(wifi.max_height, Some(480));
        assert_eq!(wifi.max_bitrate_bps, Some(DATA_SAVER_MAX_BITRATE_BPS));
    }

    #[test]
    fn user_choice_only_lowers() {
        let s = QualitySettings {
            data_saver: false,
            user_max_height: Some(4320),
        };
        assert_eq!(
            quality_cap(DeviceClass::Phone, NetworkKind::Wifi, s).max_height,
            Some(1080)
        );
        let s = QualitySettings {
            data_saver: false,
            user_max_height: Some(360),
        };
        assert_eq!(
            quality_cap(DeviceClass::Tv, NetworkKind::Cellular, s).max_height,
            Some(360)
        );
    }
}
