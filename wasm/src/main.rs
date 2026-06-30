use std::io::Read;

fn main() {
    let mut request = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut request) {
        eprintln!("error reading stdin: {e}");
        std::process::exit(2);
    }
    println!("{}", pgsafe_wasm::lint_json(&request));
}
