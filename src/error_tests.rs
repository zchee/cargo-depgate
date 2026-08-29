use std::{io, path::PathBuf, time::Duration};

use super::*;

fn invalid_json_error() -> serde_json::Error {
    let Err(error) = serde_json::from_str::<serde_json::Value>("{") else {
        panic!("the deliberately incomplete JSON unexpectedly parsed successfully");
    };
    error
}

fn error_cases() -> Vec<(Error, u8)> {
    vec![
        (Error::PolicyViolations { count: 2 }, 1),
        (Error::Configuration { message: "invalid rule".to_owned() }, 2),
        (Error::Usage { message: "missing argument".to_owned() }, 2),
        (Error::NotYetImplemented { subcommand: "check".to_owned() }, 2),
        (
            Error::CargoMetadataSpawn {
                source: io::Error::new(io::ErrorKind::NotFound, "cargo was not found"),
            },
            3,
        ),
        (Error::CargoMetadataTimeout { timeout: Duration::from_secs(300) }, 3),
        (
            Error::CargoMetadataRead {
                source: io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"),
            },
            3,
        ),
        (Error::CargoMetadataFailed { status: Some(101) }, 3),
        (Error::CargoMetadataUnparseable { source: invalid_json_error() }, 3),
        (
            Error::MetadataRead {
                path: PathBuf::from("metadata.json"),
                source: io::Error::new(io::ErrorKind::NotFound, "missing"),
            },
            3,
        ),
        (Error::MetadataInvalid { message: "resolve is null".to_owned() }, 3),
    ]
}

#[test]
fn timeout_message_names_the_flag_and_the_whole_seconds() {
    let error = Error::CargoMetadataTimeout { timeout: Duration::from_secs(1) };

    assert_eq!(error.to_string(), "cargo metadata exceeded --cargo-timeout=1s");
}

#[test]
fn every_error_variant_maps_to_its_contract_exit_code() {
    for (error, expected_exit_code) in error_cases() {
        assert_eq!(error.exit_code(), expected_exit_code, "unexpected mapping for {error:?}");
    }
}

#[test]
fn successful_result_maps_to_zero() {
    assert_eq!(exit_code_for(&Ok(())), 0);
}

#[test]
fn every_error_result_maps_to_its_error_exit_code() {
    for (error, expected_exit_code) in error_cases() {
        assert_eq!(exit_code_for(&Err(error)), expected_exit_code, "unexpected result mapping");
    }
}
