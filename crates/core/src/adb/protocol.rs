//! Wire format: a request is `<4 hex digits length><payload>`, the reply starts
//! with `OKAY` or `FAIL` (followed by a length-prefixed message).

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::AdbError;

const MAX_REQUEST_LEN: usize = 0xFFFF;

pub async fn send_request<S: AsyncWrite + Unpin>(
	stream: &mut S,
	request: &str,
) -> Result<(), AdbError> {
	if request.len() > MAX_REQUEST_LEN {
		return Err(AdbError::Protocol(format!(
			"request too long ({} bytes)",
			request.len()
		)));
	}
	let framed = format!("{:04x}{request}", request.len());
	stream.write_all(framed.as_bytes()).await?;
	stream.flush().await?;
	Ok(())
}

pub async fn read_status<S: AsyncRead + Unpin>(stream: &mut S) -> Result<(), AdbError> {
	let mut status = [0u8; 4];
	stream.read_exact(&mut status).await?;
	match &status {
		b"OKAY" => Ok(()),
		b"FAIL" => Err(AdbError::Server(read_hex_string(stream).await?)),
		other => Err(AdbError::Protocol(format!(
			"unexpected status {:?}",
			String::from_utf8_lossy(other)
		))),
	}
}

pub async fn read_hex_string<S: AsyncRead + Unpin>(stream: &mut S) -> Result<String, AdbError> {
	let len = read_hex_len(stream).await?;
	let mut buf = vec![0u8; len];
	stream.read_exact(&mut buf).await?;
	Ok(String::from_utf8_lossy(&buf).into_owned())
}

async fn read_hex_len<S: AsyncRead + Unpin>(stream: &mut S) -> Result<usize, AdbError> {
	let mut len = [0u8; 4];
	stream.read_exact(&mut len).await?;
	std::str::from_utf8(&len)
		.ok()
		.and_then(|s| usize::from_str_radix(s, 16).ok())
		.ok_or_else(|| AdbError::Protocol(format!("bad length prefix {len:?}")))
}

pub async fn send_request_checked<S: AsyncRead + AsyncWrite + Unpin>(
	stream: &mut S,
	request: &str,
) -> Result<(), AdbError> {
	send_request(stream, request).await?;
	read_status(stream).await
}

/// Shell protocol v2 packet ids.
pub mod shell_v2 {
	pub const STDOUT: u8 = 1;
	pub const STDERR: u8 = 2;
	pub const EXIT: u8 = 3;
}

pub struct ShellPacket {
	pub id: u8,
	pub data: Vec<u8>,
}

/// Returns `None` on a clean EOF between packets.
pub async fn read_shell_packet<S: AsyncRead + Unpin>(
	stream: &mut S,
) -> Result<Option<ShellPacket>, AdbError> {
	let mut header = [0u8; 5];
	match stream.read_exact(&mut header).await {
		Ok(_) => {}
		Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
		Err(e) => return Err(e.into()),
	}
	let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
	let mut data = vec![0u8; len];
	stream.read_exact(&mut data).await?;
	Ok(Some(ShellPacket {
		id: header[0],
		data,
	}))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn frames_request() {
		let mut out = Vec::new();
		send_request(&mut out, "host:version").await.unwrap();
		assert_eq!(out, b"000chost:version");
	}

	#[tokio::test]
	async fn reads_fail_message() {
		let mut input: &[u8] = b"FAIL0010device not found";
		let err = read_status(&mut input).await.unwrap_err();
		assert!(matches!(err, AdbError::Server(m) if m == "device not found"));
	}

	#[tokio::test]
	async fn rejects_garbage_status() {
		let mut input: &[u8] = b"WHAT";
		assert!(matches!(
			read_status(&mut input).await,
			Err(AdbError::Protocol(_))
		));
	}

	#[tokio::test]
	async fn reads_shell_packets_until_eof() {
		let mut input: &[u8] = &[1, 2, 0, 0, 0, b'h', b'i', 3, 1, 0, 0, 0, 7];
		let first = read_shell_packet(&mut input).await.unwrap().unwrap();
		assert_eq!(
			(first.id, first.data.as_slice()),
			(shell_v2::STDOUT, &b"hi"[..])
		);
		let exit = read_shell_packet(&mut input).await.unwrap().unwrap();
		assert_eq!(
			(exit.id, exit.data.as_slice()),
			(shell_v2::EXIT, &[7u8][..])
		);
		assert!(read_shell_packet(&mut input).await.unwrap().is_none());
	}
}
