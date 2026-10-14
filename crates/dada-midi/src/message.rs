//! MIDI üzenetek és byte-szintű parser (running status támogatással).
//!
//! A parser szándékosan nem függ semmilyen platform-könyvtártól, így
//! determinisztikusan tesztelhető: a hardveres réteg (midir) csak byte-okat ad át.

/// Egy MIDI üzenet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiMessage {
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8, velocity: u8 },
    PolyAftertouch { channel: u8, note: u8, pressure: u8 },
    ControlChange { channel: u8, cc: u8, value: u8 },
    ProgramChange { channel: u8, program: u8 },
    ChannelAftertouch { channel: u8, pressure: u8 },
    PitchBend { channel: u8, value: i16 },
    Clock,
    Start,
    Continue,
    Stop,
    SongPosition(u16),
    ActiveSensing,
    Reset,
}

impl MidiMessage {
    pub fn channel(&self) -> Option<u8> {
        use MidiMessage::*;
        Some(match self {
            NoteOn { channel, .. }
            | NoteOff { channel, .. }
            | PolyAftertouch { channel, .. }
            | ControlChange { channel, .. }
            | ProgramChange { channel, .. }
            | ChannelAftertouch { channel, .. }
            | PitchBend { channel, .. } => *channel,
            _ => return None,
        })
    }
}

/// Állapotgépes parser. Kezeli a running status-t (gyakori clock-stream mellett).
#[derive(Default)]
pub struct MidiParser {
    running_status: Option<u8>,
    pending: Vec<u8>,
    in_sysex: bool,
}

impl MidiParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.running_status = None;
        self.pending.clear();
        self.in_sysex = false;
    }

    /// Nyers byte-ok feldolgozása; a kész üzeneteket adja vissza.
    pub fn parse(&mut self, bytes: &[u8]) -> Vec<MidiMessage> {
        let mut out = Vec::new();
        for &b in bytes {
            if self.in_sysex {
                if b == 0xF7 {
                    self.in_sysex = false;
                }
                continue;
            }
            if b >= 0xF8 {
                // Real-time üzenetek a stream bármely pontján jöhetnek.
                match b {
                    0xF8 => out.push(MidiMessage::Clock),
                    0xFA => out.push(MidiMessage::Start),
                    0xFB => out.push(MidiMessage::Continue),
                    0xFC => out.push(MidiMessage::Stop),
                    0xFE => out.push(MidiMessage::ActiveSensing),
                    0xFF => out.push(MidiMessage::Reset),
                    _ => {}
                }
                continue;
            }
            if b == 0xF0 {
                self.in_sysex = true;
                self.pending.clear();
                continue;
            }
            if b >= 0x80 {
                self.pending.clear();
                self.running_status = Some(b);
                continue;
            }
            // Data byte.
            let status = match self.running_status {
                Some(s) => s,
                None => continue,
            };
            self.pending.push(b);
            let needed = data_bytes(status);
            if needed > 0 && self.pending.len() >= needed {
                let data: Vec<u8> = self.pending.drain(..needed).collect();
                if let Some(m) = build(status, &data) {
                    out.push(m);
                }
                // System Common üzeneteknél a running status megszűnik.
                if status < 0xF0 && !is_two_byte(status) {
                    // ide nem jutunk
                }
                if (0xF1..0xF7).contains(&status) {
                    self.running_status = None;
                }
            }
        }
        out
    }
}

fn is_two_byte(_status: u8) -> bool {
    false
}

/// Hány adatbájt tartozik a státuszhoz.
fn data_bytes(status: u8) -> usize {
    match status & 0xF0 {
        0x80 | 0x90 | 0xA0 | 0xB0 | 0xE0 => 2,
        0xC0 | 0xD0 => 1,
        _ => match status {
            0xF1 => 1,
            0xF2 => 2,
            0xF3 => 1,
            _ => 0,
        },
    }
}

