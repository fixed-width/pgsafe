use std::io::Read;

fn main() {
    let mut sql = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut sql) {
        eprintln!("error reading stdin: {e}");
        std::process::exit(2);
    }
    println!("{}", pgsafe_wasm::run(&sql));
}
