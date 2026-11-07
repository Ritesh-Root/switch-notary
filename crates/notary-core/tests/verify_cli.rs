//! Exit codes for `notary-verify` argument parsing. Receipt checks live in `receipt.rs`.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_notary-verify")).args(args).output().expect("run notary-verify")
}

const USAGE: &str = "usage: notary-verify <receipt.json> [--key <public key hex>]\n";

#[test]
fn help_exits_0_with_usage() {
    for flag in ["-h", "--help"] {
        let out = run(&[flag]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        assert_eq!(String::from_utf8(out.stdout).expect("stdout"), USAGE, "{flag}");
        assert_eq!(out.stderr, b"", "{flag}");
    }
}

#[test]
fn missing_receipt_path_exits_2() {
    for args in [&[][..], &["--key", "ab"][..]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(String::from_utf8(out.stderr).expect("stderr"), USAGE, "{args:?}");
        assert_eq!(out.stdout, b"", "{args:?}");
    }
}

#[test]
fn missing_key_argument_exits_2() {
    let out = run(&["--key"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(String::from_utf8(out.stderr).expect("stderr"), "--key needs the expected public key in hex\n");
    assert_eq!(out.stdout, b"");
}
