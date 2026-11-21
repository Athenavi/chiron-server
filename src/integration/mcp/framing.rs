//! JSON-RPC 分帧。
//!
//! MCP 的三条传输各有分帧口径：
//! * stdio：一行一条 JSON（NDJSON，UTF-8，`\n` 结尾）；
//! * 部分老客户端用 LSP 式 `Content-Length: <n>\r\n\r\n<body>`；
//! * SSE / streamable HTTP：HTTP 层已分帧，body 里的 `data:` 还要再过一次 SSE。
//!
//! 因此解码器不做 I/O：喂字节、取消息，任意切分都能重组。

use serde_json::Value;

use super::jsonrpc::McpError;

/// 默认单帧上限（8 MiB）。工具结果可能很大，但拒绝无上限缓冲。
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// 一行一条 JSON。
    Ndjson,
    /// `Content-Length` + 空行 + body。
    ContentLength,
}

impl Framing {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ndjson => "ndjson",
            Self::ContentLength => "content-length",
        }
    }

    /// 从 manifest/配置里解析；未知值返回 None，由调用方决定是否报错。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ndjson" | "line" | "lines" | "jsonl" => Some(Self::Ndjson),
            "content-length" | "lsp" | "header" => Some(Self::ContentLength),
            _ => None,
        }
    }

    pub fn encode(self, message: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(message).unwrap_or_default();
        match self {
            Self::Ndjson => {
                let mut out = body;
                out.push(b'\n');
                out
            }
            Self::ContentLength => {
                let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
                out.extend_from_slice(&body);
                out
            }
        }
    }
}

#[derive(Debug)]
pub struct FrameDecoder {
    framing: Framing,
    buf: Vec<u8>,
    max_frame: usize,
}

impl FrameDecoder {
    pub fn new(framing: Framing) -> Self {
        Self {
            framing,
            buf: Vec::new(),
            max_frame: MAX_FRAME_BYTES,
        }
    }

    pub fn with_max_frame(mut self, max: usize) -> Self {
        self.max_frame = max.max(1);
        self
    }

    pub fn framing(&self) -> Framing {
        self.framing
    }

    /// 喂入字节；超过单帧上限立即失败，避免对端用一个超大帧把内存打满。
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), McpError> {
        if self.buf.len() + chunk.len() > self.max_frame {
            return Err(McpError::Protocol(format!(
                "frame exceeds {} bytes",
                self.max_frame
            )));
        }
        self.buf.extend_from_slice(chunk);
        Ok(())
    }

    /// 取出一条完整消息；`Ok(None)` 表示还缺字节。
    pub fn pop(&mut self) -> Result<Option<Vec<u8>>, McpError> {
        match self.framing {
            Framing::Ndjson => self.pop_line(),
            Framing::ContentLength => self.pop_block(),
        }
    }

    pub fn has_pending(&self) -> bool {
        !self.buf.is_empty()
    }

    fn pop_line(&mut self) -> Result<Option<Vec<u8>>, McpError> {
        loop {
            let Some(pos) = self.buf.iter().position(|b| *b == b'\n') else {
                return Ok(None);
            };
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            // 去掉 \n 与可能的 \r；空行是心跳/分隔，跳过。
            let payload = trim_ascii(&line[..line.len() - 1]);
            if payload.is_empty() {
                continue;
            }
            if payload.len() > self.max_frame {
                return Err(McpError::Protocol("frame too large".into()));
            }
            return Ok(Some(payload.to_vec()));
        }
    }

    fn pop_block(&mut self) -> Result<Option<Vec<u8>>, McpError> {
        let Some((header_len, body_start)) = find_header_end(&self.buf) else {
            return Ok(None);
        };
        let header = String::from_utf8_lossy(&self.buf[..header_len]).into_owned();
        let length = parse_content_length(&header)?;
        if length > self.max_frame {
            return Err(McpError::Protocol(format!(
                "content-length {length} exceeds {}",
                self.max_frame
            )));
        }
        if self.buf.len() < body_start + length {
            return Ok(None);
        }
        let payload: Vec<u8> = self.buf.drain(..body_start + length).collect();
        Ok(Some(payload[body_start..].to_vec()))
    }
}

