// mux
use sosistab2::{Multiplex, /*Stream*/};

// pipe
use sosistab2::{Pipe, PipeListener};
use sosistab2_obfsws::{ws, ObfsWsPipe, ObfsWsListener};

// types
use std::net::{SocketAddr, Ipv4Addr, Ipv6Addr};
use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use sosistab2::{MuxSecret, MuxPublic};

// traits
use futures_util::{AsyncReadExt, AsyncWriteExt};
use rand::{Rng, RngCore};
use anyhow::Context;

// concurrent lock
use async_std::sync::Arc;
/*use async_lock::Mutex;*/

// smol
use smol::channel::{Sender, Receiver};

// async TCP socket
use async_std::net::{TcpStream, TcpListener};

// socks protocol implemention
use socksv5::SocksVersion;
use socksv5::v4::{SocksV4RequestStatus, SocksV4Host};
use socksv5::v5::{SocksV5RequestStatus, SocksV5Host, SocksV5AuthMethod};

// time
use std::time::{Duration, SystemTime, Instant};

// standard base64 encode/decode
use base64::Engine;
use base64::prelude::BASE64_STANDARD;

// CHACHA20 cipher
use cryptoxide::chacha20::ChaCha20;

// command line arguments parse
use clap::Parser;

#[derive(Clone, Debug, clap::Parser)]
struct ClientOpt {
    #[arg(long)]
    /// URL of wsocks server. e.g. wss://example.com/DestroyGFW
    /// both ws:// (plaintext) and wss:// (encrypted) scheme are supported, due to wsocks server is probably behind a reverse-proxy (and TLS is provided by this reverse proxy)
    remote_url: ws::Uri,

    #[arg(long)]
    /// a public key of wsocks server, encoded by hex format, usually 32 bytes (provides 256-bit security)
    remote_public_key: String,

    #[arg(long, default_value="2")]
    /// the max limit of opened websocket connection
    remote_pipes_max: u8,

    /// local proxy server (socks4 / socks4a / socks5) listen address
    #[arg(long, default_value="127.0.0.1:1989")]
    local_socks_listen: SocketAddr,

    /// local proxy server (HTTP) listen address
    #[arg(long, default_value="127.0.0.1:1986")]
    local_http_listen: SocketAddr,
    /* redsocks_listen: u16, */
}

#[derive(Clone, Debug, clap::Parser)]
struct ServerOpt {
    /// ws:// (plaintext) listen address of wsocks server.
    /// TLS layer should be provided by another reverse proxy.
    #[arg(long, default_value="[::]:2038")]
    listen: SocketAddr,

    #[arg(long)]
    key_file: String,

    #[arg(long)]
    http_path: Option<String>,
}

#[derive(Clone, Debug, clap::Parser)]
#[command(author, version, about, long_about)]
enum Opt {
    Client(ClientOpt),
    Server(ServerOpt),
    Info,
}

/// a helper for convert from hex string to 256 bit key
fn hex2key(h: &str) -> anyhow::Result<[u8; 32]> {
    hex2bytes(h)
}
fn hex2bytes<const LENGTH: usize>(h: &str) -> anyhow::Result<[u8; LENGTH]> {
    let mut b = [0u8; LENGTH];
    hex::decode_to_slice(h, &mut b)?;
    Ok(b)
}

#[derive(Debug, Serialize, Deserialize)]
enum NetworkError {
    ServerFailure,
    ConnectionNotAllowed,
    NetworkUnreachable,
    HostUnreachable,
    ConnectionRefused,
    TtlExpired,
    Other(String)
}

impl Into<SocksV4RequestStatus> for NetworkError {
    fn into(self) -> SocksV4RequestStatus {
        log::warn!("wsocks server return connecting error: {self:?}");
        SocksV4RequestStatus::Failed
    }
}
impl Into<SocksV5RequestStatus> for NetworkError {
    fn into(self) -> SocksV5RequestStatus {
        match self {
            Self::ServerFailure => SocksV5RequestStatus::ServerFailure,
            Self::ConnectionNotAllowed => SocksV5RequestStatus::ConnectionNotAllowed,
            Self::NetworkUnreachable => SocksV5RequestStatus::NetworkUnreachable,
            Self::HostUnreachable => SocksV5RequestStatus::HostUnreachable,
            Self::ConnectionRefused => SocksV5RequestStatus::ConnectionRefused,
            Self::TtlExpired => SocksV5RequestStatus::TtlExpired,
            Self::Other(_) => {
                log::warn!("wsocks server return custom error: {self:?}");
                SocksV5RequestStatus::ServerFailure
            }
        }
    }
}

