//! Projektfájl: a teljes felállás mentése és betöltése.
//!
//! A formátum szándékosan a parancssori szintaxist tükrözi (TOML), hogy
//! kézzel is szerkeszthető legyen, és a CLI-vel azonos parsereket használjon.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const PROJECT_VERSION: u32 = 1;

fn default_version() -> u32 {
    PROJECT_VERSION
}

/// A teljes projekt.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Project {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shader: Option<String>,
    #[serde(default)]
    pub audio: AudioConfig,
    #[serde(default)]
    pub decks: Vec<DeckConfig>,
    #[serde(default)]
    pub mods: Vec<ModConfig>,
    /// A deck-rétegek Z-sorrendje alulról felfelé (deck indexek).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer_order: Option<Vec<usize>>,
    /// Az ISF effekt cél rétege: None = legfelső kompozit, Some(N) = a deck N
    /// rétegét cseréli le az effekt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shader_target: Option<usize>,
    /// "coverage" (opacity fedés) vagy "shader" (shader saját alpha / kulcsozás).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shader_alpha: Option<String>,
    /// Az ISF effekt blend módja.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shader_blend: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct OutputConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vsync: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serve: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AudioConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

/// Egy deck (= layer) beállítása.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct DeckConfig {
    /// Mappa, amiből a clip-bank jön (vagy egyetlen fájl).
    pub folder: String,
    /// Egyedi klip-beállítások (sebesség, loop, BPM snap, in-point).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<ClipConfig>,
    /// Melyik klip kezdődjön (index).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_clip: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autopilot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every_beats: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantize: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_beats: Option<f64>,
    /// Deck-szintű BPM sync ütemszám (0 = kikapcsolva).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm_sync: Option<f64>,
    /// A kézi következő-klip gomb módja: "sequential" / "shuffle".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_next: Option<String>,
    /// Deck-szintű chroma key (színkivétel) bekapcsolva.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_key: Option<bool>,
    /// A kivett kulcsszín sRGB-ben (0..1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_color: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_tolerance: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_softness: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_spill: Option<f32>,
}

/// Egy klip mentett beállításai.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ClipConfig {
    /// A klip neve (a fájlnév), ami alapján vissza lehet párosítani.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_point: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snap_beats: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<f32>,
    /// Klipp-szintű chroma override módja: "inherit" / "custom" / "off".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_mode: Option<String>,
    /// A klip saját kulcsszíne sRGB-ben (custom mód).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_color: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_tolerance: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_softness: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_spill: Option<f32>,
}

impl Project {
    /// A parancssori argumentumokból összeállított projekt (mentéshez).
    pub fn from_cli(args: &crate::Args, loaded: Option<&Project>) -> Self {
        let mut p = loaded.cloned().unwrap_or_default();
        p.version = PROJECT_VERSION;
        p.output.size = Some(format!("{}x{}", args.width, args.height));
        p.output.vsync = Some(args.vsync);
        p.output.serve = Some(args.serve);
        p.output.port = Some(args.port);
        p.shader = args.shader.clone().or(p.shader);
        p.audio.enabled = Some(args.audio_visual);
        p.audio.device = args.audio_device.clone().or(p.audio.device);

        if !args.decks.is_empty() {
            p.decks = args
                .decks
                .iter()
                .map(|s| Project::parse_deck_spec(s, args.autopilot_beats).0)
                .collect();
        }
        if let Some(video) = &args.video {
            p.decks = vec![DeckConfig {
                folder: video.clone(),
                autopilot: Some("off".into()),
                every_beats: Some(0.0),
                quantize: Some("beat".into()),
                ..Default::default()
            }];
        }
        if !args.mods.is_empty() {
            p.mods = args
                .mods
                .iter()
                .map(|r| ModConfig { route: r.clone() })
                .collect();
        }
        p
    }
}

/// Egy modulációs útvonal (a CLI-vel azonos szintaxissal).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ModConfig {
    pub route: String,
}

