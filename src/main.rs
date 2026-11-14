fn main() {
    match lennon::cli::main_entry() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("lennon: {}", lennon::cli::safe_text(&e.to_string()));
            std::process::exit(1);
        }
    }
}
