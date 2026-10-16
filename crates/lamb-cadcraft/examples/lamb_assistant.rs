//! One assistant turn from a shell; see `lamb_cadcraft::assistant_cli` for the usage.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = lamb_cadcraft::assistant_cli::run(&args, None, &mut std::io::stdout(), &mut std::io::stderr());
    std::process::exit(code);
}
