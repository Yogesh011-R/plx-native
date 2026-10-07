//! **Local-file playback**: the step between the file browser's OK on a video and
//! `start_bufferfeed`.
//!
//! A server-chosen item arrives with its codecs, frame rate, raster and Dolby Vision layering
//! already described by PMS, and `route::apply_plan` installs that description as the Starfish
//! `Load` declaration. A file on a USB drive has nobody to describe it, and the declaration must
//! still be honest — a wrong codec is a refused Load or silent audio (`player/CLAUDE.md`). So the
//! file is read first ([`crate::ff::probe_file`], off the frame), [`decide`] turns what it found
//! into the declaration or into the reason the television cannot play it, and [`install`] puts the
//! declaration and a `file://` URL into the route the way `install_synthetic_playurl` does for the
//! pipeline tier's trigger. The engine sends a `file://` URL to the demuxer's file source
//! ([`crate::ff::DemuxSource::File`]).
//!
//! Nothing here is `Debug`, and nothing logs a path: a file name is the viewer's, and the event log
//! can leave the set.

use std::path::{Path, PathBuf};

/// The scheme a local file's route URL carries.
const SCHEME: &str = "file://";

/// The video codecs the Load payload can declare: `"hevc"` selects the H265 payload and `"h264"`
/// the H264 one. Anything else would be fed to a decoder configured for a different codec.
const VIDEO_CODECS: &[&str] = &["h264", "hevc"];

/// The route URL for a local file.
pub fn url_for(path: &Path) -> String {
    format!("{SCHEME}{}", path.display())
}

/// The local file a route URL names, or `None` for any other URL. Only an ABSOLUTE path counts:
/// the demuxer has no working directory to resolve a relative one against.
pub fn file_path(url: &str) -> Option<PathBuf> {
    let path = Path::new(url.strip_prefix(SCHEME)?);
    path.is_absolute().then(|| path.to_path_buf())
}

/// A local file and the Load declaration to play it with.
#[derive(Clone, PartialEq)]
pub struct LocalPlay {
    pub path: PathBuf,
    /// FFmpeg's name for the video codec: `"h264"` or `"hevc"`.
    pub vcodec: String,
    /// FFmpeg's name for the audio codec of the track to feed — the first one the payload can
    /// declare, which is also the first one `demux` matches against the declaration.
    pub acodec: String,
    /// The container's average frame rate; 0 omits it from the payload.
    pub fps: f64,
    pub dovi: plx_data::metadata::Dovi,
    /// The coded size, when the container states it.
    pub raster: Option<(u16, u16)>,
}

/// Why a local file will not be played.
#[derive(Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The bundled FFmpeg is missing, so nothing can be played.
    PlayerUnavailable,
    /// The file could not be opened, or its container is not one the player reads.
    Unreadable,
    /// The file has no video stream.
    NoVideo,
    /// The video codec, by FFmpeg name (`"mpeg4"`, `"vp9"`, …), is not one the TV decodes here.
    VideoCodec(String),
    /// The file has no audio track.
    NoAudio,
    /// None of the audio tracks is in a codec the TV decodes here; the first track's codec.
    AudioCodec(String),
    /// A Dolby Vision layering this television cannot show (`Dovi::presentation`'s refusal).
    DolbyVision(&'static str),
}

impl Refusal {
    /// The event-log form: a category and a codec name, never a file name.
    pub fn log_form(&self) -> String {
        match self {
            Refusal::PlayerUnavailable => "player unavailable".into(),
            Refusal::Unreadable => "unreadable".into(),
            Refusal::NoVideo => "no video".into(),
            Refusal::VideoCodec(c) => format!("video codec {c}"),
            Refusal::NoAudio => "no audio".into(),
            Refusal::AudioCodec(c) => format!("audio codec {c}"),
            Refusal::DolbyVision(why) => format!("dolby vision ({why})"),
        }
    }
}

