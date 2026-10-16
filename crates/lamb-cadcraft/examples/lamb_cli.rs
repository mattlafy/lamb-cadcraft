//! Command records from stdin applied headlessly; see `lamb_cadcraft::cli` for the usage.

use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut stdin = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut stdin) {
        eprintln!("stdin: {e}");
        std::process::exit(1);
    }
    let code = lamb_cadcraft::cli::run(&args, &stdin, &mut std::io::stdout(), &mut std::io::stderr());
    std::process::exit(code);
}
