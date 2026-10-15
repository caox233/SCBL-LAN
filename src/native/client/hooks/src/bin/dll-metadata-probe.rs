#[path = "../dll_utils/parse.rs"]
mod dll_parse;
fn main() {
    let path = std::env::args().nth(1).expect("DLL path is required");
    let data = std::fs::read(path).expect("read DLL");
    match dll_parse::parse(&data) {
        Ok(info) => println!("DLL_METADATA_PARSED company_present={} product_present={}", info.company.is_some(), info.product.is_some()),
        Err(error) => { eprintln!("DLL_METADATA_ERROR {error}"); std::process::exit(1); }
    }
}
