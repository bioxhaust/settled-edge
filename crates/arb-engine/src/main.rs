#![forbid(unsafe_code)]
//! The CLI. One subcommand this round.
//!
//! ```text
//! cargo run -p arb-engine -- replay fixtures/
//! ```
//!
//! Argument parsing is hand-rolled because hard rule 1 forbids external
//! dependencies, and because a scanner whose argument parser needs six crates has
//! already lost the argument about complexity. Unknown flags are refused rather
//! than ignored: a typo'd `--notional` that silently keeps the default would make
//! the operator read a table about a size they did not ask for.

use std::path::PathBuf;
use std::process::ExitCode;

fn usage() -> String {
    "arb-engine — cross-venue funding and basis scanner\n\
     \n\
     USAGE:\n\
     \x20   arb-engine replay <fixtures-dir>\n\
     \n\
     COMMANDS:\n\
     \x20   replay <dir>   Load every .fix file in <dir> and print the ranked table.\n\
     \n\
     Every number this prints is computed from fixtures that were invented from\n\
     memory of exchange API shapes and never verified against a live API. The\n\
     arithmetic is tested; the inputs are not measurements. See\n\
     FIXTURES-UNVERIFIED.md.\n"
        .to_string()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut it = args.iter();

    let cmd = match it.next() {
        Some(c) => c.as_str(),
        None => {
            print!("{}", usage());
            return ExitCode::FAILURE;
        }
    };

    match cmd {
        "replay" => {
            let dir = match it.next() {
                Some(d) => PathBuf::from(d),
                None => {
                    eprintln!("replay needs a directory\n");
                    print!("{}", usage());
                    return ExitCode::FAILURE;
                }
            };
            if let Some(extra) = it.next() {
                eprintln!("unexpected argument: {extra}\n");
                print!("{}", usage());
                return ExitCode::FAILURE;
            }
            run_replay(&dir)
        }
        "-h" | "--help" | "help" => {
            print!("{}", usage());
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command: {other}\n");
            print!("{}", usage());
            ExitCode::FAILURE
        }
    }
}

fn run_replay(dir: &std::path::Path) -> ExitCode {
    let cfg = match arb_engine::replay_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration is unusable: {e}");
            return ExitCode::FAILURE;
        }
    };
    match arb_engine::replay(dir, &cfg) {
        Ok(r) => {
            print!("{}", arb_engine::render(&r, &cfg));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("replay failed: {e}");
            ExitCode::FAILURE
        }
    }
}
