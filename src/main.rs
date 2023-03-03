use tokio::{
    io,
    net::TcpListener,
    sync::mpsc::{Receiver, Sender},
};

const MAX: usize = 8 * 1024 * 1024;

mod decoder;
mod duplex;
mod encoder;

async fn echo_worker(mut receiver: Receiver<String>, sender: Sender<String>) {
    while let Some(msg) = receiver.recv().await {
        sender.send(msg).await.expect("disconnected");
    }
}

async fn server() -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:34254").await?;

    let (stream, _) = listener.accept().await?;

    let (send_tx, send_rx) = tokio::sync::mpsc::channel::<String>(10);
    let (reply_tx, reply_rx) = tokio::sync::mpsc::channel::<String>(10);

    tokio::spawn(echo_worker(send_rx, reply_tx));

    let decoder = decoder::MyStringDecoder {};
    let encoder = encoder::MyStringEncoder {};

    duplex::spawn_duplex(stream, decoder, encoder, send_tx, reply_rx)
        .await
        .expect("duplex failed");

    Ok(())
}

async fn client() -> io::Result<()> {
    todo!()
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
