pub mod pipe;
pub mod listener;

pub use pipe::ObfsWsPipe;
pub use listener::ObfsWsListener;

pub mod ws {
    pub use async_tungstenite::{accept_hdr_async, WebSocketStream};
    pub use async_tungstenite::tungstenite::handshake::client::Request;
    pub use async_tungstenite::tungstenite::handshake::server::Response;
    pub use async_tungstenite::tungstenite::handshake::server::Callback;
    pub use async_tungstenite::async_std::{
        connect_async,
        ConnectStream,
    };
    pub use async_tungstenite::tungstenite::protocol::{Message};
    pub use async_tungstenite::stream::Stream;

    pub type WS = WebSocketStream<ConnectStream>;

}

pub use sosistab2::{Pipe, PipeListener};

