//! Binary entry: parse with `--help`/`--version` as protocol exceptions (exit 0 via
//! Clap). Every other outcome — including runtime usage errors — is exactly one JSON object on
//! stdout with a `session_id`, then `process::exit(code)`.

use llxprt_code_rs::cli;

fn main() {
    let session_hint = cli::session_hint();
    let args = cli::parse_args_fallback();
    let outcome = cli::run(args);
    let v = cli::json(&outcome, &session_hint);
    let code = cli::exit_code(&outcome);
    println!("{v}");
    std::process::exit(code);
}
