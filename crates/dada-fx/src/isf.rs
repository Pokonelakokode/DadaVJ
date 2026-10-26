//! ISF (Interactive Shader Format) betöltés és fordítás.
//!
//! Az ISF egy JSON fejléc + GLSL fragment shader. A naga GLSL-frontendje a
//! szigorú Vulkan-stílusú GLSL-t fordítja, de a WebGL-es ISF-fajtákat nem
//! (gl_FragColor, varying, sampler2D). Ezért:
//!
//!   1. kiolvassuk a JSON fejlécet,
//!   2. a shader-törzset előfeldolgozzuk (gl_FragColor, RENDERSIZE, TIME, ...),
//!   3. egy szigorú GLSL preambulumot adunk hozzá (explicit location/binding),
//!   4. a naga GLSL->WGSL fordítást végez.
//!
//! Ez a *generátorok* túlnyomó részét lefedi. A képi bemenetet használó
//! effektek (sampler2D) külön fázist kapnak, mert a naga nem támogatja a
//! önálló opaque uniformot.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Egy ISF bemeneti paraméter.
#[derive(Debug, Clone, Deserialize)]
pub struct Input {
    #[serde(rename = "NAME")]
    pub name: String,
    #[serde(rename = "TYPE")]
    pub kind: String,
    #[serde(rename = "DEFAULT")]
    pub default: Option<serde_json::Value>,
    #[serde(rename = "MIN")]
    pub min: Option<f64>,
    #[serde(rename = "MAX")]
    pub max: Option<f64>,
    #[serde(rename = "LABELS")]
    pub labels: Option<Vec<String>>,
    #[serde(rename = "VALUES")]
    pub values: Option<Vec<f64>>,
}

/// Az ISF fejléc.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Metadata {
    #[serde(rename = "DESCRIPTION", default)]
    pub description: String,
    #[serde(rename = "CREDIT", default)]
    pub credit: String,
    #[serde(rename = "CATEGORIES", default)]
    pub categories: Vec<String>,
    #[serde(rename = "ISFVSN", default)]
    pub isf_vsn: String,
    #[serde(rename = "INPUTS", default)]
    pub inputs: Vec<Input>,
    #[serde(rename = "PASSES", default)]
    pub passes: Vec<serde_json::Value>,
}

/// A paraméter típusa a fordító szempontjából.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Float,
    Bool,
    Long,
    Event,
    Point2D,
    Color,
}

impl ParamKind {
    pub fn glsl_type(self) -> &'static str {
        match self {
            ParamKind::Float | ParamKind::Bool | ParamKind::Long | ParamKind::Event => "float",
            ParamKind::Point2D => "vec2",
            ParamKind::Color => "vec4",
        }
    }

    pub fn components(self) -> usize {
        match self {
            ParamKind::Float | ParamKind::Bool | ParamKind::Long | ParamKind::Event => 1,
            ParamKind::Point2D => 2,
            ParamKind::Color => 4,
        }
    }

    /// std140/WGSL igazítás (float=4, vec2=8, vec4=16).
    pub fn alignment(self) -> usize {
        match self {
            ParamKind::Float | ParamKind::Bool | ParamKind::Long | ParamKind::Event => 4,
            ParamKind::Point2D => 8,
            ParamKind::Color => 16,
        }
    }

    pub fn size(self) -> usize {
        match self {
            ParamKind::Float | ParamKind::Bool | ParamKind::Long | ParamKind::Event => 4,
            ParamKind::Point2D => 8,
            ParamKind::Color => 16,
        }
    }
}

/// Egy fordítható paraméter.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub kind: ParamKind,
    pub min: f32,
    pub max: f32,
    pub default: [f32; 4],
    pub labels: Vec<String>,
    pub values: Vec<f32>,
    /// Eltolás az uniform pufferben.
    pub offset: usize,
}

/// Az ISF-fájl által használt, hordozható típusok.
fn param_kind(kind: &str) -> Option<ParamKind> {
    match kind {
        "float" => Some(ParamKind::Float),
        "bool" => Some(ParamKind::Bool),
        "long" => Some(ParamKind::Long),
        "event" => Some(ParamKind::Event),
        "point2D" => Some(ParamKind::Point2D),
        "color" => Some(ParamKind::Color),
        _ => None,
    }
}

