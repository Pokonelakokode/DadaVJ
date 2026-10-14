//! FLV (Flash Video) csomagolás az RTMP-hez.
//!
//! Az RTMP-n a videó a FLV videó-tag törzsével megy:
//!
//!   [0x17 | 0x27]  frame típus (1 = keyframe) + kodek (7 = AVC)
//!   [0x00 | 0x01]  AVCPacketType (0 = sequence header, 1 = NALU)
//!   [3 bájt]       composition time (B-frame késleltetés, nálunk 0)
//!   [törzs]        avcC (sequence header) vagy length-prefixed NALU-k

/// Az FLV videó-tag típusa (RTMP message type).
pub const VIDEO_TAG: u8 = 0x09;

/// AVC sequence header (SPS/PPS) FLV videó-törzse.
pub fn avc_sequence_header(avcc: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + avcc.len());
    out.push(0x17); // keyframe + AVC
    out.push(0x00); // AVCPacketType: sequence header
    out.extend_from_slice(&[0, 0, 0]); // composition time
    out.extend_from_slice(avcc);
    out
}

/// Egy AVC frame FLV videó-törzse (a NALU-k már length-prefixedek).
pub fn avc_frame(nals: &[u8], keyframe: bool, composition_ms: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + nals.len());
    out.push(if keyframe { 0x17 } else { 0x27 });
    out.push(0x01); // AVCPacketType: NALU
    let ct = composition_ms.max(0) as u32;
    out.extend_from_slice(&[(ct >> 16) as u8, (ct >> 8) as u8, ct as u8]);
    out.extend_from_slice(nals);
    out
}

/// Az FLV audio-tag típusa (RTMP message type).
pub const AUDIO_TAG: u8 = 0x08;

/// AAC sequence header (AudioSpecificConfig) FLV audio-törzse.
///
/// 0xAF = hangformátum 10 (AAC), 44 kHz, 16 bit, sztereó;
/// 0x00 = AAC sequence header. (Az igazi mintavételi rátát az
/// AudioSpecificConfig hordozza, ezért az FLV mező mindig 44 kHz.)
pub fn aac_sequence_header(config: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + config.len());
    out.push(0xAF);
    out.push(0x00);
    out.extend_from_slice(config);
    out
}

/// Egy AAC frame FLV audio-törzse (nyers AAC packet).
pub fn aac_frame(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + data.len());
    out.push(0xAF);
    out.push(0x01);
    out.extend_from_slice(data);
    out
}

/// AVCDecoderConfigurationRecord (az avcC doboz) SPS-ből és PPS-ből.
///
/// Ez a H.264 konfigurációs rekord, amit a lejátszó a dekóder inicializálásához
/// kap (a sequence headerben).
pub fn avcc_from_sps_pps(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(11 + sps.len() + pps.len());
    out.push(0x01); // configurationVersion
    out.push(sps.first().copied().unwrap_or(0x64)); // AVCProfileIndication
    out.push(sps.get(1).copied().unwrap_or(0x00)); // profile_compatibility
    out.push(sps.get(2).copied().unwrap_or(0x28)); // AVCLevelIndication
    out.push(0xFF); // 6 bit reserved + lengthSizeMinusOne (3 = 4 bájtos hossz)
    out.push(0xE1); // 3 bit reserved + numOfSequenceParameterSets (1)
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1); // numOfPictureParameterSets
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_header_layout() {
        let avcc = avcc_from_sps_pps(&[0x67, 0x64, 0x28, 0xAA], &[0x68, 0xBB]);
        let tag = avc_sequence_header(&avcc);
        assert_eq!(tag[0], 0x17);
        assert_eq!(tag[1], 0x00);
        assert_eq!(&tag[2..5], &[0, 0, 0]);
        assert_eq!(tag[5], 0x01); // configurationVersion
        assert_eq!(tag[6], 0x67); // profile
        assert_eq!(tag[10], 0xE1);
        assert_eq!(&tag[11..13], &[0x00, 0x04]); // SPS hossz
        assert_eq!(&tag[13..17], &[0x67, 0x64, 0x28, 0xAA]);
        assert_eq!(tag[17], 1); // PPS db
    }

    #[test]
    fn frame_layout_keyframe_and_inter() {
        let nals = [0, 0, 0, 2, 0x65, 0x88];
        let k = avc_frame(&nals, true, 0);
        assert_eq!(k[0], 0x17);
        assert_eq!(k[1], 0x01);
        assert_eq!(&k[2..5], &[0, 0, 0]);
        assert_eq!(&k[5..], &nals);

        let i = avc_frame(&nals, false, 0);
        assert_eq!(i[0], 0x27);
    }

    #[test]
    fn aac_tags_layout() {
        let asc = [0x11u8, 0x90];
        let h = aac_sequence_header(&asc);
        assert_eq!(h, vec![0xAF, 0x00, 0x11, 0x90]);
        let f = aac_frame(&[0x21, 0x10, 0x05]);
        assert_eq!(f, vec![0xAF, 0x01, 0x21, 0x10, 0x05]);
    }

    #[test]
    fn composition_time_is_three_bytes() {
        let t = avc_frame(&[], false, 0x010203);
        assert_eq!(&t[2..5], &[0x01, 0x02, 0x03]);
    }
}
