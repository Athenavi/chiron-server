//! 最小 SSE 解码器（B3）。
//!
//! 两种协议都是 `text/event-stream`，差异只在事件名与 data 结构上，因此流分帧
//! 逻辑只写一份，适配器只负责把 data 解释成 `StreamDelta`。

/// 一个完整的 SSE 事件（已去掉 framing 行）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// `event:` 字段，缺省为 `message`（OpenAI 不发事件名）。
    pub event: Option<String>,
    /// `data:` 字段多行以 `\n` 连接。
    pub data: String,
    /// `id:` 字段（两家都不用，保留）。
    pub id: Option<String>,
}

impl SseEvent {
    pub fn event_name(&self) -> &str {
        self.event.as_deref().unwrap_or("message")
    }

    /// `[DONE]` 哨兵（OpenAI 兼容端点用它收尾）。
    pub fn is_done(&self) -> bool {
        self.data.trim() == "[DONE]"
    }
}

/// 增量式 SSE 解码器：字节块可以任意切分，跨块的多字节字符与事件不会丢。
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// 喂入一块字节，返回其中已经完整的事件。
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some((end, delim)) = find_boundary(&self.buf) {
            let block: Vec<u8> = self.buf.drain(..end + delim).collect();
            let text = String::from_utf8_lossy(&block[..end]).into_owned();
            if let Some(ev) = parse_block(&text) {
                out.push(ev);
            }
        }
        out
    }

    /// 流结束时若还有半截数据，按一个事件解析（宽容处理缺少结尾空行的服务端）。
    pub fn finish(&mut self) -> Option<SseEvent> {
        if self.buf.is_empty() {
            return None;
        }
        let text = String::from_utf8_lossy(&self.buf).into_owned();
        self.buf.clear();
        parse_block(&text)
    }

    pub fn has_pending(&self) -> bool {
        !self.buf.is_empty()
    }
}

fn find_boundary(b: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\n' {
            if i + 1 < b.len() && b[i + 1] == b'\n' {
                return Some((i, 2));
            }
        } else if b[i] == b'\r'
            && i + 3 < b.len()
            && b[i + 1] == b'\n'
            && b[i + 2] == b'\r'
            && b[i + 3] == b'\n'
        {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

fn parse_block(block: &str) -> Option<SseEvent> {
    let mut ev = SseEvent::default();
    let mut has_data = false;
    for line in block.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "event" => ev.event = Some(value.to_string()),
            "data" => {
                if has_data {
                    ev.data.push('\n');
                }
                ev.data.push_str(value);
                has_data = true;
            }
            "id" => ev.id = Some(value.to_string()),
            _ => {}
        }
    }
    if !has_data && ev.event.is_none() {
        None
    } else {
        Some(ev)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_across_arbitrary_byte_chunks() {
        let mut p = SseParser::new();
        // 故意在事件中间、以及中文多字节字符中间切开
        let raw = "data: {\"a\":\"键\"}\n\ndata: [DONE]\n\n".as_bytes();
        let mut events = Vec::new();
        for chunk in raw.chunks(5) {
            events.extend(p.push(chunk));
        }
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "{\"a\":\"键\"}");
        assert!(events[1].is_done());
    }

    #[test]
    fn handles_crlf_and_named_events_and_comments() {
        let mut p = SseParser::new();
        let raw = ": ping\r\n\r\nevent: content_block_delta\r\ndata: {\"x\":1}\r\n\r\n";
        let events = p.push(raw.as_bytes());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_name(), "content_block_delta");
        assert_eq!(events[0].data, "{\"x\":1}");
    }

    #[test]
    fn multiline_data_is_joined_with_newline() {
        let mut p = SseParser::new();
        let events = p.push(b"data: a\ndata: b\n\n");
        assert_eq!(events[0].data, "a\nb");
    }

    #[test]
    fn unterminated_tail_is_recovered_on_finish() {
        let mut p = SseParser::new();
        assert!(p.push(b"data: {\"ok\":true}").is_empty());
        assert!(p.has_pending());
        let ev = p.finish().expect("tail event");
        assert_eq!(ev.data, "{\"ok\":true}");
        assert!(!p.has_pending());
    }
}
