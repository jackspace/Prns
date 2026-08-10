//! Byte-stream scanner for the Taixin AT console. The console interleaves command responses with
//! unsolicited `[timestamp]` debug lines, ANSI color escapes, and periodic multi-line
//! `LMAC STATUS` dumps at any moment, so the scanner keys on structure alone: complete lines, and
//! the one URC that switches the stream to binary — `+RXDATA:<n>\r\n` followed by exactly `n` raw
//! bytes that must never be line-split (payloads contain `\r\n` and arbitrary bytes).

use super::protocol::HALOW_AT_AIR_FRAME_CAP;

/// Longest console line kept whole. `LMAC STATUS` dump lines and config dumps fit well under
/// this; anything longer arrives truncated, which still matches every token the driver keys on.
pub const AT_LINE_CAP: usize = 192;

/// What one fed byte completed, if anything. Data is read back through
/// [`line`](AtConsole::line) / [`rx_frame`](AtConsole::rx_frame) before the next feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtStep {
    None,
    /// A complete console line (CR/LF stripped, ANSI escapes removed, possibly truncated).
    Line,
    /// A complete `+RXDATA` delivery: header plus payload, exactly as counted.
    RxFrame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ansi {
    Idle,
    Escape,
    Csi,
}

pub struct AtConsole {
    line: [u8; AT_LINE_CAP],
    line_len: usize,
    /// Length of the line exposed by [`line`](Self::line) — valid until the next feed overwrites
    /// the buffer, matching the read-before-feeding contract on [`AtStep`].
    emitted_len: usize,
    ansi: Ansi,
    frame: [u8; HALOW_AT_AIR_FRAME_CAP],
    /// Bytes of binary payload still owed to the current `+RXDATA`; zero means line mode.
    binary_need: usize,
    binary_got: usize,
    /// An announced count past the air cap cannot be a real delivery; its bytes are drained
    /// without storage so the scanner stays aligned with the stream.
    binary_discard: bool,
}

impl Default for AtConsole {
    fn default() -> Self {
        Self::new()
    }
}

impl AtConsole {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            line: [0; AT_LINE_CAP],
            line_len: 0,
            emitted_len: 0,
            ansi: Ansi::Idle,
            frame: [0; HALOW_AT_AIR_FRAME_CAP],
            binary_need: 0,
            binary_got: 0,
            binary_discard: false,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// The completed line after [`AtStep::Line`].
    #[must_use]
    pub fn line(&self) -> &[u8] {
        &self.line[..self.emitted_len]
    }

    /// The completed delivery (header ++ payload) after [`AtStep::RxFrame`].
    #[must_use]
    pub fn rx_frame(&self) -> &[u8] {
        &self.frame[..self.binary_got]
    }

    pub fn feed(&mut self, byte: u8) -> AtStep {
        if self.binary_need > 0 {
            if !self.binary_discard {
                self.frame[self.binary_got] = byte;
            }
            self.binary_got += 1;
            if self.binary_got == self.binary_need {
                self.binary_need = 0;
                if self.binary_discard {
                    self.binary_discard = false;
                    self.binary_got = 0;
                    return AtStep::None;
                }
                return AtStep::RxFrame;
            }
            return AtStep::None;
        }

        match self.ansi {
            Ansi::Escape => {
                self.ansi = if byte == b'[' { Ansi::Csi } else { Ansi::Idle };
                return AtStep::None;
            }
            Ansi::Csi => {
                if (0x40..=0x7E).contains(&byte) {
                    self.ansi = Ansi::Idle;
                }
                return AtStep::None;
            }
            Ansi::Idle => {}
        }

        match byte {
            0x1B => {
                self.ansi = Ansi::Escape;
                AtStep::None
            }
            b'\n' => self.finish_line(),
            b'\r' => AtStep::None,
            _ => {
                if self.line_len < AT_LINE_CAP {
                    self.line[self.line_len] = byte;
                    self.line_len += 1;
                }
                AtStep::None
            }
        }
    }

    fn finish_line(&mut self) -> AtStep {
        let len = self.line_len;
        self.line_len = 0;
        if len == 0 {
            return AtStep::None;
        }
        if let Some(count) = parse_rxdata_count(&self.line[..len]) {
            if count == 0 {
                return AtStep::None;
            }
            self.binary_need = count;
            self.binary_got = 0;
            self.binary_discard = count > HALOW_AT_AIR_FRAME_CAP;
            return AtStep::None;
        }
        self.emitted_len = len;
        AtStep::Line
    }
}

fn parse_rxdata_count(line: &[u8]) -> Option<usize> {
    let digits = line.strip_prefix(b"+RXDATA:")?;
    if digits.is_empty() || digits.len() > 5 {
        return None;
    }
    let mut count = 0usize;
    for &d in digits {
        if !d.is_ascii_digit() {
            return None;
        }
        count = count * 10 + usize::from(d - b'0');
    }
    Some(count)
}

/// The boot banner's stable prefix — the module-ready sentinel after `AT+RESET` or a spontaneous
/// reboot ("** hgSDK-v1.6.4.3-…").
#[must_use]
pub fn is_boot_banner(line: &[u8]) -> bool {
    line.starts_with(b"** hgSDK-v")
}

#[must_use]
pub fn is_ok(line: &[u8]) -> bool {
    line == b"OK"
}

#[must_use]
pub fn is_error(line: &[u8]) -> bool {
    line == b"ERROR"
}

/// Case-sensitive substring search, for skimming config-dump lines whose exact shape is firmware
/// noise around the token that matters.
#[must_use]
pub fn line_contains(line: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > line.len() {
        return needle.is_empty();
    }
    line.windows(needle.len()).any(|window| window == needle)
}

