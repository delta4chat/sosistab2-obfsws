// mux
use sosistab2::Multiplex;

// pipe
use sosistab2::{Pipe, PipeListener};
use sosistab2_obfsws::{ObfsWsPipe, ObfsWsListener};

// types
use std::net::{SocketAddr, Ipv4Addr, Ipv6Addr};
use serde::{Serialize, Deserialize};
use sosistab2::{MuxSecret, MuxPublic};

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
use std::time::Duration;

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

enum NetworkError {
        ServerFailure,
        ConnectionNotAllowed,
        NetworkUnreachable,
        HostUnreachable,
        ConnectionRefused,
        TtlExpired,
        CommandNotSupported,
        AddrtypeNotSupported,
}

enum SessionError {
    SessionUnknown, // client sends TcpResume or TcpClose, but conn ID is not found.
    SessionTimeout, // conn ID exists, but inactive a long time, so reachs the server timeout.
    BrokenPipe, // proxied connection is closed by peer 
}


#[derive(Debug, Serialize, Deserialize)]
enum OfferError {
    NetworkError,
    SessionError,
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
}

async fn client(copt: ClientOpt) -> anyhow::Result<()> {
    let pubkey = MuxPublic::from_bytes(
        hex2key(&copt.remote_pubkey).expect("[hex::decode] cannot parse remote public key")
    );

    let max_conn = copt.remote_max_websockets as usize;

    let mux = Arc::new(Mutex::new(Multiplex::new(MuxSecret::generate(), Some(pubkey))));

    let pipemgr_fut = {
        let mux = mux.clone();
        async move {
            let mut pipe: ObfsWsPipe;
            loop {
                let mux = mux.lock().await;

                // remove any closed ws pipe
                mux.retain(|p| { p.peer_addr().len() > 0 });

                if mux.iter_pipes().collect::<Vec<_>>().len() > max_conn {
                    std::mem::drop(mux);
                    smol::Timer::after(Duration::from_secs(5)).await;
                    continue;
                }

                pipe = ObfsWsPipe::connect(&copt.remote_url, "").await?;
                mux.add_pipe(pipe);

                std::mem::drop(mux);
            }

            anyhow::Ok(())
        }
    };

    let (req_tx, req_rx) = smol::channel::unbounded();
    let request_fut = async move {
        loop {
            let (req, conn) = req_rx.recv().await.unwrap();
            println!("{:?}", req);
            println!("{:?}", conn);
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

    mux.lock().await.add_drop_friend(smolscale::spawn(socks_fut));
    mux.lock().await.add_drop_friend(smolscale::spawn(request_fut));

    pipemgr_fut.await
}

async fn server(sopt: ServerOpt) -> anyhow::Result<()> {
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
    smol::block_on(async_main())
}

