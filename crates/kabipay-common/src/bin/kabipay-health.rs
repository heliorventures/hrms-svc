//! Dependency-free container liveness client. Never follows redirects or reads environment.
use std::{
    io::{BufRead, BufReader, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream},
    time::Duration,
};
fn probe(port: u16) -> std::io::Result<bool> {
    let timeout = Duration::from_secs(2);
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        timeout,
    )?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.write_all(b"GET /livez HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut line = Vec::new();
    // Bound the response even if the service emits a malformed status line.
    use std::io::Read;
    BufReader::new(stream)
        .take(128)
        .read_until(b'\n', &mut line)?;
    Ok(line.starts_with(b"HTTP/1.1 200 ") || line.starts_with(b"HTTP/1.0 200 "))
}
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let port = match args.as_slice() {
        [flag, port] if flag == "--port" && port.bytes().all(|v| v.is_ascii_digit()) => {
            port.parse::<u16>().ok().filter(|v| *v > 0)
        }
        _ => None,
    };
    std::process::exit(if port.is_some_and(|port| probe(port).unwrap_or(false)) {
        0
    } else {
        1
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn liveness_client_requires_200_and_uses_only_livez() {
        for (status, expected) in [("200 OK", true), ("503 Unavailable", false)] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let thread = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(line, "GET /livez HTTP/1.1\r\n");
                write!(socket, "HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n").unwrap();
            });
            assert_eq!(probe(port).unwrap(), expected);
            thread.join().unwrap();
        }
    }
}
