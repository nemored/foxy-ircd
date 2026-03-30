use std::{
    net::SocketAddr,
};

use tokio::{
    prelude::*,
    io,
    net::TcpStream,
};

pub trait FoxyStream : AsyncRead + AsyncWrite + Send + Unpin {
    fn peer_addr(&self) -> io::Result<SocketAddr>;
}

impl FoxyStream for TcpStream {
    fn peer_addr(&self) -> io::Result<SocketAddr> {
        TcpStream::peer_addr(self)
    }
}
