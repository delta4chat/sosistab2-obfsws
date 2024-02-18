/*use std::net::SocketAddr;*/
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use bytes::Bytes;

use anyhow::Context;

use async_trait::async_trait;
use futures_util::{StreamExt, /*AsyncWriteExt, AsyncWrite,*/ sink::SinkExt};
use futures_util::stream::{SplitStream, SplitSink};

use crate::ws;
use ws::WS;

use smol::channel::{Sender, Receiver};

use async_std::sync::Arc;
use async_std::net::TcpStream;
//use async_lock::{Mutex, RwLock};

//type Inner = async_dup::Arc<async_dup::Mutex<WS>>;
#[derive(Debug, Clone)]
pub struct ObfsWsPipe {
    //inner: Inner,
    //inner_reader: Arc<RwLock<SplitStream<WS>>>,
    //inner_writer: SplitSink<WS, ws::Message>,
    inner_send_tx: Sender<Bytes>,
    inner_recv_rx: Receiver<Bytes>,

    closed: Arc<AtomicBool>,

    peer_url: Option<String>,
    peer_metadata: String,
}

impl ObfsWsPipe {
    pub async fn connect(peer_url: impl ToString, peer_metadata: impl ToString) -> anyhow::Result<Self> {
        let peer_url: String = peer_url.to_string();
        let peer_metadata: String = peer_metadata.to_string();

        let peer_url: ws::Uri = peer_url.parse()?;
        let mut host = peer_url.host().ok_or(anyhow::Error::msg("cannot parse host:port pair in provided URL({peer_url:?})!"))?.to_string();

        let mut port: Option<u16> = peer_url.port_u16();
        if port.is_none() {
            match peer_url.scheme_str() {
                Some("ws") => {
                    port = Some(80)
                },
                Some("wss") => {
                    port = Some(443)
                },
                _ => {}
            }
        }
        if let Some(port) = port {
            host.push(':');
            host.push_str(&port.to_string());
        }

        let req =
            ws::Request::builder()
            .method("GET")
            .uri(peer_url.clone())
            .header("Host", &host)
            .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-Websocket-Version", "13")
            .header("Sec-Websocket-Key", &peer_metadata)
            .body(())?;

        let socket = TcpStream::connect(host).await?;
        socket.set_nodelay(true)?;
        let (inner, resp) = ws::client_async_tls_with_connector_and_config(
            req,
            socket,
            None,
            Some(ws::CONFIG)
        ).await?;
        log::trace!("ws pipe connected: inner={inner:?} | resp={resp:?}");
        let mut this = Self::new(inner, peer_metadata);
        this.peer_url = Some(peer_url.to_string());
        Ok(this)
    }

    pub(crate) fn new(inner: WS, peer_metadata: impl ToString) -> Self {
        //let inner = async_dup::Mutex::new(inner);
        //let inner = async_dup::Arc::new(inner);
    
        let (inner_send_tx, inner_send_rx) = smol::channel::bounded(100000);
        let (inner_recv_tx, inner_recv_rx) = smol::channel::bounded(100000);
        let (inner_writer, inner_reader) = inner.split();
        let closed = Arc::new(AtomicBool::new(false));

        // background task for sending pkts
        smolscale2::spawn(
            smol::future::race(
                send_loop(
                    inner_send_rx,
                    inner_writer,
                    closed.clone()
                ),
                recv_loop(
                    inner_recv_tx,
                    inner_reader,
                    closed.clone()
                )
            )
        ).detach();

        Self {
            closed,

            //inner: inner.clone(),
            //inner_reader: Arc::new(RwLock::new(inner_reader)),
            //inner_writer,
            inner_send_tx,
            inner_recv_rx,
            peer_url: None,
            peer_metadata: peer_metadata.to_string(),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Relaxed)
    }
}
async fn send_loop(
    inner_send_rx: Receiver<Bytes>,
    mut inner_writer: SplitSink<WS, ws::Message>,
    closed: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    loop {
        let msg: Bytes = inner_send_rx.recv().await?;
        log::trace!("ws(plain) sending new message: {:?}", &msg);

        let result = inner_writer.send( ws::Message::binary(msg) ).await;
        if result.is_err() {
            log::debug!("set closed atomic bool");
            closed.store(true, Relaxed);
        }
        result?;
    }
}
async fn recv_loop(
    inner_recv_tx: Sender<Bytes>,
    mut inner_reader: SplitStream<WS>,
    closed: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    loop {
        let ret = inner_reader.next().await;
        if let Some(ret) = ret {
            if let Ok(ret) = ret {
                match ret {
                    ws::Message::Binary(msg) => {
                        log::trace!("from ws stream recved (before 256 bytes) {:?}", if msg.len() < 256 { &msg } else { &msg[..256] });
                        inner_recv_tx.send(msg.into()).await?;
                    },

                    _ => {
                        log::warn!("Unexpected Websocket Message type received!!! {:?}", ret);
                    }
                }
            }
        } else {
            log::debug!("set closed atomic bool");
            closed.store(true, Relaxed);
            break;
        }
    }

    Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into())
}

#[async_trait]
impl sosistab2::Pipe for ObfsWsPipe {
    fn send(&self, msg: Bytes) {
        if self.is_closed() {
            let err = std::io::Result::<()>::Err(std::io::ErrorKind::BrokenPipe.into()).context("Try send to a closed ObfsWsPipe!");
            log::debug!("{err:?}");
            return;
        }

        let msg_len = msg.len();
        if msg_len < 65536 {
            let ret = self.inner_send_tx.try_send(msg);
            if ret.is_ok() {
                log::trace!("sent {} bytes via ws: {:?}", msg_len, ret);
            } else {
                log::warn!("unable to send {} bytes via ws: maybe `smol::channel::bounded` reach max size (100000) ? Error= {:?}", msg_len, ret);
            }
        } else {
            log::warn!("Websocket Message too big (len={})", msg_len);
        }
    }

    async fn recv(&self) -> std::io::Result<Bytes> {
        match self.inner_recv_rx.recv().await {
            Ok(v) => Ok(v),
            Err(e) => {
                log::debug!("cannot recv from channel: {e:?}");
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
    }

    fn protocol(&self) -> &str {
        "obfsws-1"
    }

    fn peer_addr(&self) -> String {
        let mut s = "#".to_string();
        if self.is_closed() {
            s.clear();
            return s;
        }

        if let Some(url) = &self.peer_url {
            s.push_str(url);
        } else {
            s.push_str(&self.peer_metadata);
        }

        s
    }

    fn peer_metadata(&self) -> &str {
        &self.peer_metadata
    }
}


