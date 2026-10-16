//! The 9-click-region FILLET matrix; see `lamb_cadcraft::fillet_matrix` for the usage.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = lamb_cadcraft::fillet_matrix::run(&args, &mut std::io::stdout(), &mut std::io::stderr());
    std::process::exit(code);
}
