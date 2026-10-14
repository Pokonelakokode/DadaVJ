// Spike A — procedurális teszt-shader, réteg-instance támogatással.
//
// A layer-index (@builtin(instance_index)) szándékosan megváltoztatja a képet,
// így a driver nem tudja a draw-kat összevonni: a mért GPU-idő valódi
// kompozitálási költséget jelent (alpha blendeléssel).

struct Uniforms {
    resolution : vec2<f32>,
    time : f32,
    beat : f32,
};

@group(0) @binding(0) var<uniform> u : Uniforms;

fn hsv2rgb(c : vec3<f32>) -> vec3<f32> {
    let k = vec4<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0, 3.0);
    let p = abs(fract(vec3<f32>(c.x) + k.xyz) * 6.0 - vec3<f32>(3.0));
    return c.z * mix(vec3<f32>(1.0), clamp(p - vec3<f32>(1.0), vec3<f32>(0.0), vec3<f32>(1.0)), c.y);
}

// Szintetikus "spektrum": addig, amíg valódi FFT nincs.
fn spectrum(x : f32, t : f32) -> f32 {
    let i = floor(x * 64.0);
    let f = i / 64.0;
    let a = sin(f * 41.0 + t * 2.1) * 0.5 + 0.5;
    let b = sin(f * 13.0 - t * 1.3) * 0.5 + 0.5;
    let c = sin(f * 7.0 + t * 0.7) * 0.5 + 0.5;
    let shape = pow(1.0 - f, 1.6) * 0.85 + 0.1;
    return clamp((a * 0.5 + b * 0.3 + c * 0.2) * shape, 0.0, 1.0);
}

struct VOut {
    @builtin(position) pos : vec4<f32>,
    @location(0) @interpolate(flat) layer : u32,
};

// ---------------------------------------------------------------------------
// Videó-réteg: zero-copy MTLTexture (CVPixelBuffer/IOSurface) mintavételezése.
// ---------------------------------------------------------------------------

@group(0) @binding(0) var video_tex : texture_2d<f32>;
@group(0) @binding(1) var video_sampler : sampler;

struct VideoParams {
    opacity : f32,
    _pad0 : f32,
    _pad1 : f32,
    _pad2 : f32,
};
@group(0) @binding(2) var<uniform> video_params : VideoParams;

struct VOutVideo {
    @builtin(position) pos : vec4<f32>,
    @location(0) uv : vec2<f32>,
};

@vertex
fn vs_video(@builtin(vertex_index) vi : u32) -> VOutVideo {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0)
    );
    var o : VOutVideo;
    o.pos = vec4<f32>(p[vi], 0.0, 1.0);
    // A textúra Y-ja lefelé nő, az NDC Y-ja felfelé.
    o.uv = vec2<f32>((p[vi].x + 1.0) * 0.5, 1.0 - (p[vi].y + 1.0) * 0.5);
    return o;
}

@fragment
fn fs_video(in : VOutVideo) -> @location(0) vec4<f32> {
    let c = textureSample(video_tex, video_sampler, in.uv);
    // Az alpha hordozza a layer-opacity-t (a blend src=SrcAlpha-t használ).
    return vec4<f32>(c.rgb, video_params.opacity);
}

@vertex
fn vs_main(@builtin(vertex_index) vi : u32, @builtin(instance_index) inst : u32) -> VOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0)
    );
    var o : VOut;
    o.pos = vec4<f32>(p[vi], 0.0, 1.0);
    o.layer = inst;
    return o;
}

@fragment
fn fs_main(in : VOut) -> @location(0) vec4<f32> {
    let res = u.resolution;
    let uv0 = in.pos.xy / res;
    let l = f32(in.layer);
    let uv = vec2<f32>(fract(uv0.x + l * 0.013), uv0.y);
    var col = vec3<f32>(0.0);

    let d = distance(uv, vec2<f32>(0.5, 0.55));
    col += vec3<f32>(0.02, 0.03, 0.07) * (1.2 - d);

    let grid = max(
        smoothstep(0.98, 1.0, fract(uv.x * 32.0)),
        smoothstep(0.98, 1.0, fract(uv.y * 18.0))
    );
    col += vec3<f32>(0.05, 0.07, 0.12) * grid;

    let s = spectrum(uv.x, u.time + l * 0.7);
    let top = 1.0 - s * 0.72;
    if (uv.y > top) {
        let hue = fract(uv.x * 0.7 + u.time * 0.05 + l * 0.11);
        let c = hsv2rgb(vec3<f32>(hue, 0.8, 1.0));
        col += c * (0.35 + 0.65 * (uv.y - top) / max(1.0 - top, 0.001));
    }

    let phase = fract(u.beat);
    col += vec3<f32>(0.18, 0.28, 0.55) * pow(1.0 - phase, 8.0) * 0.9;

    let corner = min(min(uv0.x, 1.0 - uv0.x), min(uv0.y, 1.0 - uv0.y));
    if (corner < 0.004) {
        col = vec3<f32>(0.9, 0.25, 0.1);
    }

    let alpha = select(1.0, 0.6, in.layer > 0u);
    return vec4<f32>(col, alpha);
}

