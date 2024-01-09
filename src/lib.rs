pub mod pipe;
pub mod listener;

pub use pipe::ObfsWsPipe;
pub use listener::ObfsWsListener;

pub mod ws {
    pub use async_tungstenite::{accept_async, WebSocketStream};
    pub use async_tungstenite::async_std::{
        connect_async as connect,
        ConnectStream,
    };
    pub use async_tungstenite::tungstenite::protocol::{Message};
    pub use async_tungstenite::stream::Stream;

    pub type WS = WebSocketStream<ConnectStream>;

}

pub use sosistab2::{Pipe, PipeListener};

