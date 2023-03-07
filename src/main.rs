use std::io::ErrorKind;

use duplex::Shuttle;
use futures::SinkExt;
use tokio::{
    io::{self},
    net::{TcpListener, TcpStream},
    sync::mpsc::{Receiver, Sender},
};
use tokio_stream::StreamExt;
use tokio_util::codec::{Decoder, Framed};
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

async fn send(msg: String, framed: &mut Framed<TcpStream, proto::MyStringCodec>) -> io::Result<()> {
    tracing::info!(">> writing: {msg}");
    framed.send(msg.clone()).await?;
    tracing::info!("<< written: {msg}");
    Ok(())
}

async fn recv(framed: &mut Framed<TcpStream, proto::MyStringCodec>) -> io::Result<()> {
    tracing::info!(">> receiving");
    let msg = framed
        .next()
        .await
        .transpose()?
        .ok_or(io::Error::from(ErrorKind::UnexpectedEof))?;

    tracing::info!("<< received: {msg}");
    Ok(())
}

#[tracing::instrument]
async fn client() -> io::Result<()> {
    let stream = TcpStream::connect(ADDR).await?;

    let codec = proto::MyStringCodec {};
    let mut framed = codec.framed(stream);

    // ping pong
    for i in 0..2 {
        let msg = format!("ping pong {i}");
        send(msg, &mut framed).await?;

        recv(&mut framed).await?;
    }

    // 4 in 4 out
    for i in 0..4 {
        let msg = format!("4x4 a {i}");
        send(msg, &mut framed).await?;
    }

    for _ in 0..4 {
        recv(&mut framed).await?;
    }

    // TODO other possible tests: SinkExt::feed, SinkExt::send_all
    // https://docs.rs/futures/latest/futures/sink/trait.SinkExt.html#method.feed

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