/// Turn a file's streams into the declaration to play it with, or the reason it cannot be played.
/// Pure, so the host suite pins every rule. Dolby Vision's refusal depends on the television and is
/// [`probe`]'s second step, not this one's.
pub fn decide(path: &Path, streams: &crate::ff::FileStreams) -> Result<LocalPlay, Refusal> {
    let video = streams.video.as_ref().ok_or(Refusal::NoVideo)?;
    if !VIDEO_CODECS.contains(&video.codec.as_str()) {
        return Err(Refusal::VideoCodec(video.codec.clone()));
    }
    let acodec = match streams
        .audio
        .iter()
        .find(|codec| super::engine::audio_declarable(codec))
    {
        Some(codec) => codec.clone(),
        None => {
            return Err(match streams.audio.first() {
                Some(codec) => Refusal::AudioCodec(codec.clone()),
                None => Refusal::NoAudio,
            })
        }
    };
    let dovi = video.dovi.map_or(plx_data::metadata::Dovi::NONE, |d| {
        plx_data::metadata::Dovi {
            present: d.dv_profile > 0,
            profile: i64::from(d.dv_profile),
            bl_compat: i64::from(d.dv_bl_signal_compatibility_id),
            el_present: d.el_present_flag != 0,
            ..plx_data::metadata::Dovi::NONE
        }
    });
    let raster = match (u16::try_from(video.width), u16::try_from(video.height)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Some((w, h)),
        _ => None,
    };
    Ok(LocalPlay {
        path: path.to_path_buf(),
        vcodec: video.codec.clone(),
        acodec,
        fps: if video.fps.is_finite() && video.fps > 0.0 { video.fps } else { 0.0 },
        dovi,
        raster,
    })
}

/// **Read a file and decide how to play it.** Blocking (it reads the drive): call it off the frame.
pub fn probe(path: &Path) -> Result<LocalPlay, Refusal> {
    let streams = crate::ff::probe_file(path).map_err(|fail| match fail {
        crate::ff::ProbeFail::Unavailable => Refusal::PlayerUnavailable,
        crate::ff::ProbeFail::Open | crate::ff::ProbeFail::Streams => Refusal::Unreadable,
    })?;
    let play = decide(path, &streams)?;
    // The same rule `route::set_stream_declaration` enforces at install, asked here so the viewer
    // reads the reason on the browser instead of a player that never starts.
    if let Some(why) = play.dovi.presentation_now(play.vcodec == "hevc").refusal() {
        return Err(Refusal::DolbyVision(why));
    }
    Ok(play)
}

