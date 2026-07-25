//! `kabipay-auth-hash` — compute a seed-ready argon2id hash for a password.
//!
//! Usage:
//!   cargo run -p kabipay-auth --bin kabipay-auth-hash -- <password>
//!
//! Used by `kabipay-database/scripts/seed-demo-data.ps1` (and manual ops) so we never check in
//! fabricated hashes that won't verify.

use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
use argon2::Argon2;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(password) = args.next() else {
        eprintln!("usage: kabipay-auth-hash <password>");
        std::process::exit(2);
    };

    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing failed")
        .to_string();
    println!("{hash}");
}
