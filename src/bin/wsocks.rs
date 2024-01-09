// mux
use sosistab2::Multiplex;

// pipe
use sosistab2::{Pipe, PipeListener};
use sosistab2_obfsws::{ObfsWsPipe, ObfsWsListener};

// types
use std::net::{SocketAddr, Ipv4Addr, Ipv6Addr};
use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use sosistab2::{MuxSecret, MuxPublic};

// traits
use futures_util::{AsyncReadExt, AsyncWriteExt};

// concurrent lock
use async_std::sync::Arc;
use async_lock::Mutex;

// async TCP socket
use async_std::net::{TcpStream, TcpListener};

// socks protocol implemention
use socksv5::SocksVersion;
use socksv5::v4::{SocksV4Request, SocksV4Host};
use socksv5::v5::{SocksV5Request, SocksV5Host, SocksV5AuthMethod};

// time
use std::time::{Duration, SystemTime, Instant};

// command line parse
use clap::Parser;

#[derive(Clone, Debug, clap::Parser)]
#[command(version)]
struct ClientOpt {
    #[arg(long)]
    /// URL of wsocks server. e.g. ws://example.com/DestroyGFW
    remote_url: String,

    #[arg(long)]
    /// a public key of wsocks server, encoded by hex format, usually 32 bytes (provides 256-bit security)
    remote_pubkey: String,

    #[arg(long, default_value="10")]
    /// the max limit of opened websocket connection
    remote_max_websockets: u8,

    // client-side proxy tunnels
    #[arg(long, default_value="127.0.0.1:1989")]
    local_socks_listen: SocketAddr,
    #[arg(long, default_value="127.0.0.1:1986")]
    local_http_listen: SocketAddr,
    /* redsocks_listen: u16, */
}

#[derive(Clone, Debug, clap::Parser)]
struct ServerOpt {
    #[arg(long, default_value="[::]:2038")]
    listen: SocketAddr,

    #[arg(long, default_value=".wsocks-server-secret-key")]
    key_file: String,

    #[arg(long)]
    http_path: Option<String>,
}

#[derive(Clone, Debug, clap::Parser)]
enum Opt {
    Client(ClientOpt),
    Server(ServerOpt)
}

/// a helper for convert from hex string to 256 bit key
fn hex2key(h: &str) -> anyhow::Result<[u8; 32]> {
    let mut k = [0u8; 32];
    hex::decode_to_slice(h, &mut k)?;
    Ok(k)
}

#[derive(Debug, Serialize, Deserialize)]
enum NetworkError {
    ServerFailure,
    ConnectionNotAllowed,
    NetworkUnreachable,
    HostUnreachable,
    ConnectionRefused,
    TtlExpired,
}

#[derive(Debug, Serialize, Deserialize)]
enum SessionError {
    SessionUnknown, // client sends TcpResume or TcpClose, but conn ID is not found.
    SessionTimeout, // conn ID exists, but inactive a long time, so reachs the server timeout.
    BrokenPipe, // proxied connection is closed by peer 
}


#[derive(Debug, Serialize, Deserialize)]
enum OfferError {
    UnableConnect(NetworkError),
    UnableResume(SessionError),
}

#[derive(Debug, Serialize, Deserialize)]
enum Protocol {
    // this frame is for hide metadata of packet length, so it's content is complete junk and can be safety ignored
    Padding {
        junk: Vec<u8>,
    },

    // client wants to open a new TCP connection
    TcpConnect {
        dst: String, // can be IP address or Domain name
        port: u16, // 0-65535
    },

    // client wants to "re-use" a exists TCP Connection
    TcpResume {
        id: u128,
    },

    // client wants to close a exists TCP connection
    TcpClose {
        id: u128,
    },

    // server offers the client request (one of TcpConnect, TcpResume, TcpClose).
    TcpOffer {
        id: Result<u128, OfferError>,
    },

    /*
    TcpBind {}
    UDPAssociate {}
    */
}

#[derive(Debug)]
struct Etag {
    id: u128,
    time: SystemTime,
    pk: MuxPublic,
}

impl Etag {
    fn new(pk: MuxPublic) -> Self {
        Self {
            id: fastrand::u128(..),
            time: SystemTime::now(),
            pk,
        }
    }
}

const NS_WSOCKS_CLIENT: &[u8; 32] = b"\xb2\xdf\xa0\x91\xde\xc2\x07\xbf\xce\x9e\x9f\x82\xb4\xf6\x85\x16\xe2\xb6\xae\xf8\xab\xa4\xa0\x90\x96\xd3\xf8\xfe\x02\xcb\xc5\xa8";

impl ToString for Etag {
    fn to_string(&self) -> String {
        let out = format!("{:?}", self);
        let out = blake3::keyed_hash(NS_WSOCKS_CLIENT, out.as_bytes());
        let out = &out.as_bytes()[0..16];
        let out = base64::encode(out);
        out
    }
}

