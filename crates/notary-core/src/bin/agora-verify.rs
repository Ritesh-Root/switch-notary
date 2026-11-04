//! `agora-verify receipt.json [--key <hex>]`: check a Notary receipt offline.
//! Exit 0 and one OK line if every record re-hashes, the chain ends at the signed head and
//! the signature holds; exit 1 and the reason otherwise. Logic from agora-rs's agora-verify,
//! changed to check a self-contained bundle instead of a database.

use notary_core::receipt::{Bundle, verify};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut path = None;
    let mut key = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--key" => key = it.next().cloned(),
            "-h" | "--help" => {
                println!("usage: agora-verify <receipt.json> [--key <public key hex>]");
                return;
            }
            other => path = Some(other.to_string()),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: agora-verify <receipt.json> [--key <public key hex>]");
        std::process::exit(2);
    };
    let result = std::fs::read(&path)
        .map_err(|e| format!("can't read {path}: {e}"))
        .and_then(|bytes| serde_json::from_slice::<Bundle>(&bytes).map_err(|e| format!("{path} isn't a receipt: {e}")))
        .and_then(|bundle| verify(&bundle, key.as_deref()));
    match result {
        Ok(v) => println!("OK {}: {} entries, chain intact, head {}, signed by {}", v.run, v.entries, &v.head[..16], &v.key[..16]),
        Err(why) => {
            println!("FAIL: {why}");
            std::process::exit(1);
        }
    }
}
