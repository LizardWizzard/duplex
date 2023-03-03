use tokio::{io, net::TcpListener};

const MAX: usize = 8 * 1024 * 1024;

mod decoder;
mod duplex;
mod encoder;

async fn server() -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:34254").await?;

    todo!()
}

#[tokio::main]
async fn main() {
    // tokio::spawn(server())
}
