fn main() {
    let code = ferraus_cli::main_with_args(std::env::args().collect());
    std::process::exit(code);
}
