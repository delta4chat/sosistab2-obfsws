use sosistab2_obfsws::{listener, pipe};
use sosistab2::{Pipe, PipeListener, MuxSecret};
use std::time::Duration;
async fn obfsws_test_1() {
    println!("create server");

    let server_mux = sosistab2::Multiplex::new(MuxSecret::generate(), None);
//    let server_mux_1 = server_mux.clone();
    let server_fut = async move {
        let server_pipe = listener::ObfsWebsocketListener::bind("127.0.0.1:7070").await.unwrap();
        loop{
            let p = server_pipe.accept_pipe().await.unwrap();
            println!("accepted new incoming obfsws conn! {:?}", p.peer_addr());
            server_mux.add_pipe(p);

            let c = server_mux.accept_conn().await;
            println!("server accept new relconn");
            break c;
        }
    };

    smol::future::race(
        sosistab2_obfsws(),
        server_fut
    ).await.unwrap();
}

async fn sosistab2_obfsws() -> Result<sosistab2::Stream, std::io::Error> {

    smol::Timer::after(Duration::from_secs(3)).await;

    println!("create client");

    let client_pipe = pipe::ObfsWebsocketPipe::connect("ws://127.0.0.1:7070/abdgi", "metadata").await.unwrap();
    println!("{:?}", &client_pipe);



    let mux = sosistab2::Multiplex::new(MuxSecret::generate(), None);
    println!("client mux created!",);
    mux.add_pipe(client_pipe);
    let conn = mux.open_conn("client additional data").await.unwrap();
    conn.wait_connected().await.unwrap();
    println!("client relconn established");
    Ok(conn)
}

async fn async_main() {
    obfsws_test_1().await;
}

fn main() {
    smol::block_on(async_main());
}
