//! Clip-deck: bankok, kvantált indítás, autopilot, crossfade.
//!
//! Ez a VJ-munkafolyamat magja (Arkaos-szerű):
//!   * minden layer (= deck) egy clip-bank,
//!   * egy clip indítása a zenei rácsra kvantálva történik,
//!   * az autopilot N ütem után automatikusan továbblép,
//!   * a váltás lehet azonnali vagy crossfade-dal.
//!
//! A modul itt szándékosan GPU-mentes és időmentes: zenei időben (negyed)
//! dolgozik, így determinisztikusan tesztelhető.

use crate::quantize::{next_grid_strict, Quantize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    Normal,
    Add,
    Screen,
    Multiply,
    Difference,
    Lighten,
    Darken,
}

impl Default for BlendMode {
    fn default() -> Self {
        BlendMode::Normal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopMode {
    Loop,
    Once,
    PingPong,
}

impl Default for LoopMode {
    fn default() -> Self {
        LoopMode::Loop
    }
}

/// Klipp-szintű chroma key felülírás módja.
///
/// A deck szintű chroma key az alapértelmezés; a klip ezt felülírhatja.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaMode {
    /// A deck beállításait örökli (alapértelmezés).
    Inherit,
    /// A klip saját kulcsszíne/paraméterei élnek.
    Custom,
    /// Ezen a klipen nincs kulcs, akkor sem, ha a deck kulcsa be van kapcsolva.
    Off,
}

impl Default for ChromaMode {
    fn default() -> Self {
        Self::Inherit
    }
}

impl ChromaMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ChromaMode::Inherit => "inherit",
            ChromaMode::Custom => "custom",
            ChromaMode::Off => "off",
        }
    }

    pub fn from_name(name: Option<&str>) -> Self {
        match name {
            Some("custom") => ChromaMode::Custom,
            Some("off") => ChromaMode::Off,
            _ => ChromaMode::Inherit,
        }
    }
}

/// Honnan jön a clip képe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipSource {
    None,
    File(String),
    Shader(String),
    AudioVisual,
    TestPattern,
}

#[derive(Debug, Clone)]
pub struct Clip {
    pub name: String,
    pub source: ClipSource,
    /// Kezdőpont a médiában (másodperc).
    pub in_point: f64,
    /// Lejátszási sebesség (1.0 = eredeti).
    pub speed: f64,
    pub opacity: f32,
    pub blend: BlendMode,
    pub loop_mode: LoopMode,
    /// Ha > 0, a klip hosszát ehhez az ütemszámhoz igazítja a BPM-hez
    /// (a lejátszási sebesség automatikus állításával).
    pub snap_beats: f64,
    /// Súly az autopilot véletlen választásához.
    pub weight: f32,
    /// Klipp-szintű chroma key felülírás módja (deck alap felett).
    pub chroma_mode: ChromaMode,
    /// A klip saját kulcsszíne sRGB-ben (Custom módban él).
    pub chroma_color: [f32; 3],
    pub chroma_tolerance: f32,
    pub chroma_softness: f32,
    pub chroma_spill: f32,
}

impl Clip {
    /// A BPM-hez igazított hosszhoz tartozó lejátszási sebesség.
    /// 0 vagy negatív beats esetén nincs igazítás (1.0).
    pub fn snap_speed(duration_secs: f64, in_point: f64, beats: f64, bpm: f64) -> f64 {
        if beats <= 0.0 || bpm <= 0.0 || duration_secs <= 0.0 {
            return 1.0;
        }
        let media = (duration_secs - in_point).max(0.05);
        (media * bpm / (beats * 60.0)).clamp(0.05, 8.0)
    }

