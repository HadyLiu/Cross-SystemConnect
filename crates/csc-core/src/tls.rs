//! TLS 1.3 for the control channel.
//!
//! Each computer keeps a self-signed certificate next to its config. The
//! handshake proves the peer holds that key. Trust stores the certificate
//! fingerprint; a different certificate is not accepted silently.

use anyhow::Context;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, Error, SignatureScheme};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub struct Identity {
    cert_pem: String,
    key_pem: String,
    pub fingerprint: String,
}

#[derive(Debug)]
pub struct PeerCert {
    der: Mutex<Option<Vec<u8>>>,
}

impl PeerCert {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            der: Mutex::new(None),
        })
    }

    pub fn take(self: &Arc<Self>) -> anyhow::Result<Vec<u8>> {
        self.der
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| anyhow::anyhow!("peer did not present a certificate"))
    }
}

pub fn fingerprint(der: &[u8]) -> String {
    let hash = Sha256::digest(der);
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

impl Identity {
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let cert_path = dir.join("identity.crt");
        let key_path = dir.join("identity.key");
        if cert_path.is_file() && key_path.is_file() {
            let cert_pem = std::fs::read_to_string(&cert_path)
                .with_context(|| format!("read {}", cert_path.display()))?;
            let key_pem = std::fs::read_to_string(&key_path)
                .with_context(|| format!("read {}", key_path.display()))?;
            return identity_from_pem(cert_pem, key_pem);
        }
        let key_pair = KeyPair::generate().context("generate identity key")?;
        let mut params = CertificateParams::new(vec!["csc.local".into()]).context("cert params")?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, "Cross-SystemConnect");
        params.distinguished_name = dn;
        let cert = params
            .self_signed(&key_pair)
            .context("self-sign identity")?;
        let cert_pem = cert.pem();
        let key_pem = key_pair.serialize_pem();
        std::fs::write(&cert_path, &cert_pem)
            .with_context(|| format!("write {}", cert_path.display()))?;
        std::fs::write(&key_path, &key_pem)
            .with_context(|| format!("write {}", key_path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
        }
        identity_from_pem(cert_pem, key_pem)
    }

    fn material(&self) -> anyhow::Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
        let cert = CertificateDer::from_pem_slice(self.cert_pem.as_bytes())
            .map_err(|e| anyhow::anyhow!("identity cert: {e}"))?;
        let key = PrivateKeyDer::from_pem_slice(self.key_pem.as_bytes())
            .map_err(|e| anyhow::anyhow!("identity key: {e}"))?;
        Ok((cert, key))
    }

    pub fn client_config(&self, peer: Arc<PeerCert>) -> anyhow::Result<rustls::ClientConfig> {
        let (cert, key) = self.material()?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Arc::new(Verifier {
            peer,
            subjects: Vec::new(),
        });
        rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .context("tls versions")?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(vec![cert], key)
            .context("client certificate")
    }

    pub fn server_config(&self, peer: Arc<PeerCert>) -> anyhow::Result<rustls::ServerConfig> {
        let (cert, key) = self.material()?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = Arc::new(Verifier {
            peer,
            subjects: Vec::new(),
        });
        rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .context("tls versions")?
            .with_client_cert_verifier(verifier)
            .with_single_cert(vec![cert], key)
            .context("server certificate")
    }
}

fn identity_from_pem(cert_pem: String, key_pem: String) -> anyhow::Result<Identity> {
    let cert = CertificateDer::from_pem_slice(cert_pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("identity cert: {e}"))?;
    let fingerprint = fingerprint(cert.as_ref());
    Ok(Identity {
        cert_pem,
        key_pem,
        fingerprint,
    })
}

#[derive(Debug)]
struct Verifier {
    peer: Arc<PeerCert>,
    subjects: Vec<rustls::DistinguishedName>,
}

impl Verifier {
    fn remember(&self, end_entity: &CertificateDer<'_>) {
        *self.peer.der.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(end_entity.as_ref().to_vec());
    }

    fn verify12(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        self.remember(end_entity);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verify12(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

impl ClientCertVerifier for Verifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &self.subjects
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        self.remember(end_entity);
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verify12(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_sha256_hex() {
        let fp = fingerprint(b"a");
        assert_eq!(fp.len(), 64);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn handshake_exchanges_peer_certificates() {
        let base = std::env::temp_dir().join(format!("csc-tls-hs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let client_id = Identity::load(&base.join("client")).expect("client identity");
        let server_id = Identity::load(&base.join("server")).expect("server identity");
        assert_ne!(client_id.fingerprint, server_id.fingerprint);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server_fp = server_id.fingerprint.clone();
        let client_fp = client_id.fingerprint.clone();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept");
            let peer = PeerCert::new();
            let config = server_id
                .server_config(peer.clone())
                .expect("server config");
            let _stream = tokio_rustls::TlsAcceptor::from(Arc::new(config))
                .accept(tcp)
                .await
                .expect("server handshake");
            fingerprint(&peer.take().expect("client cert"))
        });

        let tcp = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let peer = PeerCert::new();
        let config = client_id
            .client_config(peer.clone())
            .expect("client config");
        let name = rustls::pki_types::ServerName::try_from("csc.local").expect("name");
        let _stream = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(name, tcp)
            .await
            .expect("client handshake");
        assert_eq!(fingerprint(&peer.take().expect("server cert")), server_fp);
        assert_eq!(server.await.expect("server task"), client_fp);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn load_creates_stable_identity() {
        let dir = std::env::temp_dir().join(format!("csc-tls-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = Identity::load(&dir).expect("create");
        let second = Identity::load(&dir).expect("reload");
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.fingerprint.len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
