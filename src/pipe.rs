use core::pin::Pin;
use core::task::Poll;

/*use std::net::SocketAddr;*/
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use bytes::Bytes;

use futures_util::{/*StreamExt, AsyncWriteExt, AsyncWrite,*/ sink::SinkExt};
use futures_util::stream::{SplitStream, SplitSink};

use crate::*;
use ws::WS;

use smol::prelude::*;
use smol::channel::{Sender, Receiver};

use async_std::sync::Arc;
use async_std::net::TcpStream;
//use async_lock::{Mutex, RwLock};

//type Inner = async_dup::Arc<async_dup::Mutex<WS>>;
#[derive(Debug, Clone)]
#[pin_project]
pub struct ObfsWsPipe {
    //inner: Inner,
    //inner_reader: Arc<RwLock<SplitStream<WS>>>,
    //inner_writer: SplitSink<WS, ws::Message>,
    inner_send_tx: Sender<Bytes>,
    inner_recv_rx: Receiver<Bytes>,

    #[pin]
    recv_buf: Vec<u8>,

    #[pin]
    closed: Arc<AtomicBool>,

    peer_url: String,
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
        Ok(Self::new(inner, peer_url, peer_metadata))
    }

    pub(crate) fn new(inner: WS, peer_url: impl ToString, peer_metadata: impl ToString) -> Self {
        //let inner = async_dup::Mutex::new(inner);
        //let inner = async_dup::Arc::new(inner);
    
        let (inner_send_tx, inner_send_rx) = smol::channel::bounded(100000);
        let (inner_recv_tx, inner_recv_rx) = smol::channel::bounded(100000);

        use futures_util::StreamExt;
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

        let mut peer_url = peer_url.to_string();
        peer_url.push('#');
        peer_url.extend(peer_metadata.to_string().chars());

        Self {
            closed,

            //inner: inner.clone(),
            //inner_reader: Arc::new(RwLock::new(inner_reader)),
            //inner_writer,

            inner_send_tx,
            inner_recv_rx,

            recv_buf: Vec::new(),

            peer_url,
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
    while ! closed.load(Relaxed) {
        let msg: Bytes = inner_send_rx.recv().await?;
        log::trace!("websocket sending new message: {:?}", &msg);

        let result = inner_writer.send( ws::Message::binary(msg) ).await;
        if result.is_err() {
            log::debug!("set closed atomic bool");
            closed.store(true, Relaxed);
            break;
        }
        result?;
    }

    Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into())
}
async fn recv_loop(
    inner_recv_tx: Sender<Bytes>,
    mut inner_reader: SplitStream<WS>,
    closed: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    while ! closed.load(Relaxed) {
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

impl AsyncWrite for ObfsWsPipe {
    fn poll_write(
        self: Pin<&mut Self>,
        _ctx: &mut core::task::Context<'_>,
        msg: &[u8]
    ) -> Poll<Result<usize, std::io::Error>> {
        let this = self.project();
        if this.closed.load(Relaxed) {
            return Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()));
        }

        let msg_len = msg.len();
        let ret = this.inner_send_tx.try_send(msg.to_vec().into());
        if ret.is_ok() {
            log::trace!("sent {} bytes via ws: {:?}", msg_len, ret);
        } else {
            log::warn!("unable to send {} bytes via ws: maybe `smol::channel::bounded` reach max size (100000) ? Error= {:?}", msg_len, ret);
        }

        Poll::Ready(Ok(msg_len))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _ctx: &mut core::task::Context<'_>
    ) -> Poll<Result<(), std::io::Error>> {
        let this = self.project();
        Poll::Ready(
            if this.closed.load(Relaxed) {
                Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into())
            } else {
                Ok(())
            }
        )
    }

    fn poll_close(
        self: Pin<&mut Self>,
        _ctx: &mut std::task::Context<'_>
    ) -> Poll<Result<(), std::io::Error>> {
        let this = self.project();
        this.closed.store(true, Relaxed);
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for ObfsWsPipe {
    fn poll_read(
        self: Pin<&mut Self>,
        _ctx: &mut core::task::Context<'_>,
        buf: &mut [u8]
    ) -> Poll<Result<usize, std::io::Error>> {
        let mut this = self.project();

        let buf_len = buf.len();
        let mut buf_pos = 0;

        {
            let recv_buf_len = this.recv_buf.len();
            if recv_buf_len > 0 {
                let buf_remaining = buf_len - buf_pos;
                if recv_buf_len > buf_remaining {
                    buf.copy_from_slice(&this.recv_buf[..buf_len]);
                    *this.recv_buf = this.recv_buf[buf_len..].to_vec();
                    return Poll::Ready(Ok(buf_len));
                } else {
                    buf[..recv_buf_len].copy_from_slice(&this.recv_buf);
                    this.recv_buf.clear();

                    buf_pos += recv_buf_len;
                }
            }
        }

        let buf2 =
            match this.inner_recv_rx.try_recv() {
                Ok(v) => v,
                _ => {
                    return Poll::Pending;
                }
            };
        let buf2_len = buf2.len();

        let buf_remaining = buf_len - buf_pos;
        if buf2_len > buf_remaining {
            buf[buf_pos..].copy_from_slice(&buf2[..buf_remaining]);
            this.recv_buf.extend(&buf2[buf_remaining..]);

            buf_pos += buf_remaining;
        } else {
            buf[buf_pos..buf_pos+buf2_len].copy_from_slice(&buf2);

            buf_pos += buf2_len;
        }

        Poll::Ready(Ok(buf_pos))
    }
}

impl Pipe for ObfsWsPipe {
    fn protocol(&self) -> &str {
        "sosistab3-obfsws"
    }

    fn remote_addr(&self) -> Option<&str> {
        if self.is_closed() {
            return None;
        }

        Some(&self.peer_url)
    }
}


