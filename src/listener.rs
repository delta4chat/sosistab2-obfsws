use crate::ws;
use crate::pipe::ObfsWsPipe;

use smol::Task;
use smol::channel::{Sender, Receiver};
use async_std::net::{TcpStream, TcpListener};
use async_trait::async_trait;

use std::net::SocketAddr;
use async_std::net::ToSocketAddrs;
use std::sync::Arc;

use sosistab2::{Pipe, PipeListener};

use base64::Engine;
use base64::prelude::BASE64_STANDARD;

pub struct ObfsWsListener {
    pipe_rx: Receiver<ObfsWsPipe>,
    _task: Task<anyhow::Result<()>>
}

impl ObfsWsListener {
    pub async fn bind(addr: impl ToSocketAddrs) -> anyhow::Result<Self> {
        let sock = TcpListener::bind(addr).await?;
        Ok(Self::from(sock))
    }
}
impl From<TcpListener> for ObfsWsListener {
    fn from(socket: TcpListener) -> Self {
        let (pipe_tx, pipe_rx) = smol::channel::bounded(1000);
        Self {
            pipe_rx,
            _task: smolscale::spawn(pipe_accept_loop(pipe_tx, socket))
        }
    }
}

async fn pipe_accept_loop(
    pipe_tx: Sender<ObfsWsPipe>,
    socket: TcpListener
) -> anyhow::Result<()> {
    let mut conn: TcpStream;
    let mut addr: SocketAddr;

    loop {
        (conn, addr) = socket.accept().await?;
        log::trace!("ws pipe listener accepted raw TCP connection from {addr:?}");

        let pipe_tx = pipe_tx.clone();
        smolscale::spawn(async move {
            let (md_tx, md_rx) = smol::channel::bounded(1);
            conn.set_nodelay(true)?;

            let ws_conn =
                ws::accept_hdr_async_with_config(
                    ws::ConnectStream::Plain(conn),
                    pipe_get_metadata(md_tx),
                    Some(ws::CONFIG),
                ).await?;
            let metadata = md_rx.recv().await?;
            let pipe = ObfsWsPipe::new(ws_conn, &metadata);
            pipe_tx.send(pipe).await?;
            Ok::<_, anyhow::Error>(())
        }).detach();
    }
}

fn pipe_get_metadata(md_tx: Sender<String>) -> impl ws::Callback {
    move |req: &ws::Request, res: ws::Response| {
        let res400 = {
            let mut r = ws::ErrorResponse::new(None);
            *r.status_mut() = 400.try_into().unwrap();
            let h = r.headers_mut();
            h.insert("Connection", "close".try_into().unwrap());
            r
        };

        if let Some(md) = req.headers().get("Sec-Websocket-Key") {
            if let Ok(v) = BASE64_STANDARD.decode(md) {
                if v.len() == 16 {
                    md_tx.try_send(md.to_str().unwrap().to_string()).unwrap();
                    return Ok(res);
                }
            }
        }

        Err(res400)
    }
}

#[async_trait]
impl PipeListener for ObfsWsListener {
    async fn accept_pipe(&self) -> std::io::Result<Arc<dyn Pipe>> {
        Ok(Arc::new(self.pipe_rx.recv().await.map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, e)
        })?))
    }
}
