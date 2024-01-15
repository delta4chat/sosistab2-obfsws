pub mod pipe;
pub mod listener;

pub use pipe::ObfsWsPipe;
pub use listener::ObfsWsListener;

pub mod ws {
    pub use async_tungstenite::{accept_hdr_async_with_config, WebSocketStream};
    pub use async_tungstenite::tungstenite::http::Uri;
    pub use async_tungstenite::tungstenite::handshake::client::Request;
    pub use async_tungstenite::tungstenite::handshake::server::{Response, ErrorResponse, Callback};
    pub use async_tungstenite::async_std::{
        //connect_async_with_config,
        client_async_tls_with_connector_and_config,
        ConnectStream,
    };
    pub use async_tungstenite::tungstenite::protocol::{WebSocketConfig, Message};
    pub use async_tungstenite::stream::Stream;

    pub type WS = WebSocketStream<ConnectStream>;

    #[allow(deprecated)]
    pub const CONFIG: WebSocketConfig =
        WebSocketConfig {
            // 1) has been deprecated by the author of that websocket library.
            // 2) But still needed, otherwise rustc compilation will fail (error[E0063]: missing struct field).
            // 3) it's really a clumsy and useless design by Rust team.
            max_send_queue: None,

            // disable buffer
            write_buffer_size: 0,
            max_write_buffer_size: usize::MAX,

            // cancel the message limit
            max_message_size: None,
            max_frame_size: None,

            // accept client message that is unmasked
            accept_unmasked_frames: true,
        };
}


pub use sosistab2::{Pipe, PipeListener};
