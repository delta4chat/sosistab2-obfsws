use crate::ws;
use crate::pipe::ObfsWebsocketPipe;

use smol::Task;
use smol::channel::{Sender, Receiver};
use async_std::net::{TcpStream, TcpListener};
use async_trait::async_trait;

use std::net::SocketAddr;
use async_std::net::ToSocketAddrs;
use std::sync::Arc;

use sosistab2::{Pipe, PipeListener};

pub struct ObfsWebsocketListener {
    pipe_rx: Receiver<ObfsWebsocketPipe>,
    _task: Task<anyhow::Result<()>>
}
impl ObfsWebsocketListener {
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
    pipe_tx: Sender<ObfsWebsocketPipe>,
    socket: TcpListener
) -> anyhow::Result<()> {
    let mut conn: TcpStream;
    let mut addr: SocketAddr;
    loop {
        (conn, addr) = socket.accept().await?;

        let pipe_tx = pipe_tx.clone();
        smolscale::spawn(async move {

            let ws_conn = ws::accept_async(ws::ConnectStream::Plain(conn)).await.unwrap();
            let pipe = ObfsWebsocketPipe::new(ws_conn, &format!("client({})", addr));
            pipe_tx.send(pipe).await.unwrap();
        }).detach();
    }
}

#[async_trait]
impl PipeListener for ObfsWebsocketListener {
    async fn accept_pipe(&self) -> std::io::Result<Arc<dyn Pipe>> {
        Ok(Arc::new(self.pipe_rx.recv().await.map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, e)
        })?))
    }
}
