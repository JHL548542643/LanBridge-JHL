//! Consistency tests against `src/protocol/vectors/*`.

use bytes::BytesMut;
use lb_protocol::{
    decode_frame, encode_frame, Bye, Features, Frame, Hello, MsgType, Ping, ProbeRequest,
    PROTOCOL_VERSION,
};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Deserialize)]
struct VectorFile {
    name: String,
    frame_hex: String,
}

fn vectors_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../protocol/vectors")
}

fn load(name: &str) -> VectorFile {
    let path = vectors_dir().join(name);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("parse vector json")
}

fn hex_decode(s: &str) -> Vec<u8> {
    hex::decode(s).expect("hex")
}

#[test]
fn vector_hello_v0() {
    let v = load("hello_v0.json");
    assert_eq!(v.name, "hello_v0");
    let raw = hex_decode(&v.frame_hex);
    let (frame, n) = decode_frame(&raw).unwrap();
    assert_eq!(n, raw.len());
    assert_eq!(frame.msg_type, MsgType::Hello.as_u16());
    let hello = Hello::decode(&frame.payload).unwrap();
    assert_eq!(hello.protocol_version, PROTOCOL_VERSION);
    assert_eq!(hello.features, Features::M1_CONTROL);
    assert_eq!(hello.device_id, "win-dev");
    assert_eq!(hello.alias, "PC");
    assert_eq!(hello.fingerprint, [0xaa; 32]);

    let mut buf = BytesMut::new();
    encode_frame(
        &Frame::new(MsgType::Hello, hello.encode().unwrap()),
        &mut buf,
    )
    .unwrap();
    assert_eq!(buf.as_ref(), raw.as_slice(), "re-encode must match vector");
}

#[test]
fn vector_ping_v0() {
    let v = load("ping_v0.json");
    let raw = hex_decode(&v.frame_hex);
    let (frame, n) = decode_frame(&raw).unwrap();
    assert_eq!(n, raw.len());
    assert_eq!(frame.msg_type, MsgType::Ping.as_u16());
    let ping = Ping::decode(&frame.payload).unwrap();
    assert_eq!(ping.nonce, 0x0123_4567_89AB_CDEF);

    let mut buf = BytesMut::new();
    encode_frame(&Frame::new(MsgType::Ping, ping.encode()), &mut buf).unwrap();
    assert_eq!(buf.as_ref(), raw.as_slice());
}

#[test]
fn vector_probe_request_v0() {
    let v = load("probe_request_v0.json");
    let raw = hex_decode(&v.frame_hex);
    let (frame, n) = decode_frame(&raw).unwrap();
    assert_eq!(n, raw.len());
    assert_eq!(frame.msg_type, MsgType::ProbeRequest.as_u16());
    let req = ProbeRequest::decode(&frame.payload).unwrap();
    assert_eq!(req.byte_count, 8);
    let mut buf = BytesMut::new();
    encode_frame(
        &Frame::new(MsgType::ProbeRequest, req.encode().unwrap()),
        &mut buf,
    )
    .unwrap();
    assert_eq!(buf.as_ref(), raw.as_slice());
}

#[test]
fn vector_bye_v0() {
    let v = load("bye_v0.json");
    assert_eq!(v.name, "bye_v0");
    let raw = hex_decode(&v.frame_hex);
    let (frame, n) = decode_frame(&raw).unwrap();
    assert_eq!(n, raw.len());
    assert_eq!(frame.msg_type, MsgType::Bye.as_u16());
    assert_eq!(MsgType::from_u16(0x0030), Some(MsgType::Bye));
    let bye = Bye::decode(&frame.payload).unwrap();

    let mut buf = BytesMut::new();
    encode_frame(&Frame::new(MsgType::Bye, bye.encode()), &mut buf).unwrap();
    assert_eq!(buf.as_ref(), raw.as_slice(), "re-encode must match vector");
    assert!(
        Bye::decode(&[0u8]).is_err(),
        "Bye payload must be empty (spec)"
    );
}

#[test]
fn unknown_msg_type_is_skippable() {
    assert!(MsgType::is_skippable_unknown(0x0203));
    assert!(MsgType::from_u16(0x0203).is_none());
    assert!(MsgType::from_u16(0x0200).is_some());
}
