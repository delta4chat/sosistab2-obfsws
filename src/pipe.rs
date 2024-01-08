use std::net::SocketAddr;
use bytes::Bytes;

use async_trait::async_trait;
use futures_util::{StreamExt, AsyncWriteExt, AsyncWrite, sink::SinkExt};
use futures_util::stream::{SplitStream, SplitSink};

use crate::ws;
use ws::WS;

use smol::channel::{Sender, Receiver};

use async_dup::{Arc, Mutex};

//type Inner = async_dup::Arc<async_dup::Mutex<WS>>;
#[derive(Debug)]
pub struct ObfsWebsocketPipe {
    //inner: Inner,
    inner_reader: Arc<Mutex<SplitStream<WS>>>,
    //inner_writer: SplitSink<WS, ws::Message>,
    inner_send_tx: Sender<Bytes>,

    peer_url: Option<String>,
    peer_metadata: String,

    _task: smol::Task<anyhow::Result<()>>,
}

impl ObfsWebsocketPipe {
    pub async fn connect(peer_url: &str, peer_metadata: &str) -> anyhow::Result<Self> {
        let (inner, _) = ws::connect(peer_url).await?;
        let mut this = Self::new(inner, peer_metadata);
        this.peer_url = Some(peer_url.to_string());
        Ok(this)
    }

    pub(crate) fn new(inner: WS, peer_metadata: &str) -> Self {
        //let inner = async_dup::Mutex::new(inner);
        //let inner = async_dup::Arc::new(inner);
    
        let (inner_send_tx, inner_send_rx) = smol::channel::unbounded();
        let (inner_writer, inner_reader) = inner.split();
        Self {
            //inner: inner.clone(),
            inner_reader: Arc::new(Mutex::new(inner_reader)),
            //inner_writer,
            inner_send_tx,
            peer_url: None,
            peer_metadata: peer_metadata.to_string(),
            _task: smolscale::spawn(send_loop(inner_send_rx, inner_writer)),
        }
    }
}
async fn send_loop(
    inner_send_rx: Receiver<Bytes>,
    mut inner_writer: SplitSink<WS, ws::Message>,
) -> anyhow::Result<()> {
    loop {
        let msg: Bytes = inner_send_rx.recv().await.unwrap();
        log::trace!("ws(plain) sending new message: {:?}", &msg);

        inner_writer.send( ws::Message::binary(msg) ).await.unwrap();
    }
}

#[async_trait]
impl sosistab2::Pipe for ObfsWebsocketPipe {
    fn send(&self, msg: Bytes) {
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
        let ret = self.inner_reader.lock().next().await;
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
        Err(std::io::ErrorKind::BrokenPipe.into())
    }

    fn protocol(&self) -> &str {
        "obfsws-1"
    }

    fn peer_addr(&self) -> String {
        if let Some(url) = &self.peer_url {
            url.clone()
        } else {
            self.peer_metadata.to_string()
        }
    }

    fn peer_metadata(&self) -> &str {
        &self.peer_metadata
    }
}