async fn client(copt: ClientOpt) -> anyhow::Result<()> {
    let pubkey = MuxPublic::from_bytes(
        hex2key(&copt.remote_pubkey).expect("[hex::decode] cannot parse remote public key")
    );

    let etag = Etag::new(pubkey).to_string();

    let max_conn = copt.remote_max_websockets as usize;

    let mux = Arc::new(Multiplex::new(MuxSecret::generate(), Some(pubkey)));

    let pipemgr_fut = {
        let url = copt.remote_url.clone();
        let mux = mux.clone();
        async move {
            let mut pipe: ObfsWsPipe;
            loop {
                // remove any closed ws pipe
                mux.retain(|p| { p.peer_addr().len() > 0 });

                if mux.iter_pipes().collect::<Vec<_>>().len() > max_conn {
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

            anyhow::Ok(())
        }
    };

    let (req_tx, req_rx) = smol::channel::unbounded();
    let request_fut = {
        let mux = mux.clone();
        async move {
            loop {
                let (req, conn): (Protocol, TcpStream) = req_rx.recv().await.unwrap();
                let mux = mux.clone();
                smolscale::spawn(async move {
                    let req = bincode::serialize(&req).unwrap();
                    let req_len: [u8; 2] = (req.len() as u16).to_be_bytes(); // convert to big-endian

                    let mut relconn = mux.open_conn("").await.unwrap();

                    // send req
                    relconn.write(&req_len).await.unwrap();
                    relconn.write_all(&req).await.unwrap();

                    // recv resp
                    let res_len: usize = {
                        let mut len = [0u8; 2];
                        relconn.read_exact(&mut len).await.unwrap();
                        u16::from_be_bytes(len).into()
                    };
                    let res = {
                        let mut buf = b".".repeat(res_len);
                        relconn.read_exact(&mut buf).await.unwrap();
                        buf
                    };

                    let res: Protocol = bincode::deserialize(&res).unwrap();
                    match res {
                        Protocol::TcpOffer { id } => {
                            smol::future::race(
                                smol::io::copy(relconn.clone(), conn.clone()),
                                smol::io::copy(conn, relconn)
                            ).await.unwrap();
                        }
                        _ => { panic!("receive a unexpected frame type from wsocks server, only TcpOffer expected."); }
                    }
                }).detach();
            }
        }
    };

    let socks_fut = {
        let req_tx = req_tx.clone();
        let socks = TcpListener::bind(copt.local_socks_listen).await?;
        async move {
            let mut conn: TcpStream;
            let mut peer: SocketAddr;
            loop {
                (conn, peer) = socks.accept().await.unwrap();
                let req_tx = req_tx.clone();
                smolscale::spawn(async move {
                    let dst: String;
                    let port: u16;
                    match socksv5::read_version(&conn).await.unwrap() {
                        SocksVersion::V4 => {
                            let req = socksv5::v4::read_request_skip_version(&conn).await.unwrap();
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
                            let _ = socksv5::v5::read_handshake_skip_version(&conn).await.unwrap();
                            socksv5::v5::write_auth_method(&conn, SocksV5AuthMethod::Noauth).await.unwrap();

                            let req = socksv5::v5::read_request(&conn).await.unwrap();
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
                        dst,
                        port,
                    };

                    req_tx.send((req, conn)).await.unwrap();
                }).detach();
            }
        }
    };

    mux.add_drop_friend(smolscale::spawn(socks_fut));
    mux.add_drop_friend(smolscale::spawn(request_fut));

    pipemgr_fut.await
}

async fn server(sopt: ServerOpt) -> anyhow::Result<()> {
    let key_file = sopt.key_file.clone();
    let listen = sopt.listen.clone();
    let mut mux: HashMap<String, Arc<Multiplex>> = HashMap::new();

    let mut i = 0;
    let sk: MuxSecret = loop {
        // this is important for avoid infinity loop
        i += 1;
        if i >= 10 {
            panic!("Cannot get long-term secret key of server!");
        }

        match smol::fs::read(&key_file).await {
            Ok(data) => {
                if data.len() == 32 {
                    let mut key = [0u8; 32];
                    for i in 0..32 {
                        key[i] = data[i];
                    }
                    break MuxSecret::from_bytes(key);
                }

                anyhow::bail!("Key file {key_file:?} is corrupted! your server's long-term secret key probably lost. please check.");
            }
            Err(err) => {
                log::warn!("Cannot read your key file {key_file:?} (Error={err:?})... so generate new secret key, then save it into disk.");
                let sk = MuxSecret::generate();
                smol::fs::write(&key_file, sk.to_bytes()).await.expect("Cannot write new key to disk!?");
            }
        }
    };

    // ws pipe listener - future
    let wpl_fut = {
        let mux = mux.clone();

        // ws pipe listener
        let wpl = ObfsWsListener::bind(listen).await?;

        async move {
            loop {
                let pipe: Arc<dyn Pipe> = wpl.accept_pipe().await.unwrap();
            }
        }
    };

    todo!()
}

async fn async_main() -> anyhow::Result<()> {
    let opt = Opt::parse();
    println!("{:?}", opt);
    match opt {
        Opt::Client(copt) => {
            client(copt).await
        },
        Opt::Server(sopt) => {
            server(sopt).await
        }
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    smol::block_on(async_main())
}