fn build(status: u8, data: &[u8]) -> Option<MidiMessage> {
    let channel = status & 0x0F;
    match status & 0xF0 {
        0x80 => Some(MidiMessage::NoteOff {
            channel,
            note: data[0],
            velocity: data[1],
        }),
        0x90 => {
            if data[1] == 0 {
                Some(MidiMessage::NoteOff {
                    channel,
                    note: data[0],
                    velocity: 0,
                })
            } else {
                Some(MidiMessage::NoteOn {
                    channel,
                    note: data[0],
                    velocity: data[1],
                })
            }
        }
        0xA0 => Some(MidiMessage::PolyAftertouch {
            channel,
            note: data[0],
            pressure: data[1],
        }),
        0xB0 => Some(MidiMessage::ControlChange {
            channel,
            cc: data[0],
            value: data[1],
        }),
        0xC0 => Some(MidiMessage::ProgramChange {
            channel,
            program: data[0],
        }),
        0xD0 => Some(MidiMessage::ChannelAftertouch {
            channel,
            pressure: data[0],
        }),
        0xE0 => {
            let value = ((data[1] as i16) << 7 | data[0] as i16) - 8192;
            Some(MidiMessage::PitchBend { channel, value })
        }
        _ => match status {
            0xF2 => Some(MidiMessage::SongPosition(
                (data[1] as u16) << 7 | data[0] as u16,
            )),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_note_on_and_off() {
        let mut p = MidiParser::new();
        let msgs = p.parse(&[0x90, 36, 100, 0x80, 36, 0]);
        assert_eq!(msgs.len(), 2);
        assert_eq!(
            msgs[0],
            MidiMessage::NoteOn {
                channel: 0,
                note: 36,
                velocity: 100
            }
        );
        assert_eq!(
            msgs[1],
            MidiMessage::NoteOff {
                channel: 0,
                note: 36,
                velocity: 0
            }
        );
    }

    #[test]
    fn note_on_velocity_zero_is_note_off() {
        let mut p = MidiParser::new();
        let msgs = p.parse(&[0x90, 40, 0]);
        assert!(matches!(msgs[0], MidiMessage::NoteOff { note: 40, .. }));
    }

    #[test]
    fn running_status_works() {
        let mut p = MidiParser::new();
        // Egy NoteOn status, majd két pár adat running status-szal.
        let msgs = p.parse(&[0x90, 36, 100, 38, 90, 40, 80]);
        assert_eq!(msgs.len(), 3);
        assert!(matches!(msgs[1], MidiMessage::NoteOn { note: 38, .. }));
        assert!(matches!(msgs[2], MidiMessage::NoteOn { note: 40, .. }));
    }

    #[test]
    fn clock_is_interleaved_with_notes() {
        let mut p = MidiParser::new();
        let msgs = p.parse(&[0xF8, 0xF8, 0x90, 36, 100, 0xF8]);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0], MidiMessage::Clock);
        assert_eq!(msgs[1], MidiMessage::Clock);
        assert!(matches!(msgs[2], MidiMessage::NoteOn { .. }));
        assert_eq!(msgs[3], MidiMessage::Clock);
    }

    #[test]
    fn parses_cc_and_spp() {
        let mut p = MidiParser::new();
        let msgs = p.parse(&[0xB0, 1, 64, 0xF2, 0x10, 0x02]);
        assert_eq!(
            msgs[0],
            MidiMessage::ControlChange {
                channel: 0,
                cc: 1,
                value: 64
            }
        );
        assert_eq!(msgs[1], MidiMessage::SongPosition(0x02 * 128 + 0x10));
    }

    #[test]
    fn transport_messages() {
        let mut p = MidiParser::new();
        let msgs = p.parse(&[0xFA, 0xFC, 0xFB]);
        assert_eq!(msgs[0], MidiMessage::Start);
        assert_eq!(msgs[1], MidiMessage::Stop);
        assert_eq!(msgs[2], MidiMessage::Continue);
    }

    #[test]
    fn sysex_is_skipped() {
        let mut p = MidiParser::new();
        let msgs = p.parse(&[0xF0, 0x7E, 0x00, 0x06, 0x01, 0xF7, 0x90, 36, 100]);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], MidiMessage::NoteOn { .. }));
    }

    #[test]
    fn split_messages_across_calls() {
        let mut p = MidiParser::new();
        let a = p.parse(&[0x90, 36]);
        assert!(a.is_empty());
        let b = p.parse(&[100]);
        assert_eq!(b.len(), 1);
    }
}
