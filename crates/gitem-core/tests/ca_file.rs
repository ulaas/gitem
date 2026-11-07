//! The CA file setting, against a local TLS server with a made-up authority. It changes a process
//! global, so it has this binary to itself. No system store is written to, and no live host.
gitem_testenv::isolate!();

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use gitem_core::forge::{self, ForgeClient, ForgeKind, RemoteRepo};

fn serve(config: rustls::ServerConfig) -> u16 {
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let mut conn = rustls::ServerConnection::new(config.clone()).unwrap();
            let mut tls = rustls::Stream::new(&mut conn, &mut s);
            let mut buf = [0u8; 2048];
            if matches!(tls.read(&mut buf), Ok(n) if n > 0) {
                let body = r#"{"login":"me"}"#;
                let _ = write!(tls, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = tls.flush();
            }
        }
    });
    port
}

#[test]
fn a_ca_file_trusts_its_authority_and_a_bad_one_leaves_the_store_in_charge() {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = certified.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(certified.signing_key.serialize_der().into());
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider())).with_safe_default_protocol_versions().unwrap().with_no_client_auth().with_single_cert(vec![cert], key).unwrap();
    let port = serve(config);
    let repo = RemoteRepo { kind: ForgeKind::GitHub, host: "localhost".into(), path: "o/r".into(), api_base: Some(format!("https://localhost:{port}")), templates: Default::default(), origin: None };
    let client = ForgeClient::new(repo, "FAKE_TOKEN".into(), String::new());
    let who = || client.whoami().map_err(|e| e.to_string());

    let refused = who().unwrap_err();
    assert!(refused.to_lowercase().contains("certificate"), "{refused}");

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("authorities.pem");
    std::fs::write(&file, certified.cert.pem()).unwrap();
    forge::set_ca_file(&file.to_string_lossy()).unwrap();
    assert_eq!(forge::ca_file_problem(), None);
    assert_eq!(who().unwrap(), "me");

    // Trusting one authority isn't trusting all: another's certificate is refused.
    let other = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let other_file = dir.path().join("other.pem");
    std::fs::write(&other_file, other.cert.pem()).unwrap();
    forge::set_ca_file(&other_file.to_string_lossy()).unwrap();
    let e = who().unwrap_err();
    assert!(e.to_lowercase().contains("certificate") || e.to_lowercase().contains("issuer"), "{e}");

    // A file that can't be used says so, and checks no less.
    let empty = dir.path().join("empty.pem");
    std::fs::write(&empty, "not a certificate\n").unwrap();
    let why = forge::set_ca_file(&empty.to_string_lossy()).unwrap_err();
    assert!(why.contains("no certificates"), "{why}");
    assert_eq!(forge::ca_file_problem().as_deref(), Some(why.as_str()));
    assert!(who().unwrap_err().to_lowercase().contains("certificate"));
    assert!(forge::set_ca_file(&dir.path().join("missing.pem").to_string_lossy()).unwrap_err().contains("Can't read"));

    // Blank is back to the store.
    forge::set_ca_file("").unwrap();
    assert_eq!(forge::ca_file_problem(), None);
    assert!(who().unwrap_err().to_lowercase().contains("certificate"));
}