// ---------------------------------------------------------------------------
// Audio-vizual: gördülő spektrogram + hullámforma + spektrum-sávok.
// ---------------------------------------------------------------------------

struct AudioUniforms {
    resolution : vec4<f32>,
    info : vec4<f32>,
    bands : vec4<f32>,
    bands2 : vec4<f32>,
};

@group(0) @binding(0) var spectrogram_tex : texture_2d<f32>;
@group(0) @binding(1) var waveform_tex : texture_2d<f32>;
@group(0) @binding(2) var spectrum_tex : texture_2d<f32>;
@group(0) @binding(3) var<uniform> au : AudioUniforms;

struct VOutAudio {
    @builtin(position) pos : vec4<f32>,
    @location(0) uv : vec2<f32>,
};

@vertex
fn vs_audio(@builtin(vertex_index) vi : u32) -> VOutAudio {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0)
    );
    var o : VOutAudio;
    o.pos = vec4<f32>(p[vi], 0.0, 1.0);
    o.uv = vec2<f32>((p[vi].x + 1.0) * 0.5, 1.0 - (p[vi].y + 1.0) * 0.5);
    return o;
}

fn heat(v : f32) -> vec3<f32> {
    let c = clamp(v, 0.0, 1.0);
    let a = vec3<f32>(0.04, 0.02, 0.12);
    let b = vec3<f32>(0.40, 0.08, 0.55);
    let d = vec3<f32>(1.00, 0.50, 0.12);
    let e = vec3<f32>(1.00, 1.00, 0.85);
    if (c < 0.35) { return mix(a, b, c / 0.35); }
    if (c < 0.70) { return mix(b, d, (c - 0.35) / 0.35); }
    return mix(d, e, (c - 0.70) / 0.30);
}

@fragment
fn fs_audio(in : VOutAudio) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let write_col = au.info.x;

    // A textúra 256 (frekvencia) x 512 (idő): a legfrissebb sor a jobb szélen.
    let time_row = i32(write_col + uv.x * 512.0) % 512;
    let freq_col = i32((1.0 - uv.y) * 255.0);
    let spec = textureLoad(spectrogram_tex, vec2<i32>(freq_col, time_row), 0).r;

    var c = heat(spec * 1.2);

    // Hullámforma a felső sávban.
    if (uv.y < 0.24) {
        let wx = i32(uv.x * 1023.0);
        let w = textureLoad(waveform_tex, vec2<i32>(wx, 0), 0).r;
        let dist = abs(uv.y - 0.12 - w * 0.09);
        let line = smoothstep(0.007, 0.0, dist);
        c = mix(c * 0.30, vec3<f32>(0.55, 1.0, 0.85), line);
    }

    // Spektrum-sávok az alsó sávban.
    if (uv.y > 0.80) {
        let bin = i32(uv.x * 511.0);
        let s = textureLoad(spectrum_tex, vec2<i32>(bin, 0), 0).r;
        let h = (uv.y - 0.80) / 0.20;
        if (h < s) {
            c = mix(c, heat(0.35 + 0.65 * s), 0.9);
        }
    }

    // Ütés-villanás + sáv-alapú színezés.
    let onset = au.bands2.w;
    let bass = au.bands.y;
    c += vec3<f32>(0.30, 0.18, 0.05) * onset;
    c += vec3<f32>(0.05, 0.02, 0.10) * bass;

    // Vignetta.
    let d = length(uv - vec2<f32>(0.5, 0.5)) * 1.15;
    c *= 1.0 - clamp(d, 0.0, 1.0) * 0.28;

    // Az alpha = layer-opacity (a resolution.z-ben érkezik).
    return vec4<f32>(c, au.resolution.z);
}

