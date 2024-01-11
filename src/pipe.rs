/*use std::net::SocketAddr;*/
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use bytes::Bytes;

use async_trait::async_trait;
use futures_util::{StreamExt, /*AsyncWriteExt, AsyncWrite,*/ sink::SinkExt};
use futures_util::stream::{SplitStream, SplitSink};

use crate::ws;
use ws::WS;

use smol::channel::{Sender, Receiver};

use async_std::sync::Arc;
use async_lock::Mutex;

//type Inner = async_dup::Arc<async_dup::Mutex<WS>>;
#[derive(Debug, Clone)]
pub struct ObfsWsPipe {
    //inner: Inner,
    inner_reader: Arc<Mutex<SplitStream<WS>>>,
    //inner_writer: SplitSink<WS, ws::Message>,
    inner_send_tx: Sender<Bytes>,

    closed: Arc<AtomicBool>,

    peer_url: Option<String>,
    peer_metadata: String,
}

impl ObfsWsPipe {
    pub async fn connect(peer_url: &str, peer_metadata: impl ToString) -> anyhow::Result<Self> {
        let peer_metadata: String = peer_metadata.to_string();

        let peer_url: ws::Uri = peer_url.parse()?;
        let mut host = peer_url.host().unwrap().to_string();
        if let Some(port) = peer_url.port_u16() {
            host.push(':');
            host.push_str(&port.to_string());
        }
        let req =
            ws::Request::builder()
            .method("GET")
            .uri(peer_url.clone())
            .header("Host", host)
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-Websocket-Version", "13")
            .header("Sec-Websocket-Key", &peer_metadata)
            .body(())
            .unwrap();
        let (inner, resp) = ws::connect_async(req).await?;
        log::trace!("ws pipe connected: inner={inner:?} | resp={resp:?}");
        let mut this = Self::new(inner, &peer_metadata);
        this.peer_url = Some(peer_url.to_string());
        Ok(this)
    }

    pub(crate) fn new(inner: WS, peer_metadata: &str) -> Self {
        //let inner = async_dup::Mutex::new(inner);
        //let inner = async_dup::Arc::new(inner);
    
        let (inner_send_tx, inner_send_rx) = smol::channel::bounded(1000);
        let (inner_writer, inner_reader) = inner.split();
        let closed = Arc::new(AtomicBool::new(false));

        // background task for sending pkts
        smolscale::spawn(
            send_loop(
                inner_send_rx,
                inner_writer,
                closed.clone()
            )
        ).detach();

        Self {
            closed,

            //inner: inner.clone(),
            inner_reader: Arc::new(Mutex::new(inner_reader)),
            //inner_writer,
            inner_send_tx,
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
            closed.store(true, Relaxed);
        }
        result.unwrap();
    }
}

#[async_trait]
impl sosistab2::Pipe for ObfsWsPipe {
    fn send(&self, msg: Bytes) {
        if self.is_closed() {
            std::io::Result::<()>::Err(std::io::ErrorKind::BrokenPipe.into()).expect("Try send to a closed ObfsWsPipe!");
        }

        let msg_len = msg.len();
        if msg_len < 65536 {
            let ret = self.inner_send_tx.try_send(msg);
            if ret.is_ok() {
                log::trace!("sent {} bytes via ws: {:?}", msg_len, ret);
            } else {
                log::warn!("unable to send {} bytes via ws: maybe `smol::channel::bounded` reach max size (1000) ? Error= {:?}", msg_len, ret);
            }
        } else {
            log::warn!("Websocket Message too big (len={})", msg_len);
        }
    }

    async fn recv(&self) -> std::io::Result<Bytes> {
        let ret = self.inner_reader.lock().await.next().await;
        if let Some(ret) = ret {
            if let Ok(ret) = ret {
                match ret {
                    ws::Message::Binary(msg) => {
                        log::trace!("from ws stream recved (before 256 bytes) {:?}", if msg.len() < 256 { &msg } else { &msg[..256] });
                        return Ok(msg.into());
                    },

                    _ => {
                        log::warn!("Unexpected Websocket Message type received!!! {:?}", ret);
                    }
                }
            }
        }

        self.closed.store(true, Relaxed);
        Err(std::io::ErrorKind::BrokenPipe.into())
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
            s.push_str(&url);
        } else {
            s.push_str(&self.peer_metadata);
        }

        s
    }

    fn peer_metadata(&self) -> &str {
        &self.peer_metadata
    }
}