    pub fn new(name: impl Into<String>, source: ClipSource) -> Self {
        Self {
            name: name.into(),
            source,
            in_point: 0.0,
            speed: 1.0,
            opacity: 1.0,
            blend: BlendMode::Normal,
            loop_mode: LoopMode::Loop,
            snap_beats: 0.0,
            weight: 1.0,
            chroma_mode: ChromaMode::Inherit,
            chroma_color: [0.0, 1.0, 0.0],
            chroma_tolerance: 0.28,
            chroma_softness: 0.12,
            chroma_spill: 0.35,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutopilotMode {
    Off,
    Sequential,
    Shuffle,
    Weighted,
}

/// A kézi „Következő klip” gomb viselkedése — független az autopilottól.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualNextMode {
    Sequential,
    Shuffle,
}

impl Default for ManualNextMode {
    fn default() -> Self {
        Self::Sequential
    }
}

#[derive(Debug, Clone)]
pub struct Autopilot {
    pub mode: AutopilotMode,
    /// Ennyi ütem után lép tovább.
    pub every_beats: f64,
    /// A váltás kvantálása.
    pub quantize: Quantize,
    /// Mikor esedékes a következő automatikus váltás (zenei idő).
    pub next_change: Option<f64>,
}

impl Default for Autopilot {
    fn default() -> Self {
        Self {
            mode: AutopilotMode::Off,
            every_beats: 16.0,
            quantize: Quantize::Bar,
            next_change: None,
        }
    }
}

/// Egy ütemezett indítás.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Queued {
    pub clip: usize,
    pub at_beat: f64,
    pub from: Option<usize>,
    /// Kézi indítás (nem az autopilot tette be).
    pub manual: bool,
}

#[derive(Debug, Clone)]
pub struct Deck {
    pub name: String,
    pub clips: Vec<Clip>,
    pub active: Option<usize>,
    pub queued: Option<Queued>,
    pub autopilot: Autopilot,
    /// A kézi „Következő klip” gomb módja (autopilottól független).
    pub manual_next: ManualNextMode,
    /// Deck-szintű BPM-hossz igazítás ütemekben (0 = kikapcsolva).
    /// Ha > 0, a deck összes klipje ehhez igazodik.
    pub bpm_sync: f64,
    /// Crossfade hossza ütemekben (0 = azonnali vágás).
    pub fade_beats: f64,
    /// Az éppen lecsengő előző klip.
    pub fading_from: Option<usize>,
    pub fade_remaining: f64,
    pub opacity: f32,
    pub blend: BlendMode,
    /// Deck-szintű chroma key (színkivétel): a kulcsszínhez közeli pixelek
    /// átlátszóvá válnak az adott deck videó-rétegén (ArKaOS-stílus).
    pub chroma_key: bool,
    /// A kivett kulcsszín sRGB-ben (0..1).
    pub chroma_color: [f32; 3],
    /// Mennyire térhet el a kulcsszíntől (0 = semmi, 1 = nagyon megengedő).
    pub chroma_tolerance: f32,
    /// A kulcs szélének lágysága (0 = kemény szél).
    pub chroma_softness: f32,
    /// Despill: a kulcsszínhez közeli, megtartott pixelek telítettség-csökkentése.
    pub chroma_spill: f32,
    pub launched_count: u64,
    rng: u64,
}

impl Deck {
    pub fn new(name: impl Into<String>, clips: Vec<Clip>) -> Self {
        Self {
            name: name.into(),
            clips,
            active: None,
            queued: None,
            autopilot: Autopilot::default(),
            manual_next: ManualNextMode::default(),
            bpm_sync: 0.0,
            fade_beats: 0.0,
            fading_from: None,
            fade_remaining: 0.0,
            opacity: 1.0,
            blend: BlendMode::Normal,
            chroma_key: false,
            chroma_color: [0.0, 1.0, 0.0],
            chroma_tolerance: 0.28,
            chroma_softness: 0.12,
            chroma_spill: 0.35,
            launched_count: 0,
            rng: 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// A deck chroma key beállításai: (bekapcsolva, szín, tolerance, softness, spill).
    pub fn chroma(&self) -> (bool, [f32; 3], f32, f32, f32) {
        (
            self.chroma_key,
            self.chroma_color,
            self.chroma_tolerance,
            self.chroma_softness,
            self.chroma_spill,
        )
    }

    /// Egy klip **effektív** chroma beállításai a deck-alap és a klip
    /// felülírása alapján:
    ///   * Inherit -> a deck beállításai,
    ///   * Custom  -> a klip saját kulcsszíne és paraméterei (bekapcsolva),
    ///   * Off     -> ezen a klipen nincs kulcs.
    ///
    /// clip = None (vagy érvénytelen index) esetén a deck beállításai.
    pub fn chroma_for_clip(&self, clip: Option<usize>) -> (bool, [f32; 3], f32, f32, f32) {
        let deck = self.chroma();
        let Some(c) = clip.and_then(|i| self.clips.get(i)) else {
            return deck;
        };
        match c.chroma_mode {
            ChromaMode::Inherit => deck,
            ChromaMode::Custom => (
                true,
                c.chroma_color,
                c.chroma_tolerance,
                c.chroma_softness,
                c.chroma_spill,
            ),
            ChromaMode::Off => (false, deck.1, deck.2, deck.3, deck.4),
        }
    }

    pub fn clip_count(&self) -> usize {
        self.clips.len()
    }

    /// A crossfade pillanatnyi értéke (0 = teljesen az előző, 1 = az új clip).
    pub fn fade_progress(&self) -> f32 {
        if self.fade_beats <= 0.0 || self.fading_from.is_none() {
            return 1.0;
        }
        let done = 1.0 - (self.fade_remaining / self.fade_beats).clamp(0.0, 1.0);
        done as f32
    }

    fn next_rng(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Az autopilot által választott következő clip indexe.
    pub fn pick_next(&mut self) -> Option<usize> {
        if self.clips.is_empty() {
            return None;
        }
        let n = self.clips.len();
        match self.autopilot.mode {
            AutopilotMode::Off => None,
            AutopilotMode::Sequential => {
                Some(self.active.map(|a| (a + 1) % n).unwrap_or(0))
            }
            AutopilotMode::Shuffle => {
                let r = (self.next_rng() % n as u64) as usize;
                Some(r)
            }
            AutopilotMode::Weighted => {
                let total: f32 = self.clips.iter().map(|c| c.weight.max(0.0)).sum();
                if total <= 0.0 {
                    return Some(0);
                }
                let mut t = (self.next_rng() % 10000) as f32 / 10000.0 * total;
                for (i, c) in self.clips.iter().enumerate() {
                    t -= c.weight.max(0.0);
                    if t <= 0.0 {
                        return Some(i);
                    }
                }
                Some(n - 1)
            }
        }
    }

    /// A kézi „Következő klip” gomb által választott index. Ez független
    /// az autopilot shuffle/sequential beállításától.
    pub fn pick_manual_next(&mut self) -> Option<usize> {
        let n = self.clips.len();
        if n == 0 {
            return None;
        }
        match self.manual_next {
            ManualNextMode::Sequential => {
                Some(self.active.map(|a| (a + 1) % n).unwrap_or(0))
            }
            ManualNextMode::Shuffle => {
                if n == 1 {
                    return Some(0);
                }
                let mut r = (self.next_rng() % n as u64) as usize;
                if Some(r) == self.active {
                    r = (r + 1) % n;
                }
                Some(r)
            }
        }
    }
}

/// A teljes mixer: a layerek.
#[derive(Debug, Clone)]
pub struct Mixer {
    pub decks: Vec<Deck>,
    pub master_opacity: f32,
    pub blackout: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeckEvent {
    Launch {
        deck: usize,
        clip: usize,
        at_beat: f64,
        previous: Option<usize>,
    },
    FadeDone {
        deck: usize,
    },
}

impl Mixer {
    pub fn new(deck_count: usize) -> Self {
        Self {
            decks: (0..deck_count)
                .map(|i| Deck::new(format!("Layer {}", (b'A' + i as u8) as char), Vec::new()))
                .collect(),
            master_opacity: 1.0,
            blackout: false,
        }
    }

    /// Clip indításának ütemezése a következő rácsra.
    ///
    /// Visszaadja, hányadik negyedre került (None, ha érvénytelen).
    pub fn queue(
        &mut self,
        deck: usize,
        clip: usize,
        now_beats: f64,
        quantize: Quantize,
    ) -> Option<f64> {
        let d = self.decks.get_mut(deck)?;
        if clip >= d.clips.len() {
            return None;
        }
        let at = match quantize.grid_beats() {
            Some(grid) => next_grid_strict(now_beats, grid),
            None => now_beats,
        };
        d.queued = Some(Queued {
            clip,
            at_beat: at,
            from: d.active,
            manual: true,
        });
        Some(at)
    }

    /// A zenei idővonal hirtelen eltolásakor (beat reset, tap fázis, MIDI
    /// Start, Link ugrás) a már betervezett autopilot/queue időpontokat is
    /// ugyanazzal a deltával toljuk, hogy ne akadjon meg a rotáció.
    pub fn shift_schedule(&mut self, delta_beats: f64) {
        if !delta_beats.is_finite() || delta_beats.abs() < 1e-9 {
            return;
        }
        for deck in &mut self.decks {
            if let Some(next) = deck.autopilot.next_change.as_mut() {
                *next += delta_beats;
            }
            if let Some(q) = deck.queued.as_mut() {
                q.at_beat += delta_beats;
            }
        }
    }

    /// Teljesen azonnali indítás (vészhelyzeti / kézi).
    pub fn trigger_now(&mut self, deck: usize, clip: usize, now_beats: f64) -> Option<f64> {
        let d = self.decks.get_mut(deck)?;
        if clip >= d.clips.len() {
            return None;
        }
        d.queued = Some(Queued {
            clip,
            at_beat: now_beats,
            from: d.active,
            manual: true,
        });
        Some(now_beats)
    }

    /// Időléptetés. Minden esedékes indítást végrehajt, és futtatja az autopilotot.
    pub fn tick(&mut self, now_beats: f64, dt_beats: f64) -> Vec<DeckEvent> {
        let mut events = Vec::new();
        for (i, deck) in self.decks.iter_mut().enumerate() {
            // Crossfade léptetése.
            if deck.fading_from.is_some() {
                deck.fade_remaining -= dt_beats;
                if deck.fade_remaining <= 0.0 {
                    deck.fade_remaining = 0.0;
                    deck.fading_from = None;
                    events.push(DeckEvent::FadeDone { deck: i });
                }
            }

            // Esedékes indítás.
            let mut launched = false;
            let mut launched_at = None;
            if let Some(q) = deck.queued {
                if now_beats >= q.at_beat - 1e-9 {
                    let previous = deck.active;
                    launched_at = Some(q.at_beat);
                    deck.active = Some(q.clip);
                    deck.launched_count += 1;
                    if deck.fade_beats > 0.0 && previous.is_some() {
                        deck.fading_from = previous;
                        deck.fade_remaining = deck.fade_beats;
                    } else {
                        deck.fading_from = None;
                        deck.fade_remaining = 0.0;
                    }
                    deck.queued = None;
                    events.push(DeckEvent::Launch {
                        deck: i,
                        clip: q.clip,
                        at_beat: q.at_beat,
                        previous,
                    });
                    launched = true;
                }
            }

            // Autopilot: indítás után beütemezi a következő váltást.
            if deck.autopilot.mode != AutopilotMode::Off {
                if launched {
                    // Az ütemezett időpontból számolunk, nem a pillanatnyiból.
                    let base = launched_at.unwrap_or(now_beats);
                    deck.autopilot.next_change = Some(base + deck.autopilot.every_beats);
                } else if deck.autopilot.next_change.is_none() {
                    if deck.active.is_some() {
                        deck.autopilot.next_change = Some(now_beats + deck.autopilot.every_beats);
                    } else if !deck.clips.is_empty() {
                        // Első automatikus indítás.
                        deck.autopilot.next_change = Some(now_beats);
                    }
                }
                // Ha esedékes és nincs már ütemezett indítás, beteszünk egyet.
                if deck.queued.is_none() {
                    if let Some(due) = deck.autopilot.next_change {
                        if now_beats >= due - 1e-9 {
                            let q = deck.autopilot.quantize;
                            if let Some(next) = deck.pick_next() {
                                // Az autopilot automatikus: ha pontosan a rácsra esik,
                                // nem lökjük tovább (az a kézi indítás szabálya).
                                let at = match q.grid_beats() {
                                    Some(grid) => crate::quantize::grid_floor(now_beats, grid),
                                    None => now_beats,
                                };
                                deck.queued = Some(Queued {
                                    clip: next,
                                    at_beat: at,
                                    from: deck.active,
                                    manual: false,
                                });
                                // A következő esedékesség a most ütemezett indítás után.
                                deck.autopilot.next_change = Some(at + deck.autopilot.every_beats);
                            }
                        }
                    }
                }
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bank(n: usize) -> Vec<Clip> {
        (0..n)
            .map(|i| Clip::new(format!("clip{i}"), ClipSource::TestPattern))
            .collect()
    }

    #[test]
    fn launch_lands_on_the_grid() {
        let mut m = Mixer::new(1);
        m.decks[0].clips = bank(3);
        // 3.4 negyednél nyomok egy gombot 4 negyedes kvantálással.
        let at = m.queue(0, 1, 3.4, Quantize::Bar).unwrap();
        assert!((at - 4.0).abs() < 1e-9, "at = {at}");
        // 4.0 előtt még nincs indítás.
        let ev = m.tick(3.9, 0.1);
        assert!(ev.is_empty());
        let ev = m.tick(4.0, 0.1);
        assert_eq!(ev.len(), 1);
        assert!(matches!(ev[0], DeckEvent::Launch { clip: 1, .. }));
        assert_eq!(m.decks[0].active, Some(1));
    }

    #[test]
    fn auto_advance_every_n_beats() {
        let mut m = Mixer::new(1);
        m.decks[0].clips = bank(4);
        m.decks[0].autopilot.mode = AutopilotMode::Sequential;
        m.decks[0].autopilot.every_beats = 8.0;
        m.decks[0].autopilot.quantize = Quantize::Bar;
        // 48 negyed szimulálása 1/16-os lépésekben.
        let mut launches = Vec::new();
        let mut t = 0.0f64;
        while t < 48.0 {
            for e in m.tick(t, 0.25) {
                if let DeckEvent::Launch { clip, at_beat, .. } = e {
                    launches.push((at_beat, clip));
                }
            }
            t += 0.25;
        }
        assert!(launches.len() >= 5, "indítások: {launches:?}");
        // Az indítások 4 negyedre esnek (1 bar kvantálás).
        for (at, _) in &launches {
            assert!((at / 4.0 - (at / 4.0).round()).abs() < 1e-6, "at = {at}");
        }
        // Sorrendes: 0,1,2,3,0,...
        let clips: Vec<usize> = launches.iter().map(|(_, c)| *c).collect();
        assert_eq!(&clips[..4], &[0, 1, 2, 3]);
        assert_eq!(clips[4], 0);
    }

    #[test]
    fn fade_completes() {
        let mut m = Mixer::new(1);
        m.decks[0].clips = bank(2);
        m.decks[0].fade_beats = 2.0;
        m.trigger_now(0, 0, 0.0);
        m.tick(0.0, 0.0);
        m.trigger_now(0, 1, 0.0);
        let ev = m.tick(0.0, 0.0);
        assert!(matches!(ev[0], DeckEvent::Launch { .. }));
        assert_eq!(m.decks[0].fading_from, Some(0));
        assert!((m.decks[0].fade_progress() - 0.0).abs() < 1e-6);
        m.tick(1.0, 1.0);
        assert!((m.decks[0].fade_progress() - 0.5).abs() < 1e-6);
        let ev = m.tick(2.0, 1.0);
        assert!(ev.iter().any(|e| matches!(e, DeckEvent::FadeDone { .. })));
        assert_eq!(m.decks[0].fading_from, None);
        assert!((m.decks[0].fade_progress() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn manual_queue_beats_autopilot() {
        let mut m = Mixer::new(1);
        m.decks[0].clips = bank(4);
        m.decks[0].autopilot.mode = AutopilotMode::Sequential;
        m.decks[0].autopilot.every_beats = 4.0;
        m.decks[0].autopilot.quantize = Quantize::Beat;
        // Kézzel beteszünk egy clipet a 2. negyedre.
        m.queue(0, 2, 1.0, Quantize::Beat);
        let ev = m.tick(2.0, 0.1);
        assert!(ev
            .iter()
            .any(|e| matches!(e, DeckEvent::Launch { clip: 2, .. })));
        assert_eq!(m.decks[0].active, Some(2));
    }

    #[test]
    fn manual_next_is_independent_of_autopilot() {
        let mut d = Deck::new("A", bank(5));
        d.active = Some(2);
        d.autopilot.mode = AutopilotMode::Shuffle;
        d.manual_next = ManualNextMode::Sequential;
        assert_eq!(d.pick_manual_next(), Some(3));

        d.manual_next = ManualNextMode::Shuffle;
        for _ in 0..50 {
            let n = d.pick_manual_next().unwrap();
            assert!(n < 5);
            assert_ne!(n, 2, "a kézi random ne ismételje önmagát");
        }
    }

    #[test]
    fn shift_schedule_preserves_relative_timing() {
        let mut m = Mixer::new(1);
        m.decks[0].clips = bank(3);
        m.decks[0].autopilot.next_change = Some(8.0);
        m.queue(0, 1, 0.0, Quantize::Beat);
        m.shift_schedule(-1.0);
        assert!((m.decks[0].autopilot.next_change.unwrap() - 7.0).abs() < 1e-9);
        assert!((m.decks[0].queued.unwrap().at_beat - 0.0).abs() < 1e-9);
        let ev = m.tick(0.0, 0.0);
        assert!(ev.iter().any(|e| matches!(e, DeckEvent::Launch { clip: 1, .. })));
    }

    #[test]
    fn invalid_clip_is_rejected() {
        let mut m = Mixer::new(1);
        m.decks[0].clips = bank(2);
        assert!(m.queue(0, 5, 0.0, Quantize::Bar).is_none());
        assert!(m.queue(9, 0, 0.0, Quantize::Bar).is_none());
    }

    #[test]
    fn snap_speed_fits_clip_to_beats() {
        // 8 másodperces klip 128 BPM-en 4 ütem = 1.875 s -> 4.27x.
        let speed = Clip::snap_speed(8.0, 0.0, 4.0, 128.0);
        assert!((speed - 4.2667).abs() < 0.001, "speed = {speed}");
        // 4 ütem 120 BPM-en 2 s, 8 s klip -> 4x.
        assert!((Clip::snap_speed(8.0, 0.0, 4.0, 120.0) - 4.0).abs() < 1e-9);
        assert_eq!(Clip::snap_speed(8.0, 0.0, 0.0, 120.0), 1.0);
    }

    #[test]
    fn weighted_picks_never_zero_weight() {
        let mut d = Deck::new("t", bank(3));
        d.clips[0].weight = 0.0;
        d.clips[1].weight = 0.0;
        d.clips[2].weight = 5.0;
        d.autopilot.mode = AutopilotMode::Weighted;
        for _ in 0..20 {
            assert_eq!(d.pick_next(), Some(2));
        }
    }

    #[test]
    fn chroma_key_defaults_to_off_green() {
        let d = Deck::new("t", bank(1));
        let (on, color, tol, soft, spill) = d.chroma();
        assert!(!on);
        assert_eq!(color, [0.0, 1.0, 0.0]);
        assert!((tol - 0.28).abs() < 1e-6);
        assert!((soft - 0.12).abs() < 1e-6);
        assert!((spill - 0.35).abs() < 1e-6);
    }

    #[test]
    fn chroma_for_clip_resolves_inherit_custom_off() {
        let mut d = Deck::new("t", bank(2));
        d.chroma_key = true;
        d.chroma_color = [0.1, 0.9, 0.2];
        d.chroma_tolerance = 0.3;
        d.chroma_softness = 0.11;
        d.chroma_spill = 0.4;

        // Alap: öröklés -> deck beállítások.
        assert_eq!(d.clips[0].chroma_mode, ChromaMode::Inherit);
        assert_eq!(d.chroma_for_clip(Some(0)), d.chroma());
        // Nincs klip / érvénytelen index -> deck.
        assert_eq!(d.chroma_for_clip(None), d.chroma());
        assert_eq!(d.chroma_for_clip(Some(9)), d.chroma());

        // Custom: a klip saját értékei, bekapcsolva.
        d.clips[1].chroma_mode = ChromaMode::Custom;
        d.clips[1].chroma_color = [1.0, 0.0, 0.0];
        d.clips[1].chroma_tolerance = 0.5;
        d.clips[1].chroma_softness = 0.2;
        d.clips[1].chroma_spill = 0.6;
        let (on, color, tol, soft, spill) = d.chroma_for_clip(Some(1));
        assert!(on);
        assert_eq!(color, [1.0, 0.0, 0.0]);
        assert!((tol - 0.5).abs() < 1e-6);
        assert!((soft - 0.2).abs() < 1e-6);
        assert!((spill - 0.6).abs() < 1e-6);

        // Off: deck kulcs ellenére sincs kulcs a klipen.
        d.clips[0].chroma_mode = ChromaMode::Off;
        assert!(!d.chroma_for_clip(Some(0)).0);
        // A deck beállítása és a másik klip öröklése változatlan.
        assert!(d.chroma().0);
        assert_eq!(d.chroma_for_clip(Some(0)).1, d.chroma_color);
        // Visszaállítás öröklésre.
        d.clips[0].chroma_mode = ChromaMode::Inherit;
        assert!(d.chroma_for_clip(Some(0)).0);
    }

    #[test]
    fn chroma_mode_names_round_trip() {
        for mode in [ChromaMode::Inherit, ChromaMode::Custom, ChromaMode::Off] {
            assert_eq!(ChromaMode::from_name(Some(mode.as_str())), mode);
        }
        assert_eq!(ChromaMode::from_name(None), ChromaMode::Inherit);
        assert_eq!(ChromaMode::from_name(Some("ismeretlen")), ChromaMode::Inherit);
    }
}
