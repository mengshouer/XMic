use std::{
    io::{Read, Write},
    time::Duration,
};

use serde::{de::DeserializeOwned, Serialize};
use xmic_engine_protocol::{EngineRequest, EngineResponse};
use xmic_platform_windows::connect_local_ipc;

const MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;

pub fn send_request(
    pipe_name: &str,
    request: &EngineRequest,
    connect_timeout: Duration,
) -> Result<EngineResponse, String> {
    let mut stream =
        connect_local_ipc(pipe_name, connect_timeout).map_err(|error| error.to_string())?;
    write_message(&mut stream, request)?;
    read_message(&mut stream)
}

pub fn read_message<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T, String> {
    let mut length = [0u8; size_of::<u32>()];
    reader
        .read_exact(&mut length)
        .map_err(|error| format!("read IPC message length: {error}"))?;
    let length = usize::try_from(u32::from_le_bytes(length))
        .map_err(|_| "IPC message length cannot be represented".to_owned())?;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(format!("IPC message length {length} is invalid"));
    }
    let mut payload = vec![0u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|error| format!("read IPC message payload: {error}"))?;
    serde_json::from_slice(&payload).map_err(|error| format!("decode IPC message: {error}"))
}

pub fn write_message<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<(), String> {
    let payload =
        serde_json::to_vec(value).map_err(|error| format!("encode IPC message: {error}"))?;
    if payload.is_empty() || payload.len() > MAX_MESSAGE_BYTES {
        return Err(format!("IPC message length {} is invalid", payload.len()));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| "IPC message length cannot be represented".to_owned())?;
    writer
        .write_all(&length.to_le_bytes())
        .and_then(|_| writer.write_all(&payload))
        .and_then(|_| writer.flush())
        .map_err(|error| format!("write IPC message: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use xmic_engine_protocol::PROTOCOL_VERSION;

    #[test]
    fn request_frame_round_trips() {
        let request = EngineRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
        };
        let mut encoded = Vec::new();
        write_message(&mut encoded, &request).unwrap();

        let decoded: EngineRequest = read_message(&mut Cursor::new(encoded)).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn oversized_frame_is_rejected_before_allocation() {
        let encoded = u32::try_from(MAX_MESSAGE_BYTES + 1)
            .unwrap()
            .to_le_bytes()
            .to_vec();

        let result = read_message::<EngineRequest>(&mut Cursor::new(encoded));
        assert!(result.unwrap_err().contains("invalid"));
    }
}
