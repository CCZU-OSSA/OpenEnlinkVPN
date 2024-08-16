use std::{io::ErrorKind, sync::Arc};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::Mutex,
};
use tokio_rustls::{client::TlsStream, rustls::ClientConfig, TlsConnector};

use crate::{
    auth::Authorization,
    data::{GDWData, ServerData},
    trust::NoVerification,
};

#[derive(Debug, Clone)]
pub struct EnlinkProtocal<A: Authorization> {
    pub authorization: A,
    pub stream: Arc<Mutex<TlsStream<TcpStream>>>,
}

impl<A: Authorization> EnlinkProtocal<A> {
    // Write
    pub async fn connect(authorization: A) -> tokio::io::Result<Self> {
        let config = Arc::new(
            ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerification {}))
                .with_no_client_auth(),
        );
        let addr = format!("{}:{}", authorization.host(), authorization.port());

        let connector = TlsConnector::from(config);
        let tcpstream = TcpStream::connect(addr).await?;
        let stream = connector
            .connect(authorization.host().try_into().unwrap(), tcpstream)
            .await?;

        Ok(Self {
            authorization,
            stream: Arc::new(Mutex::new(stream)),
        })
    }

    pub async fn authorize(&self) -> tokio::io::Result<ServerData> {
        let mut guard = self.stream.lock().await;
        let token = self.authorization.token();
        let bytes_token = token.as_bytes();
        let user = self.authorization.user();
        let bytes_user = user.as_bytes();
        // Version
        guard.write_u8(1).await?;
        // Protocal
        guard.write_u8(1).await?;
        // Length
        guard
            .write_u16(19 + bytes_user.len() as u16 + bytes_token.len() as u16)
            .await?;
        guard
            .write(&[
                0, 0, 0, 0, // Zero
                1, 0, 0, 0, // ELK_METHOD_STUN
                1, 0, // ELK_OPT_USERNAME
            ])
            .await?;
        // User
        guard.write_u8(bytes_user.len() as u8).await?;
        guard.write(bytes_user).await?;
        // ELK_OPT_SESSID
        guard.write(&[2, 0]).await?;

        // Token
        guard.write_u8(bytes_token.len() as u8).await?;
        guard.write(bytes_token).await?;
        // -1 & 0xff

        guard.write_u8(255).await?;

        guard.flush().await?;

        // Release the Mutex
        drop(guard);

        // Read VPNData
        let status = self.read_is_authorize_ok().await?;
        if !status {
            return Err(tokio::io::Error::new(ErrorKind::Other, "Authorize Failed"));
        }
        let ip = self.read_virtual_address().await?;
        let mask = self.read_virtual_mask().await?;
        let data = self.read_gateway_dns_wins_data().await?;

        println!("{:?}", self.read_until_end().await?);
        Ok(ServerData {
            ip,
            mask,
            gateway: data.gateway,
            dns: data.dns,
            wins: data.wins,
        })
    }

    pub async fn write_heartbeat(&self) -> tokio::io::Result<()> {
        let mut guard = self.stream.lock().await;

        guard.write(&[1, 1, 0, 12, 0, 0, 0, 0, 3, 0, 0, 0]).await?;
        guard.flush().await?;
        Ok(())
    }

    pub async fn write_tcp(&self, data: &[u8], read: i16) -> tokio::io::Result<()> {
        let mut guard = self.stream.lock().await;
        // Custom header
        guard.write(&[1, 4]).await?;
        // Length
        guard.write_i16(read + 12).await?;
        // XID
        guard.write(&[0, 0, 0, 0]).await?;
        guard.write_i32(1).await?;
        // Data
        guard.write(data).await?;
        guard.write_u8(0).await?;
        guard.write_i16(read).await?;
        guard.flush().await?;
        Ok(())
    }

    // Read
    pub async fn read_is_authorize_ok(&self) -> tokio::io::Result<bool> {
        let mut guard = self.stream.lock().await;
        // Skip 10
        guard.read_exact(&mut [0u8; 10]).await?;

        let mut status = vec![0, 0];
        guard.read_exact(&mut status).await?;
        Ok(status[0] == 0 && status[1] == 0)
    }

    pub async fn read_virtual_address(&self) -> tokio::io::Result<[u8; 4]> {
        let mut guard = self.stream.lock().await;

        let mut status = [0u8; 3];
        guard.read_exact(&mut status).await?;
        if status[0] == 11 && status[1] == 0 && status[2] == 4 {
            Ok([
                guard.read_u8().await? & 255,
                guard.read_u8().await? & 255,
                guard.read_u8().await? & 255,
                guard.read_u8().await? & 255,
            ])
        } else {
            Err(tokio::io::Error::new(
                ErrorKind::InvalidData,
                format!("Invalid Data Header `{:?}`", status),
            ))
        }
    }

    pub async fn read_virtual_mask(&self) -> tokio::io::Result<usize> {
        let mut guard = self.stream.lock().await;

        let mut status = [0u8; 3];
        guard.read_exact(&mut status).await?;
        if status[0] == 12 && status[1] == 0 && status[2] == 4 {
            let mut len = 0;
            let mut a = 0;
            let mut b = 0;
            let mut data = vec![0u8; 4];
            guard.read_exact(&mut data).await?;
            data.iter().for_each(|val| {
                let val = val & 255;
                let binary = format!("{val:b}");
                while let Some(pos) = binary
                    .chars()
                    .enumerate()
                    .position(|(pos, char)| pos >= b && char == '1')
                {
                    a = pos + 1;
                    b += 1;
                }

                len += b;
            });

            Ok(len)
        } else {
            Err(tokio::io::Error::new(
                ErrorKind::InvalidData,
                format!("Invalid Data Header `{:?}`", status),
            ))
        }
    }

    pub async fn read_gateway_dns_wins_data(&self) -> tokio::io::Result<GDWData> {
        let mut guard = self.stream.lock().await;
        let mut gateway = vec![];
        let mut dns = String::default();
        let mut wins = String::default();

        loop {
            let mut status = [0u8; 2];
            guard.read_exact(&mut status).await?;
            if status[0] != 43 {
                match status {
                    [35, 0] => {
                        let length = guard.read_u8().await?;
                        let mut data = vec![0u8; length as usize];
                        guard.read_exact(&mut data).await?;

                        gateway = data.into_iter().map(|val| val & 255).collect();
                    }
                    [36, 0] => {
                        let length = guard.read_u8().await?;
                        let mut data = vec![0u8; length as usize];
                        guard.read_exact(&mut data).await?;

                        dns = String::from_utf8(data).map_err(|e| {
                            tokio::io::Error::new(ErrorKind::InvalidData, e.to_string())
                        })?;
                    }
                    [37, 0] => {
                        let length = guard.read_u8().await?;
                        let mut data = vec![0u8; length as usize];
                        guard.read_exact(&mut data).await?;

                        wins = String::from_utf8(data).map_err(|e| {
                            tokio::io::Error::new(ErrorKind::InvalidData, e.to_string())
                        })?;
                    }
                    _ => {
                        return Err(tokio::io::Error::new(
                            ErrorKind::InvalidData,
                            format!("Invalid Status {:?}", status),
                        ))
                    }
                };
            } else {
                break;
            }
        }

        return Ok(GDWData { gateway, dns, wins });
    }

    pub async fn drop(&self, size: usize) -> tokio::io::Result<Vec<u8>> {
        let mut data = vec![0u8; size];
        self.stream.lock().await.read(&mut data).await?;
        Ok(data)
    }

    pub async fn read_until_end(&self) -> tokio::io::Result<Vec<u8>> {
        let mut data = vec![];
        let mut guard = self.stream.lock().await;
        loop {
            let bin = guard.read_u8().await?;
            data.push(bin);
            if bin == 255 {
                return Ok(data);
            }
        }
    }
}