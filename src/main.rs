use bytes::BytesMut;
use duplex::Shuttle;
use tokio::{
    io::{self, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc::{Receiver, Sender},
};
use tokio_util::codec::{Decoder, Encoder};
use tracing::Instrument;

const ADDR: &str = "127.0.0.1:34254";

mod duplex;
mod proto;

#[tracing::instrument(skip_all)]
async fn echo_worker(mut receiver: Receiver<String>, sender: Sender<String>) {
    tracing::info!("waiting");
    while let Some(msg) = receiver.recv().await {
        tracing::info!("got message {msg}");
        sender.send(msg).await.expect("disconnected");
        tracing::info!("sent");
    }
}

#[tracing::instrument(skip_all)]
async fn server() -> io::Result<()> {
    let listener = TcpListener::bind(ADDR).await?;
    let (stream, _) = listener.accept().await?;

    let (send_tx, send_rx) = tokio::sync::mpsc::channel::<String>(2);
    let (reply_tx, reply_rx) = tokio::sync::mpsc::channel::<String>(2);

    tokio::spawn(echo_worker(send_rx, reply_tx).in_current_span());

    let codec = proto::MyStringCodec {};
    Shuttle::new(stream, codec, send_tx, reply_rx)
        .await
        .expect("failed");

    Ok(())
}

async fn send(
    msg: String,
    codec: &mut proto::MyStringCodec,
    mut buf: &mut BytesMut,
    stream: &mut TcpStream,
) -> io::Result<()> {
    codec.encode(msg.clone(), &mut buf).expect("encode failed");

    tracing::info!(">> writing: {msg}");
    stream.write_all_buf(&mut buf).await?;
    tracing::info!("<< written: {msg}");

    Ok(())
}

async fn recv(
    codec: &mut proto::MyStringCodec,
    mut buf: &mut BytesMut,
    stream: &mut TcpStream,
) -> io::Result<()> {
    loop {
        // The read_buf call will append to buf rather than overwrite existing data.
        tracing::info!(">> reading");
        let len = stream.read_buf(&mut buf).await?;
        tracing::info!("<< read");
        if len == 0 {
            while let Some(frame) = codec.decode_eof(&mut buf)? {
                tracing::info!("received: {frame}");
            }
            break;
        }

        while let Some(frame) = codec.decode(&mut buf)? {
            tracing::info!("received: {frame}");
        }
        break;
    }
    Ok(())
}

#[tracing::instrument]
async fn client() -> io::Result<()> {
    let mut stream = TcpStream::connect(ADDR).await?;

    let mut codec = proto::MyStringCodec {};
    let mut buf = BytesMut::new();

    // ping pong
    for i in 0..2 {
        let msg = format!("ping pong {i}");
        send(msg, &mut codec, &mut buf, &mut stream).await?;

        buf.clear();
        recv(&mut codec, &mut buf, &mut stream).await?;
        buf.clear()
    }

    // 4 in 4 out
    for i in 0..4 {
        let msg = format!("4x4 a {i}");
        send(msg, &mut codec, &mut buf, &mut stream).await?;
        buf.clear();
    }

    for _ in 0..4 {
        recv(&mut codec, &mut buf, &mut stream).await?;
        buf.clear()
    }

    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tracing_subscriber::fmt().with_target(false).init();

    let server_jh = tokio::spawn(server());
    let client_jh = tokio::spawn(client());

    let (server_result, client_result) =
        futures::try_join!(server_jh, client_jh).expect("join error");

    server_result.expect("server failed");
    client_result.expect("client failed");
}