/// 返回 `(header 长度, body 起点)`。
fn find_header_end(buf: &[u8]) -> Option<(usize, usize)> {
    if let Some(idx) = find(buf, b"\r\n\r\n") {
        return Some((idx, idx + 4));
    }
    find(buf, b"\n\n").map(|idx| (idx, idx + 2))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse_content_length(header: &str) -> Result<usize, McpError> {
    for line in header.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            return value.trim().parse::<usize>().map_err(|_| {
                McpError::Protocol(format!("invalid content-length `{}`", value.trim()))
            });
        }
    }
    Err(McpError::Protocol(
        "missing content-length header in frame".into(),
    ))
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while let Some((first, rest)) = bytes.split_first() {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    while let Some((last, rest)) = bytes.split_last() {
        if last.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ndjson_survives_arbitrary_chunking() {
        let first = json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}});
        let second = json!({"jsonrpc":"2.0","method":"notifications/message"});
        let mut wire = Framing::Ndjson.encode(&first);
        wire.extend_from_slice(&Framing::Ndjson.encode(&second));

        let mut decoder = FrameDecoder::new(Framing::Ndjson);
        let mut frames = Vec::new();
        // 逐字节喂入：跨块、跨多字节字符都不能丢。
        for byte in wire {
            decoder.push(&[byte]).unwrap();
            while let Some(frame) = decoder.pop().unwrap() {
                frames.push(frame);
            }
        }
        assert_eq!(frames.len(), 2);
        assert_eq!(
            serde_json::from_slice::<Value>(&frames[0]).unwrap()["id"],
            json!(1)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&frames[1]).unwrap()["method"],
            "notifications/message"
        );
        assert!(!decoder.has_pending());
    }

    #[test]
    fn ndjson_skips_blank_lines_and_strips_crlf() {
        let mut decoder = FrameDecoder::new(Framing::Ndjson);
        decoder.push(b"\r\n{\"a\":1}\r\n\n").unwrap();
        let frame = decoder.pop().unwrap().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&frame).unwrap()["a"], 1);
        assert!(decoder.pop().unwrap().is_none());
    }

    #[test]
    fn content_length_frames_concatenate_without_delimiter() {
        let a = Framing::ContentLength.encode(&json!({"id":1}));
        let b = Framing::ContentLength.encode(&json!({"id":2}));
        assert!(String::from_utf8_lossy(&a).starts_with("Content-Length: "));

        let mut decoder = FrameDecoder::new(Framing::ContentLength);
        // 一次喂两条 + 半个第三条：只能取出完整的两条。
        let mut wire = a.clone();
        wire.extend_from_slice(&b);
        wire.extend_from_slice(&a[..5]);
        decoder.push(&wire).unwrap();
        let first = decoder.pop().unwrap().unwrap();
        let second = decoder.pop().unwrap().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&first).unwrap()["id"], 1);
        assert_eq!(serde_json::from_slice::<Value>(&second).unwrap()["id"], 2);
        assert!(decoder.pop().unwrap().is_none());
    }

    #[test]
    fn content_length_is_case_insensitive_and_required() {
        let body = br#"{"id":9}"#;
        let wire = format!("content-length: {}\r\n\r\n", body.len());
        let mut decoder = FrameDecoder::new(Framing::ContentLength);
        decoder.push(wire.as_bytes()).unwrap();
        decoder.push(body).unwrap();
        let frame = decoder.pop().unwrap().unwrap();
        assert_eq!(frame, body.to_vec());

        let mut decoder = FrameDecoder::new(Framing::ContentLength);
        decoder.push(b"X-Other: 1\r\n\r\n{}").unwrap();
        let err = decoder.pop().unwrap_err();
        assert!(err.to_string().contains("content-length"));
    }

    #[test]
    fn oversize_frames_are_rejected() {
        let mut decoder = FrameDecoder::new(Framing::Ndjson).with_max_frame(16);
        let err = decoder.push(&[b'x'; 32]).unwrap_err();
        assert!(matches!(err, McpError::Protocol(_)));
    }

    #[test]
    fn framing_names_parse() {
        assert_eq!(Framing::parse("NDJSON"), Some(Framing::Ndjson));
        assert_eq!(Framing::parse("lsp"), Some(Framing::ContentLength));
        assert_eq!(Framing::parse("bogus"), None);
        assert_eq!(Framing::Ndjson.as_str(), "ndjson");
    }
}
