//! `cargo-depgate` is a dependency policy enforcer and CI gatekeeper for Cargo
//! workspaces.

use std::io::Write as _;

fn main() {
    let args = match cargo_depgate::cli::parse_from(std::env::args_os()) {
        Ok(args) => args,
        Err(error) => error.exit(),
    };

    let result = cargo_depgate::cli::run(&args);
    if let Err(ref error) = result {
        eprintln!("{error}");
        let mut cause = std::error::Error::source(error);
        while let Some(source) = cause {
            eprintln!("  caused by: {source}");
            cause = source.source();
        }
    }

    let code = cargo_depgate::error::exit_code_for(&result);

    let _ = std::io::stdout().flush();

    std::process::exit(code.into());
}
