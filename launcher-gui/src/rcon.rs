use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const AUTH: i32 = 3;
const AUTH_RESPONSE: i32 = 2;
const EXEC: i32 = 2;

pub struct Rcon {
    stream: TcpStream,
    next_id: i32,
}

impl Rcon {
    pub fn connect(host: &str, port: u16, password: &str, timeout: Duration) -> io::Result<Self> {
        let addr = (host, port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such address"))?;
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let mut rcon = Rcon { stream, next_id: 0 };
        let sent = rcon.send(AUTH, password)?;
        loop {
            let (id, kind, _) = rcon.recv()?;
            if kind == AUTH_RESPONSE {
                if id == -1 {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "RCON authentication failed",
                    ));
                }
                if id == sent {
                    return Ok(rcon);
                }
            }
        }
    }

    pub fn command(&mut self, body: &str) -> io::Result<String> {
        let sent = self.send(EXEC, body)?;
        let (id, _, payload) = self.recv()?;
        if id != sent {
            let (_, _, payload) = self.recv()?;
            return Ok(payload);
        }
        Ok(payload)
    }

    fn send(&mut self, kind: i32, body: &str) -> io::Result<i32> {
        self.next_id += 1;
        let id = self.next_id;
        let raw = body.as_bytes();
        let len = 4 + 4 + raw.len() + 2;
        let mut packet = Vec::with_capacity(len + 4);
        packet.extend_from_slice(&(len as i32).to_le_bytes());
        packet.extend_from_slice(&id.to_le_bytes());
        packet.extend_from_slice(&kind.to_le_bytes());
        packet.extend_from_slice(raw);
        packet.extend_from_slice(&[0, 0]);
        self.stream.write_all(&packet)?;
        Ok(id)
    }

    fn recv(&mut self) -> io::Result<(i32, i32, String)> {
        let mut size_buf = [0u8; 4];
        self.stream.read_exact(&mut size_buf)?;
        let size = i32::from_le_bytes(size_buf);
        if !(10..=8_388_608).contains(&size) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("implausible RCON packet size {size}"),
            ));
        }
        let mut body = vec![0u8; size as usize];
        self.stream.read_exact(&mut body)?;
        let id = i32::from_le_bytes(body[0..4].try_into().unwrap());
        let kind = i32::from_le_bytes(body[4..8].try_into().unwrap());
        let text = String::from_utf8_lossy(&body[8..body.len().saturating_sub(2)]).into_owned();
        Ok((id, kind, text))
    }
}
