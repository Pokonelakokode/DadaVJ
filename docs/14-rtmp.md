# 14 — RTMP streamelés (a transport)

*Dátum: 2026-10-14*

## Architektúra

Új crate: dada-stream. Két réteg:

```
flv.rs    H.264 sample -> FLV videó-tag törzs
          [0x17|0x27] frame típus + AVC
          [0x00|0x01] AVCPacketType (sequence header | NALU)
          [3 bájt]    composition time
          [törzs]     avcC vagy length-prefixed NALU-k

rtmp.rs   RtmpPublisher
          - saját TCP + RTMP kézfogás (rml_rtmp handshake)
          - rml_rtmp kliens session: connect -> createStream -> publish
          - a hálózat KÜLÖN SZÁLON fut
          - korlátos csatorna (8 frame); ha tele van, a frame ELDOBÓDIK
```

Az élő adásnál a frame-eldobás a helyes viselkedés: jobb egy kimaradt frame,
mint egy elakadó render loop.

## Az URL

```
rtmp://a.rtmp.youtube.com/live2/STREAM-KULCS
       \_____ hoszt ____/ \app/ \___ kulcs ___/
```

A YouTube így adja a stream kulcsot; a program az app nevét és a kulcsot külön
kezeli (createStream + publish).

## Igazolt működés

Integrációs teszt (dada-stream/tests/rtmp_roundtrip.rs): egy **helyi RTMP
szerver** (rml_rtmp ServerSession) fogadja a publikálót ugyanabban a
folyamatban. A teszt végigmegy a teljes láncon:

- TCP kapcsolat + RTMP kézfogás (mindkét oldal),
- connect -> ConnectionRequested -> accept,
- createStream + publish -> PublishStreamRequested -> accept,
- a videó-tagek megérkezése.

```
$ cargo test -p dada-stream
test flv::tests::sequence_header_layout ... ok
test flv::tests::frame_layout_keyframe_and_inter ... ok
test flv::tests::composition_time_is_three_bytes ... ok
test rtmp::tests::url_parsing ... ok
test rtmp::tests::url_errors ... ok
test publishes_video_to_a_local_server ... ok
  (a szerver 5 videó-csomagot kapott, bájtpontosan egyező tartalommal)
```

A teszt ellenőrzi:
- az első csomag a **sequence header** (0x17, AVCPacketType 0, avcC-vel és SPS-sel),
- a második **keyframe NALU** (0x17, AVCPacketType 1, a NALU-k változatlanul),
- a többi **interframe** (0x27),
- a küldött frame-ek száma pontosan 5.

Vagyis az RTMP transport és az FLV-csomagolás **bizonyítottan helyes**.

## Ami még hátra van az élő adáshoz

A transport kész, de a valódi H.264 sample-eket még elő kell állítani. Két út:

| Út | Állapot |
|---|---|
| AVAssetWriter (a felvételnél már megy) | nem adja ki az encoded sample-eket |
| **VTCompressionSession** (raw FFI) | ez kell: a callback adja a CMSampleBuffer-t |

Ehhez saját FFI kell, mert:
- nincs objc2-video-toolbox crate (csak audio-toolbox és media-toolbox),
- az objc2-core-media 0.3-ban nincs benne a CMSampleBufferGetDataBuffer.

A megvalósítandó lánc:
```
CVPixelBuffer (a felvételnél már bevált úton)
  -> VTCompressionSessionEncodeFrame
    -> callback: CMSampleBuffer
      -> CMBlockBuffer -> a length-prefixed NALU-k
      -> CMVideoFormatDescriptionGetH264ParameterSetAtIndex (SPS/PPS)
      -> flv::avc_sequence_header / flv::avc_frame
      -> RtmpPublisher::send
```

## Reprodukálás

```bash
source scripts/env.sh
cargo test -p dada-stream
```
