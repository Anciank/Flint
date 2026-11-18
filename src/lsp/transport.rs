use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::io::{BufRead, Write};

/// Reads a single JSON-RPC 2.0 frame framed with `Content-Length: N\r\n\r\n`.
/// Returns `Ok(None)` cleanly on EOF before any header line.
pub fn read_lsp_message(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    let mut saw_header = false;

    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            if !saw_header {
                return Ok(None);
            }
            return Err(anyhow!("Unexpected EOF inside LSP headers"));
        }
        saw_header = true;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, val)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                let len = val
                    .trim()
                    .parse::<usize>()
                    .with_context(|| format!("Invalid Content-Length header: {val}"))?;
                content_length = Some(len);
            }
        }
    }

    let len = content_length.ok_or_else(|| anyhow!("Missing Content-Length header"))?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    let parsed: Value = serde_json::from_slice(&body).context("Invalid JSON-RPC payload")?;
    Ok(Some(parsed))
}

/// Writes a single JSON-RPC 2.0 message framed with `Content-Length`.
pub fn write_lsp_message(writer: &mut impl Write, payload: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(payload)?;
    write!(writer, "Content-Length: {}\r\n\r\n", bytes.len())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn roundtrips_multibyte_utf8_lsp_frames() {
        let msg1 = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"text": "你好🦀"}});
        let msg2 = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});

        let mut wire = Vec::new();
        write_lsp_message(&mut wire, &msg1).unwrap();
        write_lsp_message(&mut wire, &msg2).unwrap();

        let mut cursor = Cursor::new(wire);
        assert_eq!(read_lsp_message(&mut cursor).unwrap(), Some(msg1));
        assert_eq!(read_lsp_message(&mut cursor).unwrap(), Some(msg2));
        assert_eq!(read_lsp_message(&mut cursor).unwrap(), None);
    }

    #[test]
    fn parses_extra_headers_before_body() {
        let body = r#"{"jsonrpc":"2.0","id":7,"result":null}"#;
        let raw = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let mut cursor = Cursor::new(raw.as_bytes());
        let parsed = read_lsp_message(&mut cursor).unwrap().unwrap();
        assert_eq!(parsed["id"], 7);
    }
}
