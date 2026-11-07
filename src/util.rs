//! Small pure helpers shared by the catalog pages: duration formatting and the
//! quality badge labels.

use crate::tidal::Track;

/// `m:ss`, or `h:mm:ss` past an hour.
pub(crate) fn format_duration(seconds: u64) -> String {
    let (hours, minutes, secs) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

/// The total playing time of `tracks` as [`format_duration`], or `None` when no
/// track reports a duration.
pub(crate) fn total_duration(tracks: &[Track]) -> Option<String> {
    let known: Vec<u64> = tracks.iter().filter_map(|track| track.duration).collect();
    if known.is_empty() {
        return None;
    }
    Some(format_duration(known.iter().sum()))
}

/// The TIDAL quality badge for an `audioQuality` tag: `MAX` for hi-res lossless,
/// `HI-RES` for lossless. The lower tiers get no badge, matching TIDAL.
pub(crate) fn quality_badge(quality: Option<&str>) -> Option<&'static str> {
    match quality?.trim().to_ascii_uppercase().as_str() {
        "HI_RES_LOSSLESS" | "HI_RES" | "MQA" => Some("MAX"),
        "LOSSLESS" => Some("HI-RES"),
        _ => None,
    }
}

/// The human-readable name of an `audioQuality` tag, for the track-info dialog: `HI_RES_LOSSLESS`
/// reads as "Hi-Res Lossless". An unknown tag is shown as TIDAL spelled it, so nothing is hidden.
pub(crate) fn quality_label(quality: Option<&str>) -> Option<String> {
    let tag = quality?.trim();
    if tag.is_empty() {
        return None;
    }
    Some(match tag.to_ascii_uppercase().as_str() {
        "LOW" => "Low".to_owned(),
        "HIGH" => "High".to_owned(),
        "LOSSLESS" => "Lossless".to_owned(),
        "HI_RES_LOSSLESS" | "HI_RES" => "Hi-Res Lossless".to_owned(),
        "MQA" => "MQA".to_owned(),
        _ => tag.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_roll_over_minutes_and_hours() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(59), "0:59");
        assert_eq!(format_duration(60), "1:00");
        assert_eq!(format_duration(3599), "59:59");
        assert_eq!(format_duration(3600), "1:00:00");
        assert_eq!(format_duration(3725), "1:02:05");
    }

    #[test]
    fn total_duration_needs_at_least_one_known_track() {
        let tracks = vec![
            Track { duration: Some(60), ..Track::default() },
            Track { duration: None, ..Track::default() },
            Track { duration: Some(30), ..Track::default() },
        ];
        assert_eq!(total_duration(&tracks).as_deref(), Some("1:30"));
        assert_eq!(total_duration(&[Track::default()]), None);
        assert_eq!(total_duration(&[]), None);
    }

    #[test]
    fn only_the_lossless_tiers_get_a_badge() {
        assert_eq!(quality_badge(Some("HI_RES_LOSSLESS")), Some("MAX"));
        assert_eq!(quality_badge(Some("lossless")), Some("HI-RES"));
        assert_eq!(quality_badge(Some("HIGH")), None);
        assert_eq!(quality_badge(Some("LOW")), None);
        assert_eq!(quality_badge(None), None);
    }

    #[test]
    fn quality_labels_read_as_tidals_tiers() {
        assert_eq!(quality_label(Some("HI_RES_LOSSLESS")).as_deref(), Some("Hi-Res Lossless"));
        assert_eq!(quality_label(Some("lossless")).as_deref(), Some("Lossless"));
        assert_eq!(quality_label(Some(" HIGH ")).as_deref(), Some("High"));
        assert_eq!(quality_label(Some("DOLBY_ATMOS")).as_deref(), Some("DOLBY_ATMOS"));
        assert_eq!(quality_label(Some("  ")), None);
        assert_eq!(quality_label(None), None);
    }

}
