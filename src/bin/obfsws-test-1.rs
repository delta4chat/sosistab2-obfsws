use sosistab2_obfsws::{listener, pipe};
use sosistab2::{Pipe, PipeListener, MuxSecret};
use std::time::{Duration, Instant};

use futures_util::io::{AsyncReadExt, AsyncWriteExt};

use smol_timeout::TimeoutExt;

async fn obfsws_test_1() {
    let server_fut = async {
        println!("create server");
        let server_mux = sosistab2::Multiplex::new(MuxSecret::generate(), None);
        let server_pipe = listener::ObfsWebsocketListener::bind("127.0.0.1:7070").await.unwrap();
        loop{
            let p = server_pipe.accept_pipe().await.unwrap();
            println!("accepted new incoming obfsws conn! {:?}", p.peer_addr());
            server_mux.add_pipe(p);

            let mut c = server_mux.accept_conn().await.unwrap();
            println!("server accept new relconn");

            smolscale::spawn(async move {
                /*
                let mut buf = b".".repeat(65535);
                loop {
                    c.read_exact(&mut buf).await.unwrap();
                    c.write_all(&buf).await.unwrap();
                }*/
                smol::io::copy(c.clone(), c).await.unwrap();
            }).detach();
        }

    };

    let client_fut = async {
        smol::Timer::after(Duration::from_secs(1)).await;

        println!("create client");
        let client_pipe = pipe::ObfsWebsocketPipe::connect("ws://127.0.0.1:7070/abdgi", "metadata").await.unwrap();
        println!("{:?}", &client_pipe);



        let mux = sosistab2::Multiplex::new(MuxSecret::generate(), None);
        println!("client mux created!",);
        mux.add_pipe(client_pipe);
        let mut conn = mux.open_conn("client additional data").await.unwrap();
        println!("opening relconn");
        conn.wait_connected().await.unwrap();
        println!("client relconn established");

        let msg = b"TEST MESSAGE".repeat(50000);
        let msg_len = msg.len();

        let mut conn2 = conn.clone();

        let (allow_send_notify, allow_send) = smol::channel::bounded(1);
        let recv_fut = async move {
            let mut total_recv_bytes = 0u128;
            let mut recv_bytes = 0u128;
            let start_time = Instant::now();
            let mut time_anchor = start_time;
            let interval = Duration::from_secs(2);

            let mut recv_buf = b".".repeat(msg_len*2);

            let b = (1.0, "byte");
            let kb = (1024.0, "KiB");
            let mb = (kb.0*1024.0, "MiB");
            let gb = (mb.0*1024.0, "GiB");

            let mut unit = kb;
            let mut total_speed;
            let mut speed;

            let mut ticks = 0u128;
            let mut recvs = 0u128;
            loop {
                ticks += 1; // add for each iteration

                if time_anchor.elapsed() >= interval {
                    total_speed = (total_recv_bytes as f64) / unit.0 / start_time.elapsed().as_secs_f64();
                    speed = (recv_bytes as f64) / unit.0 / time_anchor.elapsed().as_secs_f64();
                    {
                        let unit = unit.1;
                        let rate = if recvs > 0 {
                            let ticks = ticks as f64;
                            let recvs = recvs as f64;
                            100.0/(ticks/recvs)
                        } else {
                            0.0
                        };

                        println!("NO.{ticks}: Ok({rate:.2}%) Download speed: slice begin = {total_speed:.3} {unit}/s | slice last epoch = {speed:.3} {unit}/s");

                        if rate < 90.0 {
                            log::error!("Ok rate too low");
                            break;
                        }
                    }

                    if speed > 2000.0 {
                        unit = match unit {
                            (_, "byte") => kb,
                            (_, "KiB") => mb,
                            (_, "MiB") => gb,
                            (_, "GiB") => gb,
                            _ => { panic!("impossible") }
                        };
                    } else if speed < 1.0 {
                        unit = match unit {
                            (_, "byte") => b,
                            (_, "KiB") => b,
                            (_, "MiB") => kb,
                            (_, "GiB") => mb,
                            _ => { panic!("impossible") }
                        }
                    }

                    recv_bytes = 0;
                    time_anchor = Instant::now();
                }

                if let Some(ret) = conn2.read(&mut recv_buf).timeout(interval).await {
                    recvs += 1; // add on recv only

                    let size = ret.unwrap();
                    recv_bytes += (size as u128);
                    total_recv_bytes += (size as u128);
                    allow_send_notify.try_send(());
                } else {
                    log::warn!("Read relconn timed out");
                }
            }
        };
        let send_fut = async move {
            //let mut t;
            loop {
                //t = Instant::now();
                conn.write_all(&msg).await.unwrap();
                allow_send.recv().await;
                //println!("{:?}", t.elapsed());
            }
        };

        smol::future::race(recv_fut, send_fut).await;
    };

    smol::future::race(
        server_fut,
        client_fut
    ).await;
}

async fn async_main() {
    obfsws_test_1().await;
}

fn main() {
    env_logger::init();
    smol::block_on(async_main());
}
