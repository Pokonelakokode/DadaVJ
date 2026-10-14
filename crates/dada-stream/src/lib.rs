//! RTMP streamelés: FLV/AVC csomagolás és publikálás.
//!
//! A H.264 sample-eket (length-prefixed NALU-k) FLV videó-taggé csomagoljuk,
//! majd az RTMP szervernek küldjük. A hálózat háttérszálon fut, hogy a render
//! loopot ne blokkolja; a lassú hálózat frame-eket dob el.

pub mod flv;
pub mod rtmp;

pub use flv::{avc_frame, avc_sequence_header, avcc_from_sps_pps, VIDEO_TAG};
pub use rtmp::{RtmpPublisher, StatsSnapshot, VideoFrame};
