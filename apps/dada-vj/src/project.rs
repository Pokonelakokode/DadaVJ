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
                    autopilot: Some("sequential".into()),
                    every_beats: Some(16.0),
                    blend: Some("normal".into()),
                    opacity: Some(1.0),
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
        };
        let text = p.to_toml().unwrap();
        let back = Project::from_toml(&text).unwrap();
        assert_eq!(p, back);
        assert!(text.contains("[[decks]]"));
        assert!(text.contains("note:36->deck.opacity.1"));
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
