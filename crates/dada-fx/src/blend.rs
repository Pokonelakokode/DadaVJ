//! Blend módok -> wgpu állapot.

use dada_core::BlendMode;

/// A blend mód indexe a pipeline-tömbökben (4 variáns).
pub fn blend_index(mode: BlendMode) -> usize {
    match mode {
        BlendMode::Normal => 0,
        BlendMode::Add => 1,
        BlendMode::Multiply => 2,
        BlendMode::Screen => 3,
        _ => 0,
    }
}

/// Blend mód -> wgpu állapot.
///
/// A Normal alpha-blendelést használ (a shader alpha = layer opacity),
/// az Add/Multiply/Screen fix-funkciós blendelést.
pub fn blend_state(mode: BlendMode) -> wgpu::BlendState {
    use wgpu::{BlendComponent, BlendFactor, BlendOperation};
    let over = BlendComponent::OVER;
    match mode {
        BlendMode::Normal => wgpu::BlendState::ALPHA_BLENDING,
        BlendMode::Add => wgpu::BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::SrcAlpha,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
            alpha: over,
        },
        BlendMode::Multiply => wgpu::BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::Dst,
                dst_factor: BlendFactor::Zero,
                operation: BlendOperation::Add,
            },
            alpha: over,
        },
        BlendMode::Screen => wgpu::BlendState {
            color: BlendComponent {
                src_factor: BlendFactor::OneMinusDst,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::Add,
            },
            alpha: over,
        },
        _ => wgpu::BlendState::ALPHA_BLENDING,
    }
}
