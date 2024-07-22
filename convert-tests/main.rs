#![warn(
    rust_2018_idioms,
    trivial_casts,
    trivial_numeric_casts,
    unreachable_pub,
    unused_qualifications
)]
#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod defs;
mod test;

fn main() -> ExitCode {
    let root_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap();
    let tests_path = Path::new("convert-tests").join("tests");

    let tests_paths = gather_tests(&root_path, &tests_path);

    let args = libtest_mimic::Arguments::from_args();

    let mut tests = Vec::new();
    for test_path in tests_paths {
        let full_test_path = root_path.join(&test_path);
        tests.push(libtest_mimic::Trial::test(
            test_path.to_string_lossy(),
            move || test::run_test(&full_test_path).map_err(|e| e.into()),
        ));
    }

    let conclusion = libtest_mimic::run(&args, tests);
    if conclusion.has_failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn gather_tests(root_path: &Path, tests_path: &Path) -> BTreeSet<PathBuf> {
    let mut tests = BTreeSet::new();

    let mut dir_queue = Vec::new();
    dir_queue.push(tests_path.to_path_buf());

    while let Some(current_sub_dir) = dir_queue.pop() {
        let current_dir = root_path.join(&current_sub_dir);
        for entry in current_dir.read_dir().unwrap() {
            let entry = entry.unwrap();
            let entry_type = entry.file_type().unwrap();
            let entry_name = entry.file_name();

            if entry_type.is_dir() {
                dir_queue.push(current_sub_dir.join(entry_name));
                continue;
            }

            let extension = Path::new(&entry_name).extension();
            if extension == Some(OsStr::new("yaml")) || extension == Some(OsStr::new("yml")) {
                let inserted = tests.insert(current_sub_dir.join(entry_name));
                assert!(inserted);
            }
        }
    }

    tests
}
