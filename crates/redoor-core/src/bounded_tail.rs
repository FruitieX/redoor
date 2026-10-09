//! Drains a child-process pipe while keeping only its final bytes.

use tokio::io::{AsyncRead, AsyncReadExt};

/// Reads `reader` to EOF and returns at most its last `limit` bytes.
///
/// Diagnostics of failed subprocesses (SSH, managed agents) usually end with the
/// actionable line, so retaining the tail keeps them useful while a chatty or hostile
/// process cannot grow server memory. The pipe is always drained completely so the
/// child never blocks on a full pipe buffer.
pub async fn read_bounded_tail(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let mut tail = Vec::with_capacity(limit.min(64 * 1024));
    let mut chunk = [0_u8; 4096];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok(tail);
        }
        tail.extend_from_slice(&chunk[..read]);
        if tail.len() > limit {
            let excess = tail.len() - limit;
            tail.drain(..excess);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::read_bounded_tail;

    /// Output longer than the limit keeps exactly its final bytes.
    #[tokio::test]
    async fn keeps_only_the_tail_of_long_output() {
        let input: Vec<u8> = (0..10_000_u32).map(|value| (value % 251) as u8).collect();
        let tail = read_bounded_tail(input.as_slice(), 100).await.unwrap();
        // The bound must hold regardless of how much the child wrote.
        assert_eq!(tail.len(), 100);
        // The retained bytes are the last ones, where diagnostics usually end.
        assert_eq!(tail, input[input.len() - 100..]);
    }

    /// Short output is returned unchanged.
    #[tokio::test]
    async fn short_output_is_returned_whole() {
        let tail = read_bounded_tail(&b"permission denied\n"[..], 100)
            .await
            .unwrap();
        // Nothing may be dropped while under the limit.
        assert_eq!(tail, b"permission denied\n");
    }
}
