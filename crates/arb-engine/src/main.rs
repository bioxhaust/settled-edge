#![forbid(unsafe_code)]
//! The CLI. Two subcommands: one deterministic, one live.
//!
//! ```text
//! cargo run -p arb-engine -- replay fixtures/
//! cargo run -p arb-engine -- live BTC ETH SOL
//! ```
//!
//! `replay` prints numbers computed from invented fixtures and says so. `live`
//! fetches public market data over HTTPS and prints measured numbers. They share
//! every layer between the adapter and the report, which is the point: the live
//! path is not a second engine, it is the same engine given real inputs.
//!
//! Argument parsing is hand-rolled because hard rule 1 forbids external
//! dependencies, and because a scanner whose argument parser needs six crates has
//! already lost the argument about complexity. Unknown flags are refused rather
//! than ignored: a typo'd `--notional` that silently keeps the default would make
//! the operator read a table about a size they did not ask for.

use std::path::PathBuf;
use std::process::ExitCode;

use arb_num::Money;

fn usage() -> String {
    "arb-engine — cross-venue funding and basis scanner\n\
     \n\
     USAGE:\n\
     \x20   arb-engine replay <fixtures-dir>\n\
     \x20   arb-engine live [--notional <N>] [BASE ...]\n\
     \n\
     COMMANDS:\n\
     \x20   replay <dir>   Load every .fix file in <dir> and print the ranked table.\n\
     \x20                  Offline, deterministic, and every number in it was invented.\n\
     \x20   live [BASE..]  Fetch public order books, funding and instrument specs over\n\
     \x20                  HTTPS and print what was measured. Bases are assets, not\n\
     \x20                  venue symbols: `BTC`, not `BTCUSDT`. Defaults to BTC ETH SOL.\n\
     \x20                  Needs no API key — nothing here authenticates.\n\
     \n\
     OPTIONS (live):\n\
     \x20   --notional <N> Per-leg size in whole quote units. Default 1000.\n\
     \n\
     `replay` numbers are computed from fixtures that were invented from memory of\n\
     exchange API shapes and never verified against a live API. The arithmetic is\n\
     tested; the inputs are not measurements. See FIXTURES-UNVERIFIED.md.\n\
     \n\
     `live` numbers are measured, with one stated exception: no public endpoint\n\
     reveals an account's fee tier, so the worst published tier is assumed and every\n\
     row carries FEE_ASSUMED. That assumption runs one way — the real fee can only be\n\
     lower — so a net figure here is understated, never flattering. Ranking is not\n\
     refused for it; a read-only API key replaces the assumption with a measurement.\n"
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
        "live" => {
            let rest: Vec<String> = it.cloned().collect();
            run_live(&rest)
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

/// Parse `live`'s arguments.
///
/// Bases are upper-cased here rather than in the adapter, so that `arb-engine live
/// btc` works without `InstrumentKey::parse` having to decide what casing means.
/// A base that is not a valid asset name is rejected by `parse` later, by name.
fn parse_live_args(args: &[String]) -> Result<(Vec<String>, Money), String> {
    let mut bases: Vec<String> = Vec::new();
    let mut notional = arb_engine::live::DEFAULT_NOTIONAL;

    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--notional" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| "--notional needs a number".to_string())?;
                notional = v
                    .parse::<i64>()
                    .map_err(|_| format!("--notional {v}: not a whole number of quote units"))?;
                if notional <= 0 {
                    return Err(format!("--notional {notional}: a size must be positive"));
                }
                i += 2;
            }
            // Refused rather than treated as a base asset: a mistyped flag that
            // became a symbol would produce "not listed" instead of "unknown flag",
            // and the operator would go looking at the exchange.
            flag if flag.starts_with('-') => return Err(format!("unknown option: {flag}")),
            base => {
                bases.push(base.to_ascii_uppercase());
                i += 1;
            }
        }
    }

    if bases.is_empty() {
        bases = arb_engine::live::DEFAULT_BASES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
    }

    let notional = Money::from_int(notional).map_err(|e| format!("--notional: {e}"))?;
    Ok((bases, notional))
}

fn run_live(args: &[String]) -> ExitCode {
    let (bases, notional) = match parse_live_args(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}\n");
            print!("{}", usage());
            return ExitCode::FAILURE;
        }
    };

    match arb_engine::live::live(&bases, notional) {
        Ok(s) => {
            // Rebuilt rather than returned alongside, so the report is rendered
            // against exactly the configuration the scan ran under.
            let cfg = match arb_engine::live::live_config(notional, s.now) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("configuration is unusable: {e}");
                    return ExitCode::FAILURE;
                }
            };
            print!("{}", arb_engine::live::render_live(&s, &cfg));
            // A run that fetched nothing at all is a failure, not an empty table.
            // Every instrument having failed usually means no network, and a zero
            // exit code there would make a cron job look healthy.
            if s.legs.is_empty() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("live scan failed: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn no_bases_means_the_default_three() {
        let (bases, notional) = parse_live_args(&[]).expect("no args is valid");
        assert_eq!(bases, vec!["BTC", "ETH", "SOL"]);
        assert_eq!(notional.raw(), 1_000_000_000_000_000);
    }

    #[test]
    fn a_lower_case_base_is_the_same_base() {
        let (bases, _) = parse_live_args(&s(&["btc", "Eth"])).expect("valid");
        assert_eq!(bases, vec!["BTC", "ETH"]);
    }

    #[test]
    fn a_mistyped_flag_is_refused_and_not_read_as_a_symbol() {
        // The failure this guards: `--notionl 5000` silently scanning an asset
        // called "--NOTIONL" at the default size, and reporting "not listed".
        let e = parse_live_args(&s(&["--notionl", "5000"])).expect_err("must refuse");
        assert!(e.contains("unknown option"), "{e}");
    }

    #[test]
    fn a_notional_that_is_not_a_number_is_refused_rather_than_defaulted() {
        let e = parse_live_args(&s(&["--notional", "1e4"])).expect_err("must refuse");
        assert!(e.contains("not a whole number"), "{e}");
        let e = parse_live_args(&s(&["--notional", "0"])).expect_err("must refuse");
        assert!(e.contains("must be positive"), "{e}");
        let e = parse_live_args(&s(&["--notional"])).expect_err("must refuse");
        assert!(e.contains("needs a number"), "{e}");
    }

    #[test]
    fn a_notional_and_bases_can_be_given_in_either_order() {
        let (a, an) = parse_live_args(&s(&["--notional", "5000", "BTC"])).expect("valid");
        let (b, bn) = parse_live_args(&s(&["BTC", "--notional", "5000"])).expect("valid");
        assert_eq!(a, b);
        assert_eq!(an, bn);
        assert_eq!(an.raw(), 5_000_000_000_000_000);
    }
}
