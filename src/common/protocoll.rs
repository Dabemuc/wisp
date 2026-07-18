use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Serialize, Deserialize, Debug)]
pub enum ClientMessage {
    // control
    Attach { cols: u16, rows: u16 },
    KillServer,
    ListSessions,
    // data
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ServerMessage {
    // control
    Sessions(Vec<String>),
    // data
    Frame(Vec<u8>),
    Bell,
}

pub async fn write_msg<W, T>(w: &mut W, msg: &T) -> std::io::Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let bytes = postcard::to_allocvec(msg).expect("serialize");
    w.write_u32(bytes.len() as u32).await?;
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_msg<R, T>(r: &mut R) -> std::io::Result<T>
where
    R: AsyncReadExt + Unpin,
    T: DeserializeOwned,
{
    let len = r.read_u32().await? as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf).expect("deserialize"))
}
