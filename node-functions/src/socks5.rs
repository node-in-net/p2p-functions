use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc as tokio_mpsc;

/// Opens a plain TCP connection to `host:port` on a peer's behalf — the exit
/// side of the peer's SOCKS proxy. No SOCKS5 handshake happens here; the peer's
/// own client speaks it and sends us an already-resolved target.
///
/// Returns a channel carrying bytes read from the target, while bytes from
/// `local_rx` are written to the socket.
///
/// `allow_local` opens up the host's own network; without it the peer reaches
/// the internet and nothing else. See [`crate::net_guard`].
pub async fn start_socks_client(
    host: &str,
    port: u16,
    allow_local: bool,
    mut local_rx: tokio_mpsc::Receiver<Vec<u8>>,
) -> Result<tokio_mpsc::Receiver<Vec<u8>>, String> {
    let targets = crate::net_guard::resolve_allowed(host, port, allow_local).await?;
    match TcpStream::connect(&targets[..]).await {
        Ok(stream) => {
            let (mut rx_tcp, mut tx_tcp) = stream.into_split();
            let (tx_to_caller, rx_to_caller) = tokio_mpsc::channel::<Vec<u8>>(1024);

            let read_task_tx = tx_to_caller.clone();

            // Task 1: Read from TCP and send to caller
            let read_task = tokio::spawn(async move {
                let mut buf = [0u8; 16384];
                loop {
                    match rx_tcp.read(&mut buf).await {
                        Ok(0) => break, // EOF
                        Ok(n) => {
                            if read_task_tx.send(buf[..n].to_vec()).await.is_err() {
                                break; // Receiver dropped, stop reading
                            }
                        }
                        Err(_) => break, // Error, stop reading
                    }
                }
            });

            // Task 2: Read from caller and write to TCP
            let write_task = tokio::spawn(async move {
                while let Some(data) = local_rx.recv().await {
                    if tx_tcp.write_all(&data).await.is_err() {
                        break; // Connection broken, stop writing
                    }
                }
            });

            // Keep the task handles alive globally but they will terminate when channels or sockets drop
            tokio::spawn(async move {
                tokio::select! {
                    _ = read_task => (),
                    _ = write_task => (),
                };
            });

            Ok(rx_to_caller)
        }
        Err(e) => Err(e.to_string()),
    }
}
