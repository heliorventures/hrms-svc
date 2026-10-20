//! Explicit trust for fixed organization-plane HTTPS endpoints.
use crate::{KabiPayError, KabiPayResult};
use std::{io::Read, path::Path, time::Duration};
const MAX_CA_BYTES: u64 = 1024 * 1024;
fn invalid() -> KabiPayError {
    KabiPayError::Validation("Invalid organization HTTP CA configuration".into())
}
fn roots(path: &Path) -> KabiPayResult<Vec<reqwest::Certificate>> {
    if !path.is_absolute() {
        return Err(invalid());
    }
    let file = std::fs::File::open(path).map_err(|_| invalid())?;
    if !file.metadata().map_err(|_| invalid())?.is_file() {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(MAX_CA_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid())?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_CA_BYTES {
        return Err(invalid());
    }
    let certificates = reqwest::Certificate::from_pem_bundle(&bytes).map_err(|_| invalid())?;
    if certificates.is_empty() || certificates.len() > 32 {
        return Err(invalid());
    }
    Ok(certificates)
}
/// Trust only the explicitly supplied additional CA bundle. Defaults retain
/// reqwest's public roots; hostname and certificate verification stay enabled.
pub fn client(timeout: Duration) -> KabiPayResult<reqwest::Client> {
    client_builder(timeout)?.build().map_err(|_| invalid())
}
/// Reuse the same verified private CA policy for protocols with a separate
/// connect deadline. The caller cannot supply a CA path or disable verification.
pub(crate) fn client_builder(timeout: Duration) -> KabiPayResult<reqwest::ClientBuilder> {
    let path = std::env::var_os("KABIPAY_ORGANIZATION_HTTP_CA_FILE");
    builder_with_ca(timeout, path.as_deref().map(Path::new))
}
#[cfg(test)]
fn client_with_ca(timeout: Duration, path: Option<&Path>) -> KabiPayResult<reqwest::Client> {
    builder_with_ca(timeout, path)?
        .build()
        .map_err(|_| invalid())
}
fn builder_with_ca(
    timeout: Duration,
    path: Option<&Path>,
) -> KabiPayResult<reqwest::ClientBuilder> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(timeout);
    if let Some(path) = path {
        for certificate in roots(path)? {
            builder = builder.add_root_certificate(certificate);
        }
    }
    Ok(builder)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_ca_keeps_default_verification_and_missing_ca_fails_closed() {
        assert!(client_with_ca(Duration::from_secs(1), None).is_ok());
        assert!(client_with_ca(
            Duration::from_secs(1),
            Some(Path::new("missing-organization-ca.pem"))
        )
        .is_err());
    }
    #[test]
    fn ca_bundle_is_bounded_and_must_contain_certificates() {
        let path =
            std::env::temp_dir().join(format!("organization-ca-{}.pem", uuid::Uuid::new_v4()));
        for bytes in [
            b"".to_vec(),
            b"not a certificate".to_vec(),
            vec![b'x'; MAX_CA_BYTES as usize + 1],
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(roots(&path).is_err());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn private_ca_handshake_trusts_only_supplied_ca_and_matching_hostname() {
        use std::{
            io::BufRead,
            process::{Command, Stdio},
        };
        let directory =
            std::env::temp_dir().join(format!("organization-tls-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        struct Server(std::process::Child);
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let cleanup = Cleanup(directory.clone());
        let openssl = if cfg!(windows) {
            "C:/Program Files/Git/mingw64/bin/openssl.exe"
        } else {
            "openssl"
        };
        let run = |arguments: &[&str]| {
            let output = Command::new(openssl)
                .current_dir(&directory)
                .args(arguments)
                .output()
                .expect("OpenSSL fixture tool required");
            assert!(
                output.status.success(),
                "synthetic certificate generation failed"
            );
        };
        run(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
            "-subj",
            "/CN=OrganizationFixtureCA",
            "-days",
            "1",
        ]);
        run(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "wrong-ca.key",
            "-out",
            "wrong-ca.pem",
            "-subj",
            "/CN=UnrelatedFixtureCA",
            "-days",
            "1",
        ]);
        run(&[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "server.key",
            "-out",
            "server.csr",
            "-subj",
            "/CN=localhost",
        ]);
        std::fs::write(
            directory.join("extensions.cnf"),
            "subjectAltName=DNS:localhost\n",
        )
        .unwrap();
        run(&[
            "x509",
            "-req",
            "-in",
            "server.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            "server.pem",
            "-days",
            "1",
            "-extfile",
            "extensions.cnf",
        ]);
        let script = "const fs=require('node:fs'),https=require('node:https');const server=https.createServer({cert:fs.readFileSync('server.pem'),key:fs.readFileSync('server.key')},(_,res)=>res.end('ok'));server.listen(0,'127.0.0.1',()=>console.log(server.address().port));";
        let mut child = Server(
            Command::new("node")
                .current_dir(&directory)
                .args(["-e", script])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("Node HTTPS fixture required"),
        );
        let mut port = String::new();
        std::io::BufReader::new(child.0.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        let port: u16 = port.trim().parse().unwrap();
        let ca = directory.join("ca.pem");
        let trusted = client_with_ca(Duration::from_secs(5), Some(&ca)).unwrap();
        let result = trusted
            .get(format!("https://localhost:{port}"))
            .send()
            .await;
        let untrusted = client_with_ca(
            Duration::from_secs(5),
            Some(&directory.join("wrong-ca.pem")),
        )
        .unwrap()
        .get(format!("https://localhost:{port}"))
        .send()
        .await;
        let wrong_hostname = trusted
            .get(format!("https://127.0.0.1:{port}"))
            .send()
            .await;
        drop(child);
        drop(cleanup);
        assert!(
            result.is_ok(),
            "explicit fixture CA must enable verified TLS"
        );
        assert!(untrusted.is_err(), "untrusted fixture CA must reject");
        assert!(
            wrong_hostname.is_err(),
            "trusted CA must not disable hostname verification"
        );
    }
}
