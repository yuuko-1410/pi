//! Port of `packages/protocol/test/cbor/cbor.test.ts`.

use pi_protocol::cbor::{
    decode_cbor, encode_cbor, CborError, CborOptions, DEFAULT_MAX_CBOR_BYTE_LENGTH,
    DEFAULT_MAX_CBOR_CONTAINER_LENGTH, DEFAULT_MAX_CBOR_DEPTH, Value,
};

fn from_hex(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "Hex fixture must contain whole bytes");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("valid hex"))
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn value(hex: &str) -> Value {
    decode_cbor(&from_hex(hex), &CborOptions::default()).expect("valid fixture")
}

fn map(entries: &[(&str, Value)]) -> Value {
    Value::Map(entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

fn arr(items: &[Value]) -> Value {
    Value::Array(items.to_vec())
}

fn num(n: f64) -> Value {
    Value::Number(n)
}

fn str(s: &str) -> Value {
    Value::String(s.to_string())
}

fn bytes(b: &[u8]) -> Value {
    Value::Bytes(b.to_vec())
}

const KNOWN_VECTORS: &[(&str, &str)] = &[
    ("f6", "null"),
    ("f4", "false"),
    ("f5", "true"),
    ("00", "0"),
    ("01", "1"),
    ("0a", "10"),
    ("17", "23"),
    ("1818", "24"),
    ("1819", "25"),
    ("1864", "100"),
    ("1903e8", "1000"),
    ("1a000f4240", "1000000"),
    ("1b000000e8d4a51000", "1000000000000"),
    ("1b001fffffffffffff", "9007199254740991"),
    ("20", "-1"),
    ("29", "-10"),
    ("37", "-24"),
    ("3818", "-25"),
    ("3863", "-100"),
    ("3903e7", "-1000"),
    ("3a000f423f", "-1000000"),
    ("3b001ffffffffffffe", "-9007199254740991"),
    ("fb3ff199999999999a", "1.1"),
    ("fb8000000000000000", "-0"),
    ("4401020304", "bytes 01 02 03 04"),
    ("60", "empty string"),
    ("6449455446", "IETF"),
    ("62c3bc", "ü"),
    ("63e6b0b4", "水"),
    ("64f0908591", "𐅑"),
    ("80", "empty array"),
    ("83010203", "[1,2,3]"),
    ("8301820203820405", "[1,[2,3],[4,5]]"),
    ("a26161016162820203", "{a:1,b:[2,3]}"),
];

fn vector_value(label: &str) -> Value {
    match label {
        "null" => Value::Null,
        "false" => Value::Bool(false),
        "true" => Value::Bool(true),
        "0" => num(0.0),
        "1" => num(1.0),
        "10" => num(10.0),
        "23" => num(23.0),
        "24" => num(24.0),
        "25" => num(25.0),
        "100" => num(100.0),
        "1000" => num(1000.0),
        "1000000" => num(1_000_000.0),
        "1000000000000" => num(1_000_000_000_000.0),
        "9007199254740991" => num(9_007_199_254_740_991.0),
        "-1" => num(-1.0),
        "-10" => num(-10.0),
        "-24" => num(-24.0),
        "-25" => num(-25.0),
        "-100" => num(-100.0),
        "-1000" => num(-1000.0),
        "-1000000" => num(-1_000_000.0),
        "-9007199254740991" => num(-9_007_199_254_740_991.0),
        "1.1" => num(1.1),
        "-0" => num(-0.0),
        "bytes 01 02 03 04" => bytes(&[1, 2, 3, 4]),
        "empty string" => str(""),
        "IETF" => str("IETF"),
        "ü" => str("ü"),
        "水" => str("水"),
        "𐅑" => str("𐅑"),
        "empty array" => arr(&[]),
        "[1,2,3]" => arr(&[num(1.0), num(2.0), num(3.0)]),
        "[1,[2,3],[4,5]]" => arr(&[num(1.0), arr(&[num(2.0), num(3.0)]), arr(&[num(4.0), num(5.0)])]),
        "{a:1,b:[2,3]}" => map(&[("a", num(1.0)), ("b", arr(&[num(2.0), num(3.0)]))]),
        _ => panic!("unknown vector {label}"),
    }
}

#[test]
fn encodes_and_decodes_rfc_8949_vectors() {
    for (wire, label) in KNOWN_VECTORS {
        let value = vector_value(label);
        let encoded = encode_cbor(&value, &CborOptions::default()).expect("encodes");
        assert_eq!(to_hex(&encoded), *wire, "wire mismatch for {label}");
        let decoded = decode_cbor(&from_hex(wire), &CborOptions::default()).expect("decodes");
        if *label == "-0" {
            assert!(matches!(decoded, Value::Number(n) if n == 0.0 && n.is_sign_negative()));
        } else {
            assert_eq!(decoded, value, "round-trip mismatch for {label}");
        }
    }
}

#[test]
fn omits_undefined_object_properties_without_omitting_falsey_values() {
    // JS: { omitted: undefined, zero: 0, empty: "", no: false, nil: null }.
    // Rust has no undefined; the caller omits the key, which encodes the same.
    let value = map(&[
        ("zero", num(0.0)),
        ("empty", str("")),
        ("no", Value::Bool(false)),
        ("nil", Value::Null),
    ]);
    let decoded = decode_cbor(&encode_cbor(&value, &CborOptions::default()).unwrap(), &CborOptions::default())
        .unwrap();
    assert_eq!(decoded, value);
}

#[test]
fn preserves_a_leading_unicode_bom_and_treats_proto_as_data() {
    assert_eq!(value("63efbbbf"), str("\u{feff}"));
    let value = map(&[("__proto__", str("safe"))]);
    let decoded = decode_cbor(&encode_cbor(&value, &CborOptions::default()).unwrap(), &CborOptions::default())
        .unwrap();
    assert_eq!(decoded, value);
}

#[test]
fn rejects_unsupported_encoder_values() {
    // JS also rejects undefined, bigint, symbol, function, Date, Map, array
    // holes, and cyclic values; Rust's Value type rules those out by
    // construction, so only the representable cases are tested here.
    for bad in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        9_007_199_254_740_992.0,  // MAX_SAFE_INTEGER + 1
        -9_007_199_254_740_992.0, // MIN_SAFE_INTEGER - 1
    ] {
        let error = encode_cbor(&Value::Number(bad), &CborOptions::default()).unwrap_err();
        assert!(matches!(error, CborError(_)), "expected CborError for {bad}");
    }
}

#[test]
fn rejects_lossy_strings_cycles_and_excessive_encoder_depth() {
    // JS rejects lone surrogates (\ud800) and cycles; Rust String/Value
    // cannot represent either. Depth is representable and tested.
    let mut too_deep = Value::Null;
    for _ in 0..=DEFAULT_MAX_CBOR_DEPTH {
        too_deep = arr(&[too_deep]);
    }
    let error = encode_cbor(&too_deep, &CborOptions::default()).unwrap_err();
    assert!(error.0.to_lowercase().contains("depth"), "{error:?}");
}

const REJECTED_WIRE: &[&str] = &[
    // empty input
    "",
    // truncated integer
    "18",
    // reserved additional information
    "1c",
    // indefinite byte string
    "5f",
    // indefinite text string
    "7f",
    // indefinite array
    "9f",
    // indefinite map
    "bf",
    // tag
    "c000",
    // undefined
    "f7",
    // unsupported simple value
    "e0",
    // break outside an indefinite item
    "ff",
    // float16
    "f93c00",
    // float32
    "fa3f800000",
    // positive infinity
    "fb7ff0000000000000",
    // NaN
    "fb7ff8000000000000",
    // truncated float64
    "fb3ff00000",
    // truncated byte string
    "44010203",
    // truncated text string
    "636162",
    // truncated array
    "8201",
    // truncated map
    "a16161",
    // trailing data
    "0000",
    // non-string map key
    "a10102",
    // duplicate map key
    "a2616101616102",
    // invalid UTF-8 byte
    "61ff",
    // overlong UTF-8
    "62c080",
    // UTF-8 surrogate
    "63eda080",
    // unsafe positive integer
    "1b0020000000000000",
    // unsafe negative integer
    "3b001fffffffffffff",
    // unsafe integer encoded as float64
    "fb4340000000000000",
];

#[test]
fn rejects_invalid_decoder_input() {
    for wire in REJECTED_WIRE {
        let result = decode_cbor(&from_hex(wire), &CborOptions::default());
        assert!(result.is_err(), "expected error for wire {wire:?}");
        assert!(
            matches!(result, Err(CborError(_))),
            "expected CborError for wire {wire:?}"
        );
    }
}

#[test]
fn enforces_depth_and_declared_length_limits_before_traversing_values() {
    let mut too_deep = vec![0x81u8; (DEFAULT_MAX_CBOR_DEPTH + 2) as usize];
    *too_deep.last_mut().unwrap() = 0xf6;
    let error = decode_cbor(&too_deep, &CborOptions::default()).unwrap_err();
    assert!(error.0.to_lowercase().contains("depth"), "{error:?}");

    let oversized = [
        format!("5a{:08x}", DEFAULT_MAX_CBOR_BYTE_LENGTH + 1),
        format!("7a{:08x}", DEFAULT_MAX_CBOR_BYTE_LENGTH + 1),
        format!("9a{:08x}", DEFAULT_MAX_CBOR_CONTAINER_LENGTH + 1),
        format!("ba{:08x}", DEFAULT_MAX_CBOR_CONTAINER_LENGTH + 1),
    ];
    for wire in oversized {
        let error = decode_cbor(&from_hex(&wire), &CborOptions::default()).unwrap_err();
        assert!(error.0.to_lowercase().contains("limit"), "{error:?}");
    }
}

#[test]
fn supports_stricter_caller_provided_limits() {
    let options = CborOptions {
        max_container_length: 2,
        ..CborOptions::default()
    };
    let error = decode_cbor(&from_hex("83010203"), &options).unwrap_err();
    assert!(error.0.to_lowercase().contains("limit"), "{error:?}");

    let options = CborOptions {
        max_byte_length: 2,
        ..CborOptions::default()
    };
    let error = decode_cbor(&from_hex("626162"), &options).unwrap_err();
    assert!(error.0.to_lowercase().contains("limit"), "{error:?}");

    let options = CborOptions {
        max_container_length: 2,
        ..CborOptions::default()
    };
    let error = encode_cbor(&arr(&[num(1.0), num(2.0), num(3.0)]), &options).unwrap_err();
    assert!(error.0.to_lowercase().contains("limit"), "{error:?}");

    let options = CborOptions {
        max_byte_length: 2,
        ..CborOptions::default()
    };
    let error = encode_cbor(&str("ab"), &options).unwrap_err();
    assert!(error.0.to_lowercase().contains("limit"), "{error:?}");
}
