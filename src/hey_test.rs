// Copyright 2014 Google Inc. All Rights Reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Port of hey_test.go. Kept as an in-crate test module so it can reach the
//! private items of `main.rs`, mirroring Go's in-package `package main` test.

use super::{parse_input_with_regexp, AUTH_REGEXP, HEADER_REGEXP};

#[test]
fn test_parse_valid_header_flag() {
    let r = parse_input_with_regexp("X-Something: !Y10K:;(He@poverflow?)", HEADER_REGEXP);
    let m = match r {
        Ok(m) => m,
        Err(e) => panic!("parseInputWithRegexp errored: {}", e),
    };
    let (got, want) = (&m[1], "X-Something");
    assert_eq!(got, want, "got {}; want {}", got, want);
    let (got, want) = (&m[2], "!Y10K:;(He@poverflow?)");
    assert_eq!(got, want, "got {}; want {}", got, want);
}

#[test]
fn test_parse_invalid_header_flag() {
    let r = parse_input_with_regexp("X|oh|bad-input: badbadbad", HEADER_REGEXP);
    if r.is_ok() {
        panic!("Header parsing errored; want no errors");
    }
}

#[test]
fn test_parse_valid_auth_flag() {
    let r = parse_input_with_regexp("_coo-kie_:!!bigmonster@1969sid", AUTH_REGEXP);
    let m = match r {
        Ok(m) => m,
        Err(e) => panic!("A valid auth flag was not parsed correctly: {}", e),
    };
    let (got, want) = (&m[1], "_coo-kie_");
    assert_eq!(got, want, "got {}; want {}", got, want);
    let (got, want) = (&m[2], "!!bigmonster@1969sid");
    assert_eq!(got, want, "got {}; want {}", got, want);
}

#[test]
fn test_parse_invalid_auth_flag() {
    let r = parse_input_with_regexp("X|oh|bad-input: badbadbad", AUTH_REGEXP);
    if r.is_ok() {
        panic!("Header parsing errored; want no errors");
    }
}

#[test]
fn test_parse_auth_meta_characters() {
    let r = parse_input_with_regexp("plus+$*{:boom", AUTH_REGEXP);
    if let Err(e) = r {
        panic!(
            "Auth header with a plus sign in the user name errored: {}",
            e
        );
    }
}