impl Project {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("nem olvasható: {}", path.display()))?;
        Self::from_toml(&text)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = self.to_toml()?;
        std::fs::write(path, text).with_context(|| format!("nem írható: {}", path.display()))?;
        Ok(())
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        toml::from_str(text).context("hibás projektfájl")
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).context("a projekt nem szerializálható")
    }

    /// A deck-specifikáció értelmezése: MAPPA[:UTEM[:BLEND]].
    pub fn parse_deck_spec(spec: &str, default_beats: f64) -> (DeckConfig, bool) {
        let mut parts = spec.split(':');
        let folder = parts.next().unwrap_or(".").to_string();
        let mut manual_blend = false;
        let every: Option<f64> = parts.next().and_then(|s| s.parse().ok());
        let blend = match parts.next() {
            Some(b) => {
                manual_blend = true;
                Some(b.to_string())
            }
            None => None,
        };
        let beats = every.unwrap_or(default_beats);
        (
            DeckConfig {
                folder,
                clips: Vec::new(),
                start_clip: None,
                autopilot: Some(if beats > 0.0 {
                    "sequential".to_string()
                } else {
                    "off".to_string()
                }),
                every_beats: Some(beats),
                quantize: Some("beat".to_string()),
                blend,
                opacity: None,
                fade_beats: None,
                bpm_sync: None,
                manual_next: None,
                chroma_key: None,
                chroma_color: None,
                chroma_tolerance: None,
                chroma_softness: None,
                chroma_spill: None,
            },
            manual_blend,
        )
    }

    /// A deck-specifikáció visszaírása CLI-formára.
    pub fn deck_to_spec(d: &DeckConfig) -> String {
        let mut s = d.folder.clone();
        if let Some(b) = d.every_beats {
            s.push_str(&format!(":{}", b as i64));
        }
        if let Some(b) = &d.blend {
            if b != "normal" {
                s.push_str(&format!(":{b}"));
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_toml() {
        let p = Project {
            version: PROJECT_VERSION,
            output: OutputConfig {
                size: Some("1920x1080".into()),
                vsync: Some(true),
                serve: Some(true),
                port: Some(7777),
            },
            shader: Some("shaders/plasma.isf".into()),
            audio: AudioConfig {
                enabled: Some(true),
                device: Some("default".into()),
            },
            decks: vec![
                DeckConfig {
                    folder: "media/base".into(),
                    clips: vec![ClipConfig {
                        name: "loop.mp4".into(),
                        speed: Some(1.5),
                        in_point: Some(0.25),
                        loop_mode: Some("pingpong".into()),
                        snap_beats: Some(4.0),
                        weight: Some(2.0),
                        chroma_mode: Some("custom".into()),
                        chroma_color: Some([0.3, 0.8, 0.1]),
                        chroma_tolerance: Some(0.35),
                        chroma_softness: Some(0.15),
                        chroma_spill: Some(0.45),
                    }],
                    autopilot: Some("sequential".into()),
                    every_beats: Some(16.0),
                    blend: Some("normal".into()),
                    opacity: Some(1.0),
                    bpm_sync: Some(8.0),
                    manual_next: Some("shuffle".into()),
                    chroma_key: Some(true),
                    chroma_color: Some([0.2, 0.9, 0.3]),
                    chroma_tolerance: Some(0.4),
                    chroma_softness: Some(0.2),
                    chroma_spill: Some(0.5),
                    ..Default::default()
                },
                DeckConfig {
                    folder: "media/overlay".into(),
                    autopilot: Some("sequential".into()),
                    every_beats: Some(8.0),
                    blend: Some("screen".into()),
                    opacity: Some(0.85),
                    ..Default::default()
                },
            ],
            mods: vec![
                ModConfig {
                    route: "note:36->deck.opacity.1:1.0:180".into(),
                },
                ModConfig {
                    route: "audio.bass->shader.param.0".into(),
                },
            ],
            layer_order: Some(vec![1, 0]),
            shader_target: Some(1),
            shader_alpha: Some("shader".into()),
            shader_blend: Some("normal".into()),
        };
        let text = p.to_toml().unwrap();
        let back = Project::from_toml(&text).unwrap();
        assert_eq!(p, back);
        assert!(text.contains("[[decks]]"));
        assert!(text.contains("note:36->deck.opacity.1"));
        assert!(text.contains("layer_order = [\n    1,\n    0,\n]"));
        assert!(text.contains("shader_target = 1"));
        assert!(text.contains("shader_alpha = \"shader\""));
        assert!(text.contains("chroma_key = true"));
        assert!(text.contains("chroma_color = ["));
        assert!(text.contains("chroma_tolerance = 0.4"));
        assert!(text.contains("chroma_mode = \"custom\""));
        assert!(text.contains("chroma_spill = 0.45"));
    }

    #[test]
    fn layer_order_and_shader_target_are_optional() {
        let text = r#"
version = 1
shader = "shaders/mirror.isf"

[[decks]]
folder = "media/base"
"#;
        let p = Project::from_toml(text).unwrap();
        assert_eq!(p.layer_order, None);
        assert_eq!(p.shader_target, None);
        assert_eq!(p.shader_alpha, None);
        assert_eq!(p.shader_blend, None);
    }

    #[test]
    fn minimal_project_parses() {
        let text = r#"
version = 1
shader = "shaders/gradient.isf"

[[decks]]
folder = "media/base"
every_beats = 12.0
blend = "normal"

[[mods]]
route = "lfo:4->master.opacity:0.5"
"#;
        let p = Project::from_toml(text).unwrap();
        assert_eq!(p.version, 1);
        assert_eq!(p.decks.len(), 1);
        assert_eq!(p.decks[0].folder, "media/base");
        assert_eq!(p.decks[0].every_beats, Some(12.0));
        assert_eq!(p.mods.len(), 1);
    }

    #[test]
    fn deck_spec_parsing() {
        let (d, manual) = Project::parse_deck_spec("media/base:16:screen", 8.0);
        assert_eq!(d.folder, "media/base");
        assert_eq!(d.every_beats, Some(16.0));
        assert_eq!(d.blend.as_deref(), Some("screen"));
        assert!(manual);

        let (d2, manual2) = Project::parse_deck_spec("media/base", 8.0);
        assert_eq!(d2.every_beats, Some(8.0));
        assert_eq!(d2.blend, None);
        assert!(!manual2);

        assert_eq!(Project::deck_to_spec(&d), "media/base:16:screen");
        assert_eq!(Project::deck_to_spec(&d2), "media/base:8");
    }
}
