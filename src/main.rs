use bytes::BytesMut;
use duplex::Shuttle;
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::codec::{Decoder, Encoder};

const MAX: usize = 8 * 1024 * 1024;
const ADDR: &str = "127.0.0.1:34254";

mod duplex;
mod proto;

async fn echo_worker(mut receiver: Receiver<String>, sender: Sender<String>) {
    println!("echo: waiting");
    while let Some(msg) = receiver.recv().await {
        println!("echo: got message {}", msg);
        sender.send(msg).await.expect("disconnected");
        println!("echo: sent");
    }
}

async fn server() -> io::Result<()> {
    let listener = TcpListener::bind(ADDR).await?;

    let (stream, _) = listener.accept().await?;

    let (send_tx, send_rx) = tokio::sync::mpsc::channel::<String>(10);
    let (reply_tx, reply_rx) = tokio::sync::mpsc::channel::<String>(10);

    tokio::spawn(echo_worker(send_rx, reply_tx));

    let codec = proto::MyStringCodec {};

    Shuttle::new(stream, codec, send_tx, reply_rx)
        .await
        .expect("failed");

    Ok(())
}

async fn client() -> io::Result<()> {
    let mut stream = TcpStream::connect(ADDR).await?;

    let mut codec = proto::MyStringCodec {};
    let mut buf = BytesMut::new();

    // ping pong
    for i in 0..2 {
        let msg = format!("Hello {}", i);
        println!("sending: {}", msg);
        codec
            .encode(format!("Hello {}", i), &mut buf)
            .expect("encode failed");
        stream.write_all_buf(&mut buf).await?;
        buf.clear();
        loop {
            // The read_buf call will append to buf rather than overwrite existing data.
            let len = stream.read_buf(&mut buf).await?;

            if len == 0 {
                while let Some(frame) = codec.decode_eof(&mut buf)? {
                    println!("received: {}", frame);
                }
                break;
            }

            while let Some(frame) = codec.decode(&mut buf)? {
                println!("received: {}", frame);
            }
        }
        buf.clear()
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    let server_jh = tokio::spawn(server());
    let client_jh = tokio::spawn(client());

    let (server_result, client_result) =
        futures::try_join!(server_jh, client_jh).expect("join error");

    server_result.expect("server failed");
    client_result.expect("client failed");
}
