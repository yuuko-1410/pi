//! pi binary entry point: thin wrapper over `app::main`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = pi_coding_agent::app::main(&args, Default::default());
    std::process::exit(code);
}
