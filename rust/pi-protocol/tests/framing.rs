//! Port of `packages/protocol/test/framing.test.ts`.

use pi_protocol::framing::{
    assert_complete_frame, encode_frame, FrameDecoder, FrameError, DEFAULT_MAX_FRAME_LENGTH,
};

fn concatenate(chunks: &[&[u8]]) -> Vec<u8> {
    let length: usize = chunks.iter().map(|c| c.len()).sum();
    let mut result = Vec::with_capacity(length);
    for chunk in chunks {
        result.extend_from_slice(chunk);
    }
    result
}

#[test]
fn prefixes_payloads_with_a_four_byte_big_endian_length() {
    assert_eq!(encode_frame(&[0xaa, 0xbb, 0xcc]).unwrap(), vec![0x00, 0x00, 0x00, 0x03, 0xaa, 0xbb, 0xcc]);
    assert_eq!(encode_frame(&[]).unwrap(), vec![0, 0, 0, 0]);
}

#[test]
fn validates_one_complete_bounded_frame_without_accepting_trailing_or_partial_bytes() {
    assert!(assert_complete_frame(&[0, 0, 0, 2, 1, 2], Some(2)).is_ok());
    let error = assert_complete_frame(&[0, 0, 0, 2, 1], None).unwrap_err();
    assert!(error.0.to_lowercase().contains("complete"), "{error:?}");
    let error = assert_complete_frame(&[0, 0, 0, 1, 1, 2], None).unwrap_err();
    assert!(error.0.to_lowercase().contains("exactly"), "{error:?}");
    let error = assert_complete_frame(&[0, 0, 0, 3, 1, 2, 3], Some(2)).unwrap_err();
    assert!(error.0.to_lowercase().contains("limit"), "{error:?}");
}

#[test]
fn decodes_fragmented_coalesced_and_empty_frames_in_order() {
    let wire = concatenate(&[
        &encode_frame(&[1, 2, 3]).unwrap(),
        &encode_frame(&[]).unwrap(),
        &encode_frame(&[4]).unwrap(),
    ]);
    let mut decoder = FrameDecoder::new();
    let mut frames: Vec<Vec<u8>> = Vec::new();
    for byte in &wire {
        frames.extend(decoder.push(&[*byte]).unwrap());
    }
    decoder.end().unwrap();
    assert_eq!(frames, vec![vec![1, 2, 3], vec![], vec![4]]);

    let mut coalesced = FrameDecoder::new();
    assert_eq!(coalesced.push(&wire).unwrap(), frames);
    coalesced.end().unwrap();
}

#[test]
fn assembles_payloads_spanning_multiple_internal_blocks() {
    let payload: Vec<u8> = (0..70_000).map(|i| (i % 251) as u8).collect();
    let wire = encode_frame(&payload).unwrap();
    let mut decoder = FrameDecoder::new();
    let frames = [
        decoder.push(&wire[..101]).unwrap(),
        decoder.push(&wire[101..65_541]).unwrap(),
        decoder.push(&wire[65_541..]).unwrap(),
    ]
    .concat();
    decoder.end().unwrap();
    assert_eq!(frames, vec![payload]);
}

#[test]
fn handles_every_split_point_across_a_frame() {
    let wire = encode_frame(&[10, 20, 30, 40]).unwrap();
    for split in 0..=wire.len() {
        let mut decoder = FrameDecoder::new();
        let frames = [
            decoder.push(&wire[..split]).unwrap(),
            decoder.push(&wire[split..]).unwrap(),
        ]
        .concat();
        decoder.end().unwrap();
        assert_eq!(frames, vec![vec![10, 20, 30, 40]]);
    }
}

#[test]
fn copies_payload_bytes_instead_of_retaining_or_aliasing_input_chunks() {
    let mut chunk = encode_frame(&[1, 2, 3]).unwrap();
    let mut decoder = FrameDecoder::new();
    let frames = decoder.push(&chunk).unwrap();
    chunk.fill(9);
    assert_eq!(frames, vec![vec![1, 2, 3]]);
}

#[test]
fn accepts_empty_chunks_and_a_clean_empty_stream() {
    let mut decoder = FrameDecoder::new();
    assert_eq!(decoder.push(&[]).unwrap(), Vec::<Vec<u8>>::new());
    decoder.end().unwrap();
}

#[test]
fn rejects_a_truncated_stream_at_end() {
    for wire in [vec![0, 0, 0], vec![0, 0, 0, 2, 1]] {
        let mut decoder = FrameDecoder::new();
        assert_eq!(decoder.push(&wire).unwrap(), Vec::<Vec<u8>>::new());
        let error = decoder.end().unwrap_err();
        assert!(matches!(error, FrameError(_)));
    }
}

#[test]
fn rejects_an_oversized_declared_length_as_soon_as_its_header_is_complete() {
    let mut decoder = FrameDecoder::with_max_frame_length(3).unwrap();
    let error = decoder.push(&[0, 0, 0, 4]).unwrap_err();
    assert!(error.0.to_lowercase().contains("limit"), "{error:?}");
    let error = decoder.push(&[1]).unwrap_err();
    assert!(error.0.to_lowercase().contains("failed"), "{error:?}");
}

#[test]
fn accepts_a_frame_exactly_at_the_configured_maximum() {
    let mut decoder = FrameDecoder::with_max_frame_length(3).unwrap();
    assert_eq!(decoder.push(&encode_frame(&[1, 2, 3]).unwrap()).unwrap(), vec![vec![1, 2, 3]]);
    decoder.end().unwrap();
}

#[test]
fn cannot_be_pushed_after_end() {
    let mut decoder = FrameDecoder::new();
    decoder.end().unwrap();
    let error = decoder.push(&[]).unwrap_err();
    assert!(error.0.to_lowercase().contains("ended"), "{error:?}");
    let error = decoder.end().unwrap_err();
    assert!(error.0.to_lowercase().contains("ended"), "{error:?}");
}

#[test]
fn rejects_invalid_maximum_frame_length() {
    // JS also rejects -1, 1.5 and NaN; u64 rules those out at compile time.
    let error = FrameDecoder::with_max_frame_length(DEFAULT_MAX_FRAME_LENGTH * 1_000)
        .expect_err("invalid max frame length");
    assert!(matches!(error, pi_protocol::cbor::RangeError(_)));
}
