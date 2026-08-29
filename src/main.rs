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
        if let cargo_depgate::error::Error::Configuration { message, span } = error {
            let stderr = anstream::stderr();
            let color = stderr.current_choice() != anstream::ColorChoice::Never;
            let mut stderr = stderr.lock();
            drop(cargo_depgate::cli::render_configuration_error(
                message,
                span.as_ref(),
                color,
                &mut stderr,
            ));
        } else {
            eprintln!("{error}");
            let mut cause = std::error::Error::source(error);
            while let Some(source) = cause {
                eprintln!("  caused by: {source}");
                cause = source.source();
            }
        }
    }

    let mut code = cargo_depgate::error::exit_code_for(&result);

    // A flush that fails for any reason other than the reader going away is a report that
    // was not delivered: exit 4, like any other report write failure.
    if let Err(error) = std::io::stdout().flush()
        && error.kind() != std::io::ErrorKind::BrokenPipe
    {
        eprintln!("failed to write the report: {error}");
        code = cargo_depgate::error::Error::ReportWrite { source: error }.exit_code();
    }

    std::process::exit(code.into());
}
