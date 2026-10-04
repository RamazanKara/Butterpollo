use anyhow::Result;
use axum::{Extension, Router};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use std::{net::SocketAddr, sync::Arc};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self, DigitallySignedStruct, DistinguishedName, Error, SignatureScheme,
        client::danger::HandshakeSignatureValid,
        pki_types::{CertificateDer, UnixTime},
        server::danger::{ClientCertVerified, ClientCertVerifier},
    },
};

#[derive(Clone)]
pub struct Connection {
    pub peer: SocketAddr,
    pub local: SocketAddr,
    pub certificate: Option<Vec<u8>>,
    pub tls: bool,
}
/// TLS proves possession of any presented key. Protected HTTP routes additionally
/// require one exact enabled pairing record; a self-signed certificate is not authorization.
#[derive(Debug)]
struct ClientProof {
    provider: Arc<rustls::crypto::CryptoProvider>,
}
impl ClientCertVerifier for ClientProof {
    fn client_auth_mandatory(&self) -> bool {
        false
    }
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        end: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> std::result::Result<ClientCertVerified, Error> {
        if end.is_empty() || end.len() > 16384 {
            return Err(Error::General("invalid client certificate size".into()));
        }
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
pub fn acceptor(
    identity: &butterpollo_core::crypto::Identity,
    client_auth: bool,
) -> Result<TlsAcceptor> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?;
    let builder = if client_auth {
        builder.with_client_cert_verifier(Arc::new(ClientProof { provider }))
    } else {
        builder.with_no_client_auth()
    };
    let mut keys = identity.private_pem.as_bytes();
    let key = rustls_pemfile::private_key(&mut keys)?
        .ok_or_else(|| anyhow::anyhow!("empty server private key"))?;
    let config = builder.with_single_cert(vec![CertificateDer::from(identity.der.clone())], key)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}
pub async fn serve(
    address: SocketAddr,
    router: Router,
    acceptor: Option<TlsAcceptor>,
) -> Result<()> {
    let listener = crate::network::tcp(address)?;
    tracing::info!(%address,tls=acceptor.is_some(),"HTTP listener ready");
    loop {
        let (socket, peer) = listener.accept().await?;
        let peer = SocketAddr::new(peer.ip().to_canonical(), peer.port());
        socket.set_nodelay(true)?;
        let local = socket.local_addr()?;
        let local = SocketAddr::new(local.ip().to_canonical(), local.port());
        let router = router.clone();
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            if let Some(acceptor) = acceptor {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    acceptor.accept(socket),
                )
                .await
                {
                    Ok(Ok(stream)) => {
                        let certificate = stream
                            .get_ref()
                            .1
                            .peer_certificates()
                            .and_then(|chain| chain.first())
                            .map(|c| c.to_vec());
                        let service =
                            TowerToHyperService::new(router.layer(Extension(Connection {
                                peer,
                                local,
                                certificate,
                                tls: true,
                            })));
                        let _ = Builder::new(TokioExecutor::new())
                            .serve_connection_with_upgrades(TokioIo::new(stream), service)
                            .await;
                    }
                    _ => tracing::debug!(%peer,"TLS handshake rejected"),
                }
            } else {
                let service = TowerToHyperService::new(router.layer(Extension(Connection {
                    peer,
                    local,
                    certificate: None,
                    tls: false,
                })));
                let _ = Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(TokioIo::new(socket), service)
                    .await;
            }
        });
    }
}
