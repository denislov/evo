use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread::{self, JoinHandle};

use crate::SseFrame;

pub struct MockInferenceSseServer {
    address: SocketAddr,
    worker: Option<JoinHandle<Result<String, std::io::Error>>>,
}

impl MockInferenceSseServer {
    pub fn spawn(frames: Vec<SseFrame>) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept()?;
            let mut request = Vec::with_capacity(1024);
            let mut chunk = [0_u8; 1024];
            while request.len() < 8 * 1024 {
                let read = stream.read(&mut chunk)?;
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&request).into_owned();
            stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )?;
            for frame in frames {
                write!(stream, "event: {}\ndata: {}\n\n", frame.event, frame.data)?;
                stream.flush()?;
            }
            Ok(request)
        });
        Ok(Self {
            address,
            worker: Some(worker),
        })
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn finish(mut self) -> Result<String, String> {
        self.worker
            .take()
            .expect("mock SSE worker is present")
            .join()
            .map_err(|_| "mock SSE worker panicked".to_owned())?
            .map_err(|error| error.to_string())
    }
}

pub fn fetch_sse(address: SocketAddr, path: &str) -> Result<Vec<SseFrame>, std::io::Error> {
    let mut stream = TcpStream::connect(address)?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: 0\r\n\r\n"
    )?;
    stream.flush()?;
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0_u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => bytes.extend_from_slice(&chunk[..read]),
            Err(error)
                if error.kind() == std::io::ErrorKind::ConnectionReset && !bytes.is_empty() =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    let response = String::from_utf8_lossy(&bytes);
    let body = response.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    Ok(body
        .split("\n\n")
        .filter_map(|block| {
            let mut event = None;
            let mut data = Vec::new();
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("event: ") {
                    event = Some(value.to_owned());
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data.push(value);
                }
            }
            event.map(|event| SseFrame {
                event,
                data: data.join("\n"),
            })
        })
        .collect())
}