/// Betöltött és lefordított ISF shader.
pub struct IsfShader {
    pub path: PathBuf,
    pub metadata: Metadata,
    pub params: Vec<Param>,
    /// Képi bemenetek nevei (TYPE: image).
    pub images: Vec<String>,
    pub uniform_size: u64,
    pub wgsl_fragment: String,
    pub skipped: Vec<String>,
}

impl IsfShader {
    pub fn load(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path)
            .with_context(|| format!("nem olvasható: {}", path.display()))?;
        Self::from_source(&source, path.to_path_buf())
    }

    pub fn from_source(source: &str, path: PathBuf) -> Result<Self> {
        let (metadata, body) = split_header(source)?;
        let (params, skipped) = build_params(&metadata)?;
        let images: Vec<String> = metadata
            .inputs
            .iter()
            .filter(|i| i.kind == "image")
            .map(|i| i.name.clone())
            .collect();
        let uniform_size = uniform_layout_size(&params);
        let wgsl_fragment = translate(&body, &params, &images)?;
        Ok(Self {
            path,
            metadata,
            params,
            images,
            uniform_size,
            wgsl_fragment,
            skipped,
        })
    }

    /// A paraméterek aktuális értékei egy f32 pufferbe írva (uniform feltöltéshez).
    ///
    /// A `frame` = [frame_index, pass_index, opacity, alpha_mode]. Az alpha_mode
    /// 1.0 esetén a réteg alpha-ja a fedést adja (opacity), 0.0 esetén a shader
    /// saját alpha-ja érvényesül (kulcsozáshoz / átlátszáshoz).
    pub fn write_uniform_block(
        &self,
        buffer: &mut [u8],
        values: &[[f32; 4]],
        frame: [f32; 4],
        resolution: [f32; 2],
        time: f32,
        delta: f32,
    ) {
        let put = |buf: &mut [u8], off: usize, vals: &[f32]| {
            for (i, v) in vals.iter().enumerate() {
                let o = off + i * 4;
                if o + 4 <= buf.len() {
                    buf[o..o + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
        };
        // A fejléc: 2 db vec4 (render info + frame info).
        put(buffer, 0, &[resolution[0], resolution[1], time, delta]);
        put(buffer, 16, &frame);
        for (i, p) in self.params.iter().enumerate() {
            let v = values.get(i).copied().unwrap_or(p.default);
            put(buffer, p.offset, &v[..p.kind.components()]);
        }
    }
}

fn uniform_layout_size(params: &[Param]) -> u64 {
    // A fejléc 2 vec4, utána a paraméterek, a végén 16-ra kerekítve.
    match params.last() {
        Some(p) => ((p.offset + p.kind.size() + 15) / 16 * 16) as u64,
        None => 32,
    }
}

pub fn split_header(source: &str) -> Result<(Metadata, String)> {
    let start = source
        .find("/*")
        .ok_or_else(|| anyhow!("nincs ISF fejléc (/* ... */)"))?;
    let end = source[start..]
        .find("*/")
        .map(|i| start + i)
        .ok_or_else(|| anyhow!("a fejléc nincs lezárva (*/)"))?;
    let json = &source[start + 2..end];
    let metadata: Metadata = serde_json::from_str(json)
        .with_context(|| format!("hibás ISF JSON fejléc: {}", &json[..json.len().min(120)]))?;
    let body = source[end + 2..].to_string();
    Ok((metadata, body))
}

fn build_params(metadata: &Metadata) -> Result<(Vec<Param>, Vec<String>)> {
    let mut params = Vec::new();
    let mut skipped = Vec::new();
    // A fejléc két vec4-je (render info + frame info) után.
    let mut offset = 32usize;
    for input in &metadata.inputs {
        let Some(kind) = param_kind(&input.kind) else {
            skipped.push(format!("{} ({})", input.name, input.kind));
            continue;
        };
        offset = (offset + kind.alignment() - 1) / kind.alignment() * kind.alignment();
        let mut default = [0.0f32; 4];
        if let Some(v) = &input.default {
            match v {
                serde_json::Value::Number(n) => {
                    if let Some(f) = n.as_f64() {
                        default[0] = f as f32;
                    }
                }
                serde_json::Value::Bool(b) => default[0] = if *b { 1.0 } else { 0.0 },
                serde_json::Value::Array(a) => {
                    for (i, x) in a.iter().take(4).enumerate() {
                        default[i] = x.as_f64().unwrap_or(0.0) as f32;
                    }
                }
                _ => {}
            }
        }
        if default[3] == 0.0 && kind == ParamKind::Color {
            default[3] = 1.0;
        }
        let labels = input.labels.clone().unwrap_or_default();
        let values: Vec<f32> = input
            .values
            .as_ref()
            .map(|vs| vs.iter().map(|v| *v as f32).collect())
            .unwrap_or_else(|| {
                if kind == ParamKind::Long && !labels.is_empty() {
                    (0..labels.len()).map(|i| i as f32).collect()
                } else {
                    Vec::new()
                }
            });
        let min = input
            .min
            .map(|m| m as f32)
            .or_else(|| values.iter().copied().reduce(f32::min))
            .unwrap_or(0.0);
        let max = input
            .max
            .map(|m| m as f32)
            .or_else(|| values.iter().copied().reduce(f32::max))
            .unwrap_or(1.0);
        params.push(Param {
            name: input.name.clone(),
            kind,
            min,
            max,
            default,
            labels,
            values,
            offset,
        });
        offset += kind.size();
    }
    Ok((params, skipped))
}

/// GLSL-ben előforduló ISF-beépítettek cseréje.
fn replace_word(source: &str, from: &str, to: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < source.len() {
        if source[i..].starts_with(from) {
            let before_ok = i == 0 || !is_ident_byte(bytes[i - 1]);
            let after = i + from.len();
            let after_ok = after >= source.len() || !is_ident_byte(bytes[after]);
            if before_ok && after_ok {
                out.push_str(to);
                i = after;
                continue;
            }
        }
        let ch = source[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.'
}

/// Az ISF shader-törzs fordítása WGSL-re a naga segítségével.
pub fn translate(body: &str, params: &[Param], images: &[String]) -> Result<String> {
    let mut block = String::from("layout(std140, set = 0, binding = 0) uniform ISFParams {\n");
    block.push_str("    vec4 _isf_render;\n    vec4 _isf_frame;\n");
    for p in params {
        block.push_str(&format!("    {} {};\n", p.kind.glsl_type(), p.name));
    }
    block.push_str("};\n");

    let mut body = body.to_string();
    body = replace_word(&body, "gl_FragColor", "isf_FragColor");
    body = replace_word(&body, "RENDERSIZE.xy", "_isf_render.xy");
    body = replace_word(&body, "RENDERSIZE.x", "_isf_render.x");
    body = replace_word(&body, "RENDERSIZE.y", "_isf_render.y");
    body = replace_word(&body, "RENDERSIZE", "_isf_render.xy");
    body = replace_word(&body, "TIMEDELTA", "_isf_render.w");
    body = replace_word(&body, "TIME", "_isf_render.z");
    body = replace_word(&body, "FRAMEINDEX", "int(_isf_frame.x)");
    body = replace_word(&body, "PASSINDEX", "int(_isf_frame.y)");
    body = replace_word(
        &body,
        "gl_FragCoord.xy",
        "_isf_render.xy * isf_FragNormCoord",
    );
    body = replace_word(&body, "isf_FragCoord", "_isf_render.xy * isf_FragNormCoord");
    body = replace_word(&body, "texture2D", "texture");

    // Képi bemenetek: a naga nem támogatja a standalone samplert, ezért
    // stub-függvénnyé alakítjuk, és a generált WGSL-ben cseréljük valódi
    // textúra-mintavételre.
    let mut stubs = String::new();
    for (i, img) in images.iter().enumerate() {
        body = remove_sampler_declaration(&body, img);
        body = rewrite_texture_call(&body, img, &format!("isf_image_{i}"));
        stubs.push_str(&format!(
            "vec4 isf_image_{i}(vec2 isf_uv) {{ return vec4(0.0, 0.0, 0.0, 1.0); }}\n"
        ));
    }

    let full = format!(
        "#version 450\nlayout(location = 0) in vec2 isf_FragNormCoord;\nlayout(location = 0) out vec4 isf_FragColor;\n{block}\n{stubs}\n{body}\n"
    );

    let mut frontend = naga::front::glsl::Frontend::default();
    let options = naga::front::glsl::Options::from(naga::ShaderStage::Fragment);
    let module = frontend.parse(&options, &full).map_err(|e| {
        anyhow!(
            "GLSL fordítási hiba: {:?}",
            e.errors.iter().take(2).collect::<Vec<_>>()
        )
    })?;
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    let info = validator
        .validate(&module)
        .map_err(|e| anyhow!("validációs hiba: {e:?}"))?;
    if !module.entry_points.iter().any(|e| e.name == "main") {
        bail!("nincs main() a shaderben");
    }
    let mut wgsl =
        naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
            .map_err(|e| anyhow!("WGSL kiírás: {e:?}"))?;

    // A stub-ok cseréje igazi mintavételre, és a bindingek beillesztése.
    // A paraméterek a 0-ás bindingen vannak, a képek 1/2, 3/4, ... párokat kapnak.
    let mut globals = String::new();
    for i in 0..images.len() {
        let tex = 1 + i * 2;
        let samp = 2 + i * 2;
        globals.push_str(&format!(
            "@group(0) @binding({tex}) var isf_tex_{i}: texture_2d<f32>;\n@group(0) @binding({samp}) var isf_samp_{i}: sampler;\n"
        ));
        wgsl = replace_wgsl_function_named(
            &wgsl,
            &format!("isf_image_{i}"),
            &format!(
                "fn {{name}}(isf_uv: vec2<f32>) -> vec4<f32> {{\n    return textureSample(isf_tex_{i}, isf_samp_{i}, isf_uv);\n}}\n"
            ),
        );
    }
    if !globals.is_empty() {
        wgsl = format!("{globals}{wgsl}");
    }
    Ok(inject_layer_opacity(&wgsl))
}

/// A layer-alpha beillesztése a generált WGSL-be.
///
/// A naga kimenete kiszámítható alakú: a fragment main a végén
///   return FragmentOutput(<expr>);
/// formában tér vissza. Ezt cseréljük ki úgy, hogy az alpha:
///   * fedés módban (`_isf_frame.w` = 1.0) = `_isf_frame.z` (layer-opacity),
///     így 100%-on a réteg teljesen lefedi az alatta lévő képet;
///   * shader alpha módban (`_isf_frame.w` = 0.0) = a shader saját alpha-ja
///     szorozva az opacity-vel (kulcsozáshoz / átlátszáshoz).
///
/// Az értéket előbb egy `let`-be kötjük, hogy a shader kimenetét csak
/// egyszer értékeljük ki (a naga kimenete gyakran egy `let` változó, de
/// nem garantált).
fn inject_layer_opacity(wgsl: &str) -> String {
    let needle = "return FragmentOutput(";
    let Some(pos) = wgsl.rfind(needle) else {
        return wgsl.to_string();
    };
    let rest = &wgsl[pos + needle.len()..];
    let Some(end) = rest.find(");") else {
        return wgsl.to_string();
    };
    let expr = &rest[..end];
    let mut out = String::with_capacity(wgsl.len() + 160);
    out.push_str(&wgsl[..pos]);
    out.push_str(&format!(
        "let isf_layer_color = {expr};\n    \
         return FragmentOutput(vec4<f32>(isf_layer_color.rgb, \
         mix(isf_layer_color.a, 1.0, global._isf_frame.w) * global._isf_frame.z));"
    ));
    out.push_str(&rest[end + 2..]);
    out
}

/// A sampler-deklaráció eltávolítása (a naga nem támogatja a standalone samplert).
fn remove_sampler_declaration(body: &str, name: &str) -> String {
    let suffix = format!("{name};");
    body.lines()
        .filter(|line| {
            let t = line.trim();
            !(t.contains("sampler2D") && t.ends_with(&suffix))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// texture(name, uv) -> fuggveny(uv)
fn rewrite_texture_call(body: &str, name: &str, func: &str) -> String {
    let call = format!("{func}(");
    body.replace(&format!("texture({name},"), &call)
        .replace(&format!("texture({name} ,"), &call)
        .replace(&format!("texture( {name},"), &call)
}

/// Egy WGSL-függvény lecserélése a neve alapján (kapcsos zárójel-számlálással).
/// A függvény lecserélése; a tényleges (esetleg átnevezett) nevet is kezeli.
/// A `replacement` egy format-sablon, ahol {name} a tényleges név.
fn replace_wgsl_function_named(wgsl: &str, name: &str, replacement: &str) -> String {
    let needle = format!("fn {name}");
    let Some(start) = wgsl.find(&needle) else {
        return wgsl.to_string();
    };
    // A tényleges név: a 'fn ' után a '(' -ig.
    let name_start = start + 3;
    let Some(paren_rel) = wgsl[name_start..].find('(') else {
        return wgsl.to_string();
    };
    let actual = wgsl[name_start..name_start + paren_rel].trim();
    let Some(open_rel) = wgsl[start..].find('{') else {
        return wgsl.to_string();
    };
    let open = start + open_rel;
    let bytes = wgsl.as_bytes();
    let mut depth = 0i32;
    let mut end = None;
    for (i, b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else {
        return wgsl.to_string();
    };
    let body = replacement.replace("{name}", actual);
    format!("{}{}{}", &wgsl[..start], body, &wgsl[end..])
}

/// Az ISF kép-bemenetek száma (a bind group építéséhez).
pub fn image_bindings(images: &[String]) -> Vec<(String, u32, u32)> {
    images
        .iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), 1 + i as u32 * 2, 2 + i as u32 * 2))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_inputs_translate_to_texture_sampling() {
        let src = r#"/*{
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "amount", "TYPE": "float", "DEFAULT": 0.5 }
  ]
}*/
void main() {
    vec4 base = texture2D(inputImage, isf_FragNormCoord);
    gl_FragColor = mix(base, vec4(1.0), amount);
}
"#;
        let shader = IsfShader::from_source(src, PathBuf::from("teszt.fs")).expect("fordítás");
        assert_eq!(shader.images, vec!["inputImage".to_string()]);
        let w = &shader.wgsl_fragment;
        assert!(
            w.contains("textureSample(isf_tex_0, isf_samp_0"),
            "nincs mintavétel:\n{w}"
        );
        assert!(
            w.contains("@group(0) @binding(1) var isf_tex_0: texture_2d<f32>;"),
            "nincs textúra-binding:\n{w}"
        );
        assert!(
            w.contains("@group(0) @binding(2) var isf_samp_0: sampler;"),
            "nincs sampler-binding:\n{w}"
        );
        assert!(!w.contains("sampler2D"), "maradt sampler2D:\n{w}");
    }

    #[test]
    fn image_without_sampler_declaration_still_works() {
        // Néhány ISF shader csak a fejlécben jelzi a képet.
        let src = r#"/*{
  "INPUTS": [ { "NAME": "inputImage", "TYPE": "image" } ]
}*/
void main() {
    gl_FragColor = texture2D(inputImage, isf_FragNormCoord);
}
"#;
        let shader = IsfShader::from_source(src, PathBuf::from("teszt2.fs")).expect("fordítás");
        assert_eq!(shader.images.len(), 1);
        assert!(shader.wgsl_fragment.contains("textureSample(isf_tex_0"));
    }

    const GRADIENT: &str = r#"/*{
  "DESCRIPTION": "Teszt gradiens",
  "ISFVSN": "2",
  "INPUTS": [
    { "NAME": "speed", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.0, "MAX": 4.0 },
    { "NAME": "tint", "TYPE": "color", "DEFAULT": [1.0, 0.5, 0.2, 1.0] }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord;
    float w = 0.5 + 0.5 * sin(TIME * speed + uv.x * 6.2831);
    vec3 c = mix(vec3(0.1, 0.0, 0.3), tint.rgb, w);
    gl_FragColor = vec4(c * (0.4 + 0.6 * uv.y), 1.0);
}
"#;

    const BARS: &str = r#"/*{
  "DESCRIPTION": "Savok",
  "INPUTS": [
    { "NAME": "count", "TYPE": "float", "DEFAULT": 16.0, "MIN": 2.0, "MAX": 64.0 },
    { "NAME": "center", "TYPE": "point2D", "DEFAULT": [0.5, 0.5] },
    { "NAME": "hue", "TYPE": "color", "DEFAULT": [0.2, 0.8, 1.0, 1.0] }
  ]
}*/
void main() {
    vec2 uv = isf_FragNormCoord - center;
    float a = atan(uv.y, uv.x);
    float r = length(uv);
    float bars = step(0.5, fract(a / 3.14159 * count / 2.0));
    float glow = smoothstep(0.6, 0.0, r);
    gl_FragColor = vec4(hue.rgb * bars * glow, 1.0);
}
"#;

    #[test]
    fn parses_header_and_params() {
        let s = IsfShader::from_source(GRADIENT, PathBuf::from("test.isf")).unwrap();
        assert_eq!(s.metadata.description, "Teszt gradiens");
        assert_eq!(s.params.len(), 2);
        assert_eq!(s.params[0].name, "speed");
        assert_eq!(s.params[0].kind, ParamKind::Float);
        assert_eq!(s.params[0].offset, 32);
        assert_eq!(s.params[1].name, "tint");
        assert_eq!(s.params[1].kind, ParamKind::Color);
        assert_eq!(s.params[1].offset, 48);
        assert_eq!(s.uniform_size, 64);
    }

    #[test]
    fn translates_gradient_to_wgsl() {
        let s = IsfShader::from_source(GRADIENT, PathBuf::from("test.isf")).unwrap();
        assert!(
            s.wgsl_fragment.contains("isf_FragColor"),
            "{}",
            s.wgsl_fragment
        );
        assert!(s.wgsl_fragment.contains("speed"), "{}", s.wgsl_fragment);
        assert!(s.wgsl_fragment.contains("@fragment"), "{}", s.wgsl_fragment);
    }

    #[test]
    fn translates_bars_with_point_and_color() {
        let s = IsfShader::from_source(BARS, PathBuf::from("bars.isf")).unwrap();
        assert!(s.wgsl_fragment.contains("center"));
        assert_eq!(s.params.len(), 3);
    }

    #[test]
    fn uniform_block_offsets_are_packed_and_aligned() {
        let s = IsfShader::from_source(BARS, PathBuf::from("bars.isf")).unwrap();
        // fejlec 32, count(float) @32, center(vec2) @40, hue(vec4) @48
        assert_eq!(s.params[0].offset, 32);
        assert_eq!(s.params[1].offset, 40);
        assert_eq!(s.params[2].offset, 48);
        assert_eq!(s.uniform_size, 64);
    }

    #[test]
    fn layer_alpha_mode_is_uniform_controlled() {
        let s = IsfShader::from_source(GRADIENT, PathBuf::from("teszt.isf")).unwrap();
        let w = &s.wgsl_fragment;
        assert!(
            w.contains("_isf_frame.w"),
            "nincs alpha-mód uniform a generált WGSL-ben:\n{w}"
        );
        assert!(
            w.contains("mix(isf_layer_color.a, 1.0, global._isf_frame.w)"),
            "az alpha nincs a fedés/shader-alpha mód szerint keverve:\n{w}"
        );
        assert!(
            w.contains("* global._isf_frame.z"),
            "az opacity szorzás hiányzik:\n{w}"
        );
    }

    #[test]
    fn image_inputs_are_skipped_not_fatal() {
        let src = r#"/*{
  "INPUTS": [
    { "NAME": "inputImage", "TYPE": "image" },
    { "NAME": "k", "TYPE": "float", "DEFAULT": 1.0 }
  ]
}*/
void main() { gl_FragColor = vec4(k, 0.0, 0.0, 1.0); }
"#;
        let s = IsfShader::from_source(src, PathBuf::from("img.isf")).unwrap();
        assert_eq!(s.params.len(), 1);
        assert_eq!(s.skipped.len(), 1);
    }
}

#[cfg(test)]
mod wgsl_dump {

    #[test]
    fn dump_wgsl_shape() {
        let src = r#"/*{
  "INPUTS": [ { "NAME": "k", "TYPE": "float", "DEFAULT": 1.0 } ]
}*/
void main() { gl_FragColor = vec4(k, 0.5, 0.2, 1.0); }
"#;
        let s = crate::isf::IsfShader::from_source(src, std::path::PathBuf::from("t.isf")).unwrap();
        println!("--- WGSL ---\n{}", s.wgsl_fragment);
    }
}
