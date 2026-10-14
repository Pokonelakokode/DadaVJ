//! Kísérlet: a naga GLSL-frontendje mit bír el az ISF-stílusú GLSL-ből?

pub fn compile_glsl_fragment(source: &str) -> Result<String, String> {
    let mut frontend = naga::front::glsl::Frontend::default();
    let options = naga::front::glsl::Options::from(naga::ShaderStage::Fragment);
    let module = frontend.parse(&options, source).map_err(|e| format!("{e:?}"))?;
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    let info = validator
        .validate(&module)
        .map_err(|e| format!("validációs hiba: {e:?}"))?;
    let wgsl = naga::back::wgsl::write_string(&module, &info, naga::back::wgsl::WriterFlags::empty())
        .map_err(|e| format!("WGSL kiírás: {e:?}"))?;
    Ok(wgsl)
}

#[cfg(test)]
mod probe_tests {
#[test]
fn probe_glsl_versions() {
    // 1) Szigorú Vulkan-stílusú GLSL
    let a = r#"#version 450
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 fragColor;
layout(set = 0, binding = 0) uniform Params { float speed; vec4 tint; };
void main() { fragColor = vec4(uv, abs(sin(speed)), 1.0) * tint; }
"#;
    // 2) ISF-szeru: samplerek + sajat kimenet
    let b = r#"#version 450
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 fragColor;
layout(set = 0, binding = 0) uniform Params { float speed; vec2 resolution; };
layout(set = 1, binding = 0) uniform sampler2D inputImage;
void main() {
    vec2 p = uv * resolution;
    vec4 c = texture(inputImage, uv);
    fragColor = vec4(c.rgb * (0.5 + 0.5 * sin(speed + p.x * 0.01)), 1.0);
}
"#;
    // 3) Nyers ISF (WebGL stilus) — varhatoan nem megy
    let c = r#"void main() {
    vec2 uv = isf_FragNormCoord;
    gl_FragColor = vec4(uv, 0.5, 1.0);
}
"#;
    for (name, src) in [("vulkan-stilus", a), ("isf-szeru", b), ("nyers isf", c)] {
        match crate::glsl_probe::compile_glsl_fragment(src) {
            Ok(wgsl) => println!("=== {name}: OK ===\\n{wgsl}"),
            Err(e) => println!("=== {name}: HIBA ===\\n{}", &e[..e.len().min(400)]),
        }
    }
}

}