/// Install a local play in the route — its declaration, its source raster and its `file://` URL —
/// as a NEW item: the previous item's audio and subtitle choices do not carry over. `false` when
/// the declaration is refused, in which case the URL is left unset (`install_synthetic_playurl`'s
/// rule: a later PLAY reads URL presence as "already admitted").
pub fn install(ps: &mut crate::route::PlaybackSession, play: &LocalPlay) -> bool {
    if !crate::route::set_stream_declaration(
        ps,
        &play.vcodec,
        &play.acodec,
        play.fps,
        play.dovi,
        false,
    ) {
        return false;
    }
    let (w, h) = play.raster.unwrap_or((0, 0));
    crate::route::set_stream_source_raster(ps, w, h);
    super::reset_audio_track();
    super::reset_subtitle();
    crate::route::set_url(ps, &url_for(&play.path));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ff::{AVDOVIDecoderConfigurationRecord, FileStreams, VideoStream};

    fn video(codec: &str) -> Option<VideoStream> {
        Some(VideoStream {
            codec: codec.into(),
            width: 1920,
            height: 1080,
            fps: 23.976,
            dovi: None,
        })
    }

    fn streams(vcodec: &str, audio: &[&str]) -> FileStreams {
        FileStreams {
            video: video(vcodec),
            audio: audio.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn refusal(r: Result<LocalPlay, Refusal>) -> Refusal {
        match r {
            Ok(_) => panic!("expected a refusal"),
            Err(why) => why,
        }
    }

    #[test]
    fn a_file_url_round_trips_and_other_urls_are_not_files() {
        let path = Path::new("/tmp/usb/Films/A film: part 2.mkv");
        assert_eq!(file_path(&url_for(path)).as_deref(), Some(path));
        assert_eq!(file_path("http://192.0.2.10:32400/library/parts/1/file.mkv"), None);
        assert_eq!(file_path("https://example.invalid/a.mkv"), None);
        assert_eq!(file_path("file://relative/a.mkv"), None, "a relative path has no meaning here");
        assert_eq!(file_path(""), None);
    }

    #[test]
    fn an_h264_file_declares_its_codecs_rate_and_raster() {
        let play = decide(Path::new("/m/a.mkv"), &streams("h264", &["ac3"])).ok().unwrap();
        assert_eq!((play.vcodec.as_str(), play.acodec.as_str()), ("h264", "ac3"));
        assert!((play.fps - 23.976).abs() < 1e-9);
        assert_eq!(play.raster, Some((1920, 1080)));
        assert_eq!(play.dovi, plx_data::metadata::Dovi::NONE);
        assert_eq!(play.path, Path::new("/m/a.mkv"));
    }

    /// The fed track is the first one the payload can declare, which is also the one `demux`
    /// matches (`audio_stream_matching` takes the first track of the declared codec).
    #[test]
    fn the_first_declarable_audio_track_is_the_one_declared() {
        let play = decide(Path::new("/m/a.mkv"), &streams("hevc", &["truehd", "eac3", "aac"]))
            .ok()
            .unwrap();
        assert_eq!(play.acodec, "eac3");
    }

    #[test]
    fn a_video_codec_the_payload_cannot_declare_is_refused_by_name() {
        assert!(matches!(
            refusal(decide(Path::new("/m/a.avi"), &streams("mpeg4", &["mp3"]))),
            Refusal::VideoCodec(c) if c == "mpeg4"
        ));
    }

    #[test]
    fn audio_the_tv_cannot_decode_is_refused_by_the_first_tracks_codec() {
        assert!(matches!(
            refusal(decide(Path::new("/m/a.mkv"), &streams("h264", &["opus", "flac"]))),
            Refusal::AudioCodec(c) if c == "opus"
        ));
        assert!(refusal(decide(Path::new("/m/a.mkv"), &streams("h264", &[]))) == Refusal::NoAudio);
    }

    #[test]
    fn a_file_without_video_is_refused() {
        let s = FileStreams { video: None, audio: vec!["aac".into()] };
        assert!(refusal(decide(Path::new("/m/a.m4a"), &s)) == Refusal::NoVideo);
    }

    /// The configuration record maps onto the declaration's four deciding fields, and `present`
    /// follows the profile, as it does for the pipeline trigger (`PlayDovi::to_dovi`).
    #[test]
    fn a_dolby_vision_record_becomes_the_declarations_layering() {
        let mut s = streams("hevc", &["eac3"]);
        s.video.as_mut().unwrap().dovi = Some(AVDOVIDecoderConfigurationRecord {
            dv_profile: 8,
            dv_bl_signal_compatibility_id: 1,
            el_present_flag: 0,
            ..Default::default()
        });
        let dv = decide(Path::new("/m/a.mkv"), &s).ok().unwrap().dovi;
        assert!(dv.present);
        assert_eq!((dv.profile, dv.bl_compat, dv.el_present), (8, 1, false));
    }

    #[test]
    fn an_unknown_rate_or_raster_is_omitted_rather_than_invented() {
        let mut s = streams("h264", &["aac"]);
        let v = s.video.as_mut().unwrap();
        (v.fps, v.width, v.height) = (f64::NAN, 0, 0);
        let play = decide(Path::new("/m/a.mp4"), &s).ok().unwrap();
        assert_eq!(play.fps, 0.0);
        assert_eq!(play.raster, None);
    }
}
