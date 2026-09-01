use llxprt_code_rs::envelope::schema_bytes;

fn main() {
    print!("{}", String::from_utf8_lossy(&schema_bytes()));
}
