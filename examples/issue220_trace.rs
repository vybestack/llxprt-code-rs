//! Export one completed issue220 trial through the production session validator.
//! No direct slot parsing, fallback reader, or cross-session directory scanning.
use llxprt_code_rs::session::{SessionId, SessionStore};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let session = args.next().ok_or("expected session id")?;
    if args.next().is_some() || !session.starts_with("issue220-") {
        return Err("expected exactly one issue220- session id".into());
    }
    let root = std::env::var_os("LLXPRT_CONFIG_HOME").ok_or("set LLXPRT_CONFIG_HOME")?;
    let store = SessionStore::load_at(&SessionId::parse(&session)?, std::path::Path::new(&root))?;
    let state = store.snapshot()?;
    println!("{}", serde_json::to_string_pretty(&state)?);
    Ok(())
}
