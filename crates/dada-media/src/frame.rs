//! Dekódolt frame absztrakció.

/// Egy dekódolt videó-frame, ami a GPU-n textúraként használható.
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    /// Zenei idő (a lejátszás pozíciója másodpercben).
    pub pts_secs: f64,
}