/// Pull a MAC out of a config-dump line after the given field marker (e.g. `b"addr:"`), tolerant
/// of spaces between the marker and the address. Returns `None` unless six colon-separated hex
/// pairs follow.
#[must_use]
pub fn parse_mac_after(line: &[u8], marker: &[u8]) -> Option<[u8; 6]> {
    let start = line
        .windows(marker.len())
        .position(|window| window == marker)?
        + marker.len();
    let mut rest = &line[start..];
    while let [b' ' | b'\t', tail @ ..] = rest {
        rest = tail;
    }
    let mut mac = [0u8; 6];
    for (index, octet) in mac.iter_mut().enumerate() {
        if index > 0 {
            let [b':', tail @ ..] = rest else {
                return None;
            };
            rest = tail;
        }
        let [high, low, tail @ ..] = rest else {
            return None;
        };
        *octet = (hex_value(*high)? << 4) | hex_value(*low)?;
        rest = tail;
    }
    Some(mac)
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(console: &mut AtConsole, bytes: &[u8]) -> Vec<(AtStep, Vec<u8>)> {
        let mut events = Vec::new();
        for &byte in bytes {
            match console.feed(byte) {
                AtStep::None => {}
                AtStep::Line => events.push((AtStep::Line, console.line().to_vec())),
                AtStep::RxFrame => events.push((AtStep::RxFrame, console.rx_frame().to_vec())),
            }
        }
        events
    }

    #[test]
    fn ok_and_error_lines_survive_interleaved_noise() {
        let mut console = AtConsole::new();
        let events = feed_all(
            &mut console,
            b"[12:03:44] dbg blah\r\nOK\r\n\x1b[32mLMAC STATUS:\x1b[0m\r\n  temp 41C vcc 3.3\r\nERROR\r\n",
        );
        let lines: Vec<&[u8]> = events.iter().map(|(_, data)| data.as_slice()).collect();
        assert!(lines.contains(&b"OK".as_slice()));
        assert!(lines.contains(&b"ERROR".as_slice()));
        assert!(is_ok(b"OK") && is_error(b"ERROR"));
        assert!(!is_ok(b"OK boss"));
    }

    #[test]
    fn ansi_escapes_are_stripped_from_lines() {
        let mut console = AtConsole::new();
        let events = feed_all(&mut console, b"\x1b[1;31mERROR\x1b[0m\r\n");
        assert_eq!(events, vec![(AtStep::Line, b"ERROR".to_vec())]);
    }

    #[test]
    fn rxdata_counts_binary_bytes_and_never_line_splits() {
        let mut console = AtConsole::new();
        // 14-byte header + 8 payload bytes containing \r\n, 0x7E, and a fake "OK\r\n".
        let mut body = vec![0xFF; 6];
        body.extend_from_slice(&[0x12, 0xFD, 0x11, 0x64, 0x98, 0x78]);
        body.extend_from_slice(&[0x48, 0x49]);
        body.extend_from_slice(b"\x7E\r\nOK\r\n\x7E");
        let mut stream = b"+RXDATA:22\r\n".to_vec();
        stream.extend_from_slice(&body);
        stream.extend_from_slice(b"OK\r\n");
        let events = feed_all(&mut console, &stream);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], (AtStep::RxFrame, body));
        assert_eq!(events[1], (AtStep::Line, b"OK".to_vec()));
    }

    #[test]
    fn an_absurd_rxdata_count_is_drained_without_storage() {
        let mut console = AtConsole::new();
        let mut stream = b"+RXDATA:300\r\n".to_vec();
        stream.extend_from_slice(&[0xAA; 300]);
        stream.extend_from_slice(b"OK\r\n");
        let events = feed_all(&mut console, &stream);
        assert_eq!(events, vec![(AtStep::Line, b"OK".to_vec())]);
    }

    #[test]
    fn a_malformed_rxdata_line_is_just_a_line() {
        let mut console = AtConsole::new();
        let events = feed_all(&mut console, b"+RXDATA:12g\r\n+RXDATA:\r\n");
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], (AtStep::Line, _)));
    }

    #[test]
    fn overlong_lines_arrive_truncated_but_never_desync() {
        let mut console = AtConsole::new();
        let mut stream = vec![b'x'; 500];
        stream.extend_from_slice(b"\r\nOK\r\n");
        let events = feed_all(&mut console, &stream);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].1.len(), AT_LINE_CAP);
        assert_eq!(events[1], (AtStep::Line, b"OK".to_vec()));
    }

    #[test]
    fn the_boot_banner_is_recognized() {
        assert!(is_boot_banner(b"** hgSDK-v1.6.4.3-28977"));
        assert!(!is_boot_banner(b"hgSDK-v1.6.4.3"));
    }

    #[test]
    fn a_config_dump_mac_parses_with_and_without_spaces() {
        assert_eq!(
            parse_mac_after(b"  addr:12:fd:11:64:98:78", b"addr:"),
            Some([0x12, 0xFD, 0x11, 0x64, 0x98, 0x78])
        );
        assert_eq!(
            parse_mac_after(b"addr:  82:59:13:71:5E:a0, more", b"addr:"),
            Some([0x82, 0x59, 0x13, 0x71, 0x5E, 0xA0])
        );
        assert_eq!(parse_mac_after(b"addr: 12:fd:11", b"addr:"), None);
        assert_eq!(parse_mac_after(b"no marker here", b"addr:"), None);
    }

    #[test]
    fn line_contains_finds_tokens_in_dump_lines() {
        assert!(line_contains(b"ROLE   :group", b"group"));
        assert!(line_contains(b"chan_list 9080/9160/9240", b"9240"));
        assert!(!line_contains(b"role:sta", b"group"));
    }
}