impl From<std::io::Error> for NetworkError {
    fn from(err: std::io::Error) -> Self {
        use std::io::ErrorKind as k;
        match err.kind() {
            // Refused
            k::ConnectionRefused => Self::ConnectionRefused,
            k::ConnectionReset => Self::ConnectionRefused,
            k::ConnectionAborted => Self::ConnectionRefused,
            k::BrokenPipe => Self::ConnectionRefused,

            // Host unreachable
            k::TimedOut => Self::HostUnreachable,

            // Not allowed
            k::PermissionDenied => Self::ConnectionNotAllowed,

            // Network unreachable
            #[cfg(feature = "io_error_more")]
            k::NetworkDown => Self::NetworkUnreachable,
            #[cfg(feature = "io_error_more")]
            k::NetworkUnreachable => Self::NetworkUnreachable,

            // Other unknown error
            _ => Self::Other(format!("{:?}", err))
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
enum OfferError {
    UnableConnect(NetworkError),
    //UnableResume(SessionError),
}

#[derive(Debug, Serialize, Deserialize)]
enum Protocol {
    // client wants to open a new TCP connection
    TcpConnect {
        dst: String, // can be IP address or Domain name
        port: u16, // 0-65535
    },

    // server offers the client request (one of TcpConnect, TcpResume, TcpClose).
    TcpOffer {
        id: Result<u128, OfferError>,
    },

    // proxied TCP stream data. send by both client and server
    TcpData {
        id: u128,
        payload: Vec<u8>,
    },

    /*
    TcpBind {
        addr: SocketAddr,
    },
    UDPAssociate {
    },
    UDPOffer {
        id: Result<u128, OfferError>,
    },
    UDPMessage {
        id: u128,
        src_addr: SocketAddr,
        dst_addr: SocketAddr,
        message: Vec<u8>,
    },
    */
}

#[derive(Debug, Serialize, Deserialize)]
struct Frame {
    protocol: Protocol,
    // this field is for hide metadata of packet length, so it's content is complete junk and can be safety ignored
    padding: Vec<u8>,
}

impl From<Protocol> for Frame {
    fn from(protocol: Protocol) -> Self {
        Self {
            protocol,
            padding: vec![0u8; fastrand::usize(1..=128)]
        }
    }
}
impl Frame {
    async fn send(&self, mut relconn: impl AsyncWriteExt + Unpin) -> anyhow::Result<()> {
        let buf = bincode::serialize(self)?;

        let len: usize = buf.len();
        if len > 65535 {
            anyhow::bail!("Bug: Protocol Frame too large! all frame should equal or small than 65535.");
        }
        let len: [u8; 2] = (len as u16).to_be_bytes();

        let mut msg = Vec::new();
        msg.extend(&len);
        msg.extend(&buf);
        relconn.write_all(&msg).await?;
        Ok(())
    }

    async fn recv(mut relconn: impl AsyncReadExt + Unpin) -> anyhow::Result<Self> {
        let mut len = [0u8; 2];
        relconn.read_exact(&mut len).await?;

        let len = u16::from_be_bytes(len) as usize;
        let mut buf = vec![0u8; len];
        relconn.read_exact(&mut buf).await?;

        let this: Self = bincode::deserialize(&buf)?;
        Ok(this)
    }
}

// a helper for create "per-application unique" hash
const NS_WSOCKS_CLIENT_ETAG: &[u8; 32] = b"\xb2\xdf\xa0\x91\xde\xc2\x07\xbf\xce\x9e\x9f\x82\xb4\xf6\x85\x16\xe2\xb6\xae\xf8\xab\xa4\xa0\x90\x96\xd3\xf8\xfe\x02\xcb\xc5\xa8";
// this is used for generate client session key
fn client_etag_hash(data: &[u8]) -> blake3::Hash {
    blake3::keyed_hash(NS_WSOCKS_CLIENT_ETAG, data)
}

const NS_WSOCKS_SERVER_SECRET: &[u8; 32] = b"\xb6\xae\xa3\x98\xc4\xde\xf3\x10\xf8\xc5\xe8\x14\x0e\xd4\xbc\x9e\x81\xc5\x9f\xc8\x1d\xe8\xce\xe1\xd6\xf9\x95\xf3\x12\x0f\xbf\x11";
fn server_sk_hash(data: &[u8]) -> blake3::Hash {
    blake3::keyed_hash(NS_WSOCKS_SERVER_SECRET, data)
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct Etag {
    random: Vec<u8>,
    time1: SystemTime,
    time2: Instant,
    pk_hash: blake3::Hash,
}

impl Etag {
    fn new(pk: MuxPublic) -> Self {
        let mut random = vec![0u8; 1024];
        rand::thread_rng().fill_bytes(&mut random);
        Self {
            random,
            time1: SystemTime::now(),
            time2: Instant::now(),
            pk_hash: client_etag_hash(pk.as_bytes()),
        }
    }

    fn calc_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }

    fn generate(&self) -> String {
        let nonce: [u8; 8] = rand::thread_rng().gen();
        let hash: [u8; 8] = self.calc_hash().to_be_bytes();

        let out = {
            // first we encrypt the hash result.
            let mut hash = hash.clone();
            ChaCha20::new(self.pk_hash.as_bytes(), &nonce).process_mut(&mut hash);

            // structs the buffer for store output
            let mut buf = Vec::new();

            // first 8 bytes: random-generated nonce.
            buf.extend(nonce);

            // last 8 bytes: encrypted hash.
            buf.extend(hash);

            // now we get the "Sec-Websocket-Key"
            buf
        };
        // make sure we generates a valid Websocket-Key
        assert_eq!(out.len(), 16);

        // encode as base64 and output.
        let out = BASE64_STANDARD.encode(out);
        out
    }

    fn from(pk: MuxPublic, b64: &str) -> anyhow::Result<u64> {
        let buf = BASE64_STANDARD.decode(b64)?;
        if buf.len() != 16 {
            anyhow::bail!("Wrong length of Sec-Websocket-Key!");
        }

        let nonce = &buf[..8];
        assert_eq!(nonce.len(), 8);
        let mut hash = buf[8..].to_vec();
        assert_eq!(hash.len(), 8);

        let pk = pk.as_bytes();
        ChaCha20::new(client_etag_hash(pk).as_bytes(), &nonce).process_mut(&mut hash);

        // this hack is needed for convert from &[u8] to [u8; 8]
        // is there anyone can tell me a better way?
        let hash = {
            let mut h = [0u8; 8];
            for i in 0..8 {
                h[i] = hash[i];
            }
            u64::from_be_bytes(h)
        };
        Ok(hash)
    }
}

impl std::hash::Hash for Etag {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        let h = format!("{:?}", self);
        let h = client_etag_hash(h.as_bytes());
        state.write(h.as_bytes());
    }
}

impl ToString for Etag {
    fn to_string(&self) -> String {
        self.generate()
    }
}
impl ToString for &Etag {
    fn to_string(&self) -> String {
        self.generate()
    }
}

async fn client(copt: ClientOpt) -> anyhow::Result<()> {
    let pubkey = MuxPublic::from_bytes(
        hex2key(&copt.remote_public_key).context("[hex::decode] cannot parse remote public key")?
    );

    println!("{:#}", env_info());

    // the helper that can generate a unique "session-id" for each request.
    let etag = Etag::new(pubkey);

    let max_conn = copt.remote_pipes_max as usize;

    let mux = Arc::new(Multiplex::new(MuxSecret::generate(), Some(pubkey)));

    let pipemgr_fut = {
        let etag = etag.clone();
        let url = copt.remote_url.clone();
        let mux = mux.clone();
        async move {
            let mut pipe: ObfsWsPipe;
            loop {
                // only kept "alive" pipes (remove any closed ws pipe)
                mux.retain(|p| { p.peer_addr().len() > 0 });

                if mux.iter_pipes().collect::<Vec<_>>().len() >= max_conn {
                    smol::Timer::after(Duration::from_secs(1)).await;
                    continue;
                }

                pipe = match ObfsWsPipe::connect(&url, &etag).await {
                    Ok(v) => v,
                    Err(err) => {
                        log::error!("Unable connect to wsocks server: cannot establish websocket conn (URL={:?}): {:?}", &url, err);
                        smol::Timer::after(Duration::from_secs(3)).await;
                        continue;
                    }
                };
                mux.add_pipe(pipe);
            }

            #[allow(unreachable_code)]
            Ok::<_, anyhow::Error>(())
        }
    };

    let (req_tx, req_rx) = smol::channel::unbounded();
    let request_fut = {
        let mux = mux.clone();
        async move {
            loop {
                let (req, conn, resp): (Protocol, TcpStream, Sender<(Result<u128, OfferError>, sosistab2::Stream)>) = req_rx.recv().await?;
                log::trace!("local proxy server received new request: req={req:?} | conn={conn:?}");
                let mux = mux.clone();
                smolscale::spawn(async move {
                    let mut relconn = mux.open_conn("").await?;

                    // send req
                    Frame::from(req).send(&mut relconn).await?;

                    // recv resp
                    let res = Frame::recv(&mut relconn).await?.protocol;

                    match res {
                        Protocol::TcpOffer { id } => {
                            resp.send((id, relconn)).await?;
                        }
                        _ => { anyhow::bail!("receive a unexpected frame type from wsocks server, only TcpOffer expected."); }
                    }
                    Ok::<_, anyhow::Error>(())
                }).detach();
            }

            #[allow(unreachable_code)]
            Ok::<_, anyhow::Error>(())
        }
    };

    let socks_fut = {
        let req_tx = req_tx.clone();
        let socksd = TcpListener::bind(copt.local_socks_listen).await?;
        async move {
            let mut conn: TcpStream;
            let mut peer: SocketAddr;
            loop {
                (conn, peer) = socksd.accept().await?;
                log::trace!("local socks listener accept raw TCP connection from {peer:?}");
                let req_tx = req_tx.clone();
                smolscale::spawn(async move {
                    let dst: String;
                    let port: u16;

                    let version = socksv5::read_version(&conn).await?;
                    match version {
                        SocksVersion::V4 => {
                            let req = socksv5::v4::read_request_skip_version(&conn).await?;
                            port = req.port;

                            dst = match req.host {
                                SocksV4Host::Ip(ip4) => {
                                    Ipv4Addr::from(ip4).to_string()
                                }
                                SocksV4Host::Domain(name) => {
                                    String::from_utf8_lossy(&name).to_string()
                                }
                            };
                        }
                        SocksVersion::V5 => {
                            let _ = socksv5::v5::read_handshake_skip_version(&conn).await?;
                            socksv5::v5::write_auth_method(&conn, SocksV5AuthMethod::Noauth).await?;

                            let req = socksv5::v5::read_request(&conn).await?;
                            port = req.port;

                            dst = match req.host {
                                SocksV5Host::Ipv4(ip4) => {
                                    Ipv4Addr::from(ip4).to_string()
                                }
                                SocksV5Host::Ipv6(ip6) => {
                                    Ipv6Addr::from(ip6).to_string()
                                }
                                SocksV5Host::Domain(name) => {
                                    String::from_utf8_lossy(&name).to_string()
                                }
                            }
                        }
                    }

                    let req = Protocol::TcpConnect {
                        dst: dst.clone(),
                        port,
                    };

                    let (resp_tx, resp_rx) = smol::channel::bounded(1);
                    req_tx.send((req, conn.clone(), resp_tx)).await?;

                    let (offer_id, relconn) = resp_rx.recv().await?;
                    match offer_id {
                        Ok(id) => {
                            log::info!("(Socks proxy) connected to remote {dst:?}:{port:?} ! ID={id:?}");
                            match version {
                                SocksVersion::V4 => {
                                    socksv5::v4::write_request_status(
                                        &mut conn,
                                        SocksV4RequestStatus::Granted,
                                        [0, 1, 1, 1],
                                        port,
                                    ).await?;
                                },
                                SocksVersion::V5 => {
                                    socksv5::v5::write_request_status(
                                        &mut conn,
                                        SocksV5RequestStatus::Success,
                                        SocksV5Host::Domain(dst.as_bytes().to_vec()),
                                        port,
                                    ).await?;
                                }
                            }

                            // forward traffic...
                            tcp_forward_loop(None, id, conn, relconn).await?;
                            /*smol::future::race(
                                smol::io::copy(relconn.clone(), conn.clone()),
                                smol::io::copy(conn, relconn)
                            ).await.unwrap();*/
                        },
                        Err(err) => {
                            let err: NetworkError = match err {
                                OfferError::UnableConnect(e) => e,
                            };
                            match version {
                                SocksVersion::V4 => {
                                    socksv5::v4::write_request_status(
                                        &mut conn,
                                        err.into(),
                                        [0, 0, 0, 0],
                                        0,
                                    ).await?;
                                },
                                SocksVersion::V5 => {
                                    socksv5::v5::write_request_status(
                                        &mut conn,
                                        err.into(),
                                        SocksV5Host::Ipv4([0, 0, 0, 0]),
                                        0,
                                    ).await?;
                                },
                            }
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                }).detach();
            }

            #[allow(unreachable_code)]
            Ok::<_, anyhow::Error>(())
        }
    };

    let http_fut = {
        let req_tx = req_tx.clone();
        let httpd = TcpListener::bind(copt.local_http_listen).await?;
        async move {
            let mut conn: TcpStream;
            let mut peer: SocketAddr;
            loop {
                (conn, peer) = httpd.accept().await?;
                let req_tx = req_tx.clone();
                smolscale::spawn(async move {
                    let dst: String;
                    let port: u16;

                    log::info!("http received request from {peer:?}");
                    let (req, _) = async_h1::server::decode(conn.clone()).await.unwrap().expect("cannot parse http req"); // `async_h1::StdError` incompatible `anyhow::Error`
                    let url = req.url();
                    log::info!("incoming http request: url={:?} parsed={req:?}", url.to_string());
                    let method = req.method().to_string();
                    match &method[..] {
                        "CONNECT" => {
                            dst = url.host_str().context("cannot parse 'host' from http proxy request")?.to_string();
                            port = url.port_or_known_default().context("cannot parse 'port' from http proxy request")?;

                            let req = Protocol::TcpConnect {
                                dst: dst.clone(),
                                port
                            };

                            let (resp_tx, resp_rx) = smol::channel::bounded(1);
                            req_tx.send((req, conn.clone(), resp_tx)).await?;

                            let (offer_id, relconn) = resp_rx.recv().await?;
                            match offer_id {
                                Ok(id) => {
                                    log::info!("(HTTP proxy) connected to remote {dst:?}:{port:?} ! ID={id:?}");

                                    conn.write_all(b"HTTP/1.0 200 Connection established\r\n\r\n").await?;
        
                                    // forward traffic...
                                    tcp_forward_loop(None, id, conn, relconn).await?;
                                    /*smol::future::race(
                                        smol::io::copy(relconn.clone(), conn.clone()),
                                        smol::io::copy(conn, relconn)
                                    ).await.unwrap();*/
                                },
                                _ => { todo!() }
                            }
                        },
                        _ => {
                            conn.write_all(b"HTTP/1.0 400 unsupported non-CONNECT request\r\n\r\n").await?;
                            conn.close().await?;
                        }
                    }
                    Ok::<_, anyhow::Error>(())
                }).detach();
            }

            #[allow(unreachable_code)]
            Ok::<_, anyhow::Error>(())
        }
    };

    mux.add_drop_friend(smolscale::spawn(socks_fut));
    mux.add_drop_friend(smolscale::spawn(http_fut));
    mux.add_drop_friend(smolscale::spawn(request_fut));

    pipemgr_fut.await
}

async fn server(sopt: ServerOpt) -> anyhow::Result<()> {
    let key_file = sopt.key_file.clone();
    let listen = sopt.listen.clone();

    let mut i = 0;
    let server_sk: MuxSecret = loop {
        // this is important for avoid infinity loop
        i += 1;
        if i >= 10 {
            anyhow::bail!("Cannot get long-term secret key of server!");
        }

        match smol::fs::read(&key_file).await {
            Ok(data) => {
                let key = server_sk_hash(&data);
                break MuxSecret::from_bytes(key.into());
            },
            Err(err) => {
                log::warn!("Cannot read your long-term secret key file {key_file:?} (Error={err:?})... so generate new secret key, then save it into disk.");
                let pre_sk = {
                    let buf_len = fastrand::usize(2048..=8192);
                    let mut buf = vec![0u8; buf_len];
                    rand::thread_rng().fill_bytes(&mut buf);
                    buf

                    /*
                    let mut buf: Vec<String> = vec![];
                    for _ in 128..fastrand::usize(256..=512) {
                        let seed: [u8; 32] = rand::thread_rng().gen();
                        let mnemonic = bip39::Mnemonic::from_entropy(&seed)?;
                        buf.push(mnemonic.to_string());
                    }
                    serde_json::json!(buf).to_string().as_bytes().to_vec()
                    */
                };
                smol::fs::write(&key_file, pre_sk).await.context("Cannot write new key to disk!?")?;
            }
        }
    };

    let server_pk = server_sk.to_public();

    let info = serde_json::json!({
        "env": env_info(),
        "public_key": hex::encode(server_pk.as_bytes()),
        "listen": sopt.listen.to_string(),
    });
    println!("{info:#}"); // {:#} format to JSON pretty

    eprintln!("Server Public Key: {:?}", hex::encode(server_pk.as_bytes()));

    // ws pipe listener - future
    let wpl_fut = {
        let mut mux_sessions: HashMap< u64, Arc<Multiplex> > = HashMap::new();

        // ws pipe listener
        let wpl = ObfsWsListener::bind(listen).await?;

        async move {
            loop {
                let pipe: Arc<dyn Pipe> = wpl.accept_pipe().await?;
                let hash: u64 = Etag::from(server_pk, pipe.peer_metadata())?;
                log::info!("geted client session: {hash}");

                let server_sk = server_sk.clone();
                let mux = mux_sessions.entry(hash)
                .or_insert_with(move || {
                    let mux = Arc::new(
                        Multiplex::new(server_sk, None)
                    );
                    log::info!("created new Multiplex for session {hash} !");
                    smolscale::spawn(server_session_loop(hash, mux.clone())).detach();
                    mux
                });
                mux.add_pipe(pipe);

                log::info!("Wsocks Server accepted new ws pipe (session: {hash})...");
            }

            #[allow(unreachable_code)]
            Ok::<_, anyhow::Error>(())
        }
    };

    wpl_fut.await
}

async fn server_session_loop(hash: u64, mux: Arc<Multiplex>) -> anyhow::Result<()> {
    let (deadline_tx, deadline_rx) = smol::channel::bounded(1);

    let pipemgr_fut = {
        let mux = mux.clone();
        async move {
            let mut last_alive = Instant::now();
            loop {
                // only kept "alive" pipes (remove any closed ws pipe)
                mux.retain(|p| { p.peer_addr().len() > 0 });

                if mux.iter_pipes().collect::<Vec<_>>().len() > 0 {
                    last_alive = Instant::now();
                }

                if last_alive.elapsed().as_secs_f64() > 100.0 {
                    // notify this mux session dead.
                    deadline_tx.send(()).await.unwrap();
                    break;
                }

                smol::Timer::after(Duration::from_secs(1)).await;
            }
        }
    };
    smolscale::spawn(pipemgr_fut).detach();

    loop {
        if deadline_rx.try_recv().is_ok() {
            anyhow::bail!("session {hash:?} deadline reached");
        }

        let mut relconn = mux.accept_conn().await?;
        log::info!("session accepted new relconn");
        let deadline_rx = deadline_rx.clone();
        smolscale::spawn(async move {
            let req = Frame::recv(&mut relconn).await.unwrap().protocol;
            match req {
                Protocol::TcpData{..} => {
                    anyhow::bail!("unexcepted receive a TCP data without handshake");
                },

                // invalid due to *we* are server
                Protocol::TcpOffer{..} => {
                    anyhow::bail!("unexpected receive a TCP offer from client");
                },

                // valid client request.
                Protocol::TcpConnect{ dst, port } => {
                    // try connect to client-requested remote
                    match smol::net::TcpStream::connect((dst, port)).await {
                        Ok(conn) => {
                            let id = fastrand::u128(..);
                            let offer = Protocol::TcpOffer {
                                id: Ok(id),
                            };
                            Frame::from(offer).send(&mut relconn).await.unwrap();

                            tcp_forward_loop(Some(deadline_rx), id, conn, relconn).await.unwrap();
                            /*
                            smol::future::race(
                                smol::io::copy(relconn.clone(), conn.clone()),
                                smol::io::copy(conn, relconn)
                            ).await.unwrap();
                            */
                        },
                        Err(err) => {
                            let offer = Protocol::TcpOffer {
                                id: Err(
                                    OfferError::UnableConnect(
                                        NetworkError::from(err)
                                    )
                                ),
                            };
                            Frame::from(offer).send(&mut relconn).await.unwrap();
                            std::mem::drop(relconn);
                        }
                    }
                },
            }
            Ok(())
        }).detach();
    }
}

async fn tcp_forward_loop<RW: AsyncReadExt + AsyncWriteExt + Clone + Unpin>(
    deadline_rx: Option<Receiver<()>>,
    offer_id: u128,
    mut conn: RW, // for compatible many variants of "smol::net::TcpStream", "async_std::net::TcpStream", "async_net::TcpStream", etc...
    mut relconn: sosistab2::Stream,
) -> anyhow::Result<()> {
    let down_fut = {
        let mut relconn = relconn.clone();
        let mut conn = conn.clone();
        let deadline_rx = deadline_rx.clone();
        async move {
            let mut frame;
            loop {
                if let Some(ref deadline_rx) = deadline_rx {
                    if deadline_rx.try_recv().is_ok() {
                        anyhow::bail!("conn {offer_id:?} deadline reached");
                    }
                }

                frame = Frame::recv(&mut relconn).await?;
                match frame.protocol {
                    Protocol::TcpData {
                        id, payload
                    } => {
                        assert_eq!(id, offer_id);
                        conn.write_all(&payload).await?;
                    },

                    _ => { todo!() }
                }
            }
        }
    };

    let up_fut = async move {
        let mut buf = vec![0u8; 60000];
        let mut frame;
        loop {
            if let Some(ref deadline_rx) = deadline_rx {
                if deadline_rx.try_recv().is_ok() {
                    anyhow::bail!("conn {offer_id:?} deadline reached");
                }
            }

            let size = conn.read(&mut buf).await?;

            frame = Frame::from(Protocol::TcpData {
                id: offer_id,
                payload: (&buf[..size]).to_vec(),
            });
            frame.send(&mut relconn).await?;
        }
    };

    smol::future::race(up_fut, down_fut).await
}

async fn async_main() -> anyhow::Result<()> {
    let opt = Opt::parse();
    eprintln!("{:?}", opt);
    match opt {
        Opt::Client(copt) => {
            client(copt).await
        },
        Opt::Server(sopt) => {
            server(sopt).await
        }
        Opt::Info => {
            println!("{:#}", env_info());
            Ok(())
        }
    }
}

fn cfg_info() -> serde_json::Value {
    /* cfg!(target_arch) */
    let mut target_arch: Vec<&str> = vec![];
    if cfg!(target_arch="x86") {
        target_arch.push("x86");
    }
    if cfg!(target_arch="x86_64") {
        target_arch.push("x86_64");
    }
    if cfg!(target_arch="mips") {
        target_arch.push("mips");
    }
    if cfg!(target_arch="powerpc") {
        target_arch.push("powerpc");
    }
    if cfg!(target_arch="powerpc64") {
        target_arch.push("powerpc64");
    }
    if cfg!(target_arch="arm") {
        target_arch.push("arm");
    }
    if cfg!(target_arch="aarch64") {
        target_arch.push("aarch64");
    }
    let target_arch: String = target_arch.join(", ");

    /* cfg!(target_feature) */
    let mut target_feature: Vec<&str> = vec![];
    if cfg!(target_feature="avx") {
        target_feature.push("avx");
    }
    if cfg!(target_feature="avx2") {
        target_feature.push("avx2");
    }
    if cfg!(target_feature="crt-static") {
        target_feature.push("crt-static");
    }
    if cfg!(target_feature="rdrand") {
        target_feature.push("rdrand");
    }
    if cfg!(target_feature="sse") {
        target_feature.push("sse");
    }
    if cfg!(target_feature="sse2") {
        target_feature.push("sse2");
    }
    if cfg!(target_feature="sse4.1") {
        target_feature.push("sse4.1");
    }
    let target_feature: String = target_feature.join(", ");

    /* cfg!(target_os) */
    let mut target_os: Vec<&str> = vec![];
    if cfg!(target_os="linux") {
        target_os.push("linux");
    }
    if cfg!(target_os="windows") {
        target_os.push("windows");
    }
    if cfg!(target_os="macos") {
        target_os.push("macos");
    }
    if cfg!(target_os="android") {
        target_os.push("android");
    }
    if cfg!(target_os="ios") {
        target_os.push("ios");
    }
    if cfg!(target_os="freebsd") {
        target_os.push("freebsd");
    }
    if cfg!(target_os="dragonfly") {
        target_os.push("dragonfly");
    }
    if cfg!(target_os="openbsd") {
        target_os.push("openbsd");
    }
    if cfg!(target_os="none") {
        target_os.push("none");
    }
    let target_os: String = target_os.join(", ");

    /* cfg!(target_family) */
    let mut target_family: Vec<&str> = vec![];
    if cfg!(target_family="unix") {
        target_family.push("unix");
    }
    if cfg!(target_family="windows") {
        target_family.push("windows");
    }
    if cfg!(target_family="wasm") {
        target_family.push("wasm");
    }
    let target_family: String = target_family.join(", ");

    /* cfg!(target_env) */
    let mut target_env: Vec<&str> = vec![];
    if cfg!(target_env="gnu") {
        target_env.push("gnu");
    }
    if cfg!(target_env="musl") {
        target_env.push("musl");
    }
    if cfg!(target_env="sgx") {
        target_env.push("sgx");
    }
    if cfg!(target_env="msvc") {
        target_env.push("msvc");
    }
    let target_env: String = target_env.join(", ");

    /* cfg!(target_endian) */
    let mut target_endian: Vec<&str> = vec![];
    if cfg!(target_endian="little") {
        target_endian.push("little");
    }
    if cfg!(target_endian="big") {
        target_endian.push("big");
    }
    let target_endian: String = target_endian.join(", ");

    /* cfg!(target_pointer_width) */
    let mut target_pointer_width: Vec<&str> = vec![];
    if cfg!(target_pointer_width="64") {
        target_pointer_width.push("64");
    }
    if cfg!(target_pointer_width="32") {
        target_pointer_width.push("32");
    }
    if cfg!(target_pointer_width="16") {
        target_pointer_width.push("16");
    }
    let target_pointer_width: String = target_pointer_width.join(", ");

    /* cfg!(target_vendor) */
    let mut target_vendor: Vec<&str> = vec![];
    if cfg!(target_vendor="pc") {
        target_vendor.push("pc");
    }
    if cfg!(target_vendor="apple") {
        target_vendor.push("apple");
    }
    if cfg!(target_vendor="fortanix") {
        target_vendor.push("fortanix");
    }
    if cfg!(target_vendor="unknown") {
        target_vendor.push("unknown");
    }
    let target_vendor: String = target_vendor.join(", ");

    /* cfg!(target_has_atomic) */
    let mut target_has_atomic: Vec<&str> = vec![];
    if cfg!(target_has_atomic="8") {
        target_has_atomic.push("8");
    }
    if cfg!(target_has_atomic="16") {
        target_has_atomic.push("16");
    }
    if cfg!(target_has_atomic="32") {
        target_has_atomic.push("32");
    }
    if cfg!(target_has_atomic="64") {
        target_has_atomic.push("64");
    }
    if cfg!(target_has_atomic="128") {
        target_has_atomic.push("128");
    }
    if cfg!(target_has_atomic="ptr") {
        target_has_atomic.push("ptr");
    }
    let target_has_atomic: String = target_has_atomic.join(", ");

    /* cfg!(panic) */
    let mut panic: Vec<&str> = vec![];
    if cfg!(panic="unwind") {
        panic.push("unwind");
    }
    if cfg!(panic="abort") {
        panic.push("abort");
    }
    let panic: String = panic.join(", ");

    /* cfg!(test) */
    let test: bool = if cfg!(test) { true } else { false };

    /* cfg!(debug_assertions) */
    let debug_assertions: bool = if cfg!(debug_assertions) { true } else { false };

    /* cfg!(proc_macro) */
    let proc_macro: bool = if cfg!(proc_macro) { true } else { false };

    serde_json::json!({
        "target_arch": target_arch,
        "target_feature": target_feature,
        "target_os": target_os,
        "target_family": target_family,
        "target_env": target_env,
        "target_endian": target_endian,
        "target_pointer_width": target_pointer_width,
        "target_vendor": target_vendor,
        "target_has_atomic": target_has_atomic,
        "panic": panic,
        "test": test,
        "debug_assertions": debug_assertions,
        "proc_macro": proc_macro,
    })
}

fn build_info() -> serde_json::Value {
    serde_json::json!({
        "time": option_env!("VERGEN_BUILD_TIMESTAMP"),
        "git" : {
            "commit": option_env!("VERGEN_GIT_SHA"),
            "branch": option_env!("VERGEN_GIT_BRANCH"),
        },
        "cargo": {
            "opt-level": option_env!("VERGEN_CARGO_OPT_LEVEL"),
            "features": option_env!("VERGEN_CARGO_FEATURES"),
            "debug": option_env!("VERGEN_CARGO_DEBUG"),
        },
        "rustc": {
            "llvm_version": option_env!("VERGEN_RUSTC_LLVM_VERSION"),

            "rustc_version": option_env!("VERGEN_RUSTC_SEMVER"),
            "rustc_channel": option_env!("VERGEN_RUSTC_CHANNEL"),
            "rustc_git_commit": option_env!("VERGEN_RUSTC_COMMIT_HASH"),

            "host_triple": option_env!("VERGEN_RUSTC_HOST_TRIPLE"),
            "target_triple": option_env!("VERGEN_CARGO_TARGET_TRIPLE"),
        },
    })
}

fn env_info() -> serde_json::Value {
    serde_json::json!({
        "build": build_info(),
        "name": "wsocks - web socks",
        "license": option_env!("CARGO_PKG_LICENSE"),
        "repo": option_env!("CARGO_PKG_REPOSITORY"),
        "version": option_env!("CARGO_PKG_VERSION"),
        "cfg": cfg_info(),
    })
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    smol::block_on(async_main())
}
