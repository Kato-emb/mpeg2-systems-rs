//! Development tasks of the workspace, run as `cargo xtask <task>`

mod fixtures;

use std::env;
use std::process::ExitCode;

/// What `cargo xtask --help` prints
const USAGE: &str = "\
Usage: cargo xtask <task>

Tasks:
  fixtures    Generate the stream fixtures of the tests with FFmpeg, or the
              ARIB caption fixture with TSDuck (--arib)
";

fn main() -> ExitCode {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();

    match arguments.as_slice() {
        ["fixtures"] => match fixtures::generate() {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("cargo xtask fixtures: {message}");
                ExitCode::FAILURE
            }
        },
        ["fixtures", "--arib"] => match fixtures::arib::generate() {
            Ok(()) => ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("cargo xtask fixtures --arib: {message}");
                ExitCode::FAILURE
            }
        },
        ["fixtures", "--help" | "-h"] => {
            print!("{}", fixtures::USAGE);
            ExitCode::SUCCESS
        }
        [] | ["--help" | "-h"] => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}
