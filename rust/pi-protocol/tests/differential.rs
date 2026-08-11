//! Byte-level differential test against the JS implementation.
//!
//! Reads wire bytes produced by the real Node implementation
//! (`scripts/generate-protocol-fixtures.mjs`) and verifies the Rust port
//! decodes them and re-encodes them byte-identically:
//!
//! 1. `decode_cbor` of the raw wire bytes succeeds;
//! 2. parsing the decoded value into a protocol model succeeds;
//! 3. re-encoding the model (`to_value` + `encode_cbor`) reproduces the
//!    exact original bytes (proves Rust field ordering and encoding rules
//!    match the JS implementation);
//! 4. re-encoding the raw decoded CBOR value also reproduces the original
//!    bytes (proves the CBOR layer is a faithful round trip).

use pi_protocol::cbor::{decode_cbor, encode_cbor, CborOptions};
use pi_protocol::{parse_client_message, parse_server_message};

const FIXTURES: &str = include_str!("fixtures.tsv");

fn from_hex(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "hex fixture must contain whole bytes: {hex:?}");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex byte"))
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn fixtures_are_present() {
    // include_str! already fails to compile when the file is missing, but the
    // panic below gives the actionable hint about regenerating it.
    assert!(
        !FIXTURES.is_empty(),
        "fixtures.tsv is empty; run `node scripts/generate-protocol-fixtures.mjs` from the repo root"
    );
}

#[test]
fn rust_port_is_byte_identical_to_js_wire_fixtures() {
    assert!(!FIXTURES.is_empty(), "no fixtures to check");
    let mut checked = 0usize;
    for (line_number, line) in FIXTURES.lines().enumerate() {
        let line_number = line_number + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((kind, hex)) = line.split_once('\t') else {
            panic!("fixtures.tsv:{line_number}: malformed line (expected `client|server<TAB>hex`): {line:?}");
        };
        assert!(
            kind == "client" || kind == "server",
            "fixtures.tsv:{line_number}: unknown direction {kind:?}"
        );
        let wire = from_hex(hex);

        // 1. CBOR decode must succeed.
        let decoded = decode_cbor(&wire, &CborOptions::default())
            .unwrap_or_else(|e| panic!("fixtures.tsv:{line_number}: CBOR decode failed: {e}"));

        // 2. Protocol parse must succeed.
        match kind {
            "client" => {
                let message = parse_client_message(&decoded)
                    .unwrap_or_else(|e| panic!("fixtures.tsv:{line_number}: client parse failed: {e}"));
                // 3. Model re-encode must reproduce the original bytes.
                let re_encoded = encode_cbor(&message.to_value(), &CborOptions::default())
                    .expect("re-encoding a parsed client message cannot fail");
                assert_eq!(
                    to_hex(&re_encoded),
                    hex,
                    "fixtures.tsv:{line_number}: client model re-encode differs from JS wire bytes"
                );
            }
            "server" => {
                let message = parse_server_message(&decoded)
                    .unwrap_or_else(|e| panic!("fixtures.tsv:{line_number}: server parse failed: {e}"));
                let re_encoded = encode_cbor(&message.to_value(), &CborOptions::default())
                    .expect("re-encoding a parsed server message cannot fail");
                assert_eq!(
                    to_hex(&re_encoded),
                    hex,
                    "fixtures.tsv:{line_number}: server model re-encode differs from JS wire bytes"
                );
            }
            _ => unreachable!("direction validated above"),
        }

        // 4. Raw CBOR value round trip must reproduce the original bytes.
        let raw_re_encoded = encode_cbor(&decoded, &CborOptions::default())
            .expect("re-encoding a decoded CBOR value cannot fail");
        assert_eq!(
            to_hex(&raw_re_encoded),
            hex,
            "fixtures.tsv:{line_number}: raw CBOR re-encode differs from JS wire bytes"
        );

        checked += 1;
    }
    assert!(checked > 0, "fixtures.tsv contained no fixture lines");
}
