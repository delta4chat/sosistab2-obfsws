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

pub struct ObfsWsListener {
    pipe_rx: Receiver<ObfsWsPipe>,
    _task: Task<anyhow::Result<()>>
}
impl ObfsWsListener {
    pub async fn bind(addr: impl ToSocketAddrs) -> anyhow::Result<Self> {
        let sock = TcpListener::bind(addr).await?;
        Ok(Self::from(sock))
    }

    pub fn from(socket: TcpListener) -> Self {
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

        let pipe_tx = pipe_tx.clone();
        smolscale::spawn(async move {
            let (md_tx, md_rx) = smol::channel::bounded(1);

            let ws_conn = ws::accept_hdr_async(ws::ConnectStream::Plain(conn), pipe_get_metadata(md_tx)).await.unwrap();
            let metadata = md_rx.recv().await.unwrap();
            let pipe = ObfsWsPipe::new(ws_conn, &metadata);
            pipe_tx.send(pipe).await.unwrap();
        }).detach();
    }
}

fn pipe_get_metadata(md_tx: Sender<String>) -> impl ws::Callback {
    move |req: &ws::Request, res: ws::Response| {
        if let Some(etag) = req.headers().get("If-Match") {
            md_tx.try_send(etag.to_str().unwrap().to_string()).unwrap();
        }

        Ok(res)
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
