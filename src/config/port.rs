use std::io;
use std::net::{SocketAddr, TcpListener};

/// Пытается занять `start_addr`. Если порт занят (`AddrInUse`),
/// перебирает следующие порты (+1) до `max_attempts` раз.
pub fn bind_available_port(
  mut addr: SocketAddr,
  max_attempts: u16,
) -> io::Result<(TcpListener, SocketAddr)> {
  for attempt in 0..max_attempts {
    match TcpListener::bind(addr) {
      Ok(listener) => {
        // Важно для tokio::net::TcpListener::from_std
        listener.set_nonblocking(true)?;
        let actual_addr = listener.local_addr()?;
        if attempt > 0 {
          println!("Port {} was busy, bound to {actual_addr} instead", addr.port() - attempt);
        }
        return Ok((listener, actual_addr));
      }
      Err(err)
        if matches!(
          err.kind(),
          io::ErrorKind::AddrInUse | io::ErrorKind::PermissionDenied
        ) =>
      {
        let next_port = addr.port().checked_add(1).ok_or_else(|| {
          io::Error::new(io::ErrorKind::AddrNotAvailable, "port overflow while searching")
        })?;
        addr.set_port(next_port);
      }
      Err(err) => return Err(err),
    }
  }

  Err(io::Error::new(
    io::ErrorKind::AddrInUse,
    format!("no free ports found after {max_attempts} attempts"),
  ))
}
