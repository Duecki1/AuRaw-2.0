#[derive(Clone, Copy, Debug)]
pub struct ExposureParams {
    pub black: f32,
    pub exposure: f32,
    pub hlcompr: f32,
    pub hlcomprthresh: f32,
    pub contrast: f32,
    pub middle_grey: f32,
    pub brightness: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub chroma_denoise: f32,
    pub ca_red: f32,
    pub ca_blue: f32,
    pub clip: f32,
    pub filmic_white: f32,
    pub filmic_black: f32,
}

impl Default for ExposureParams {
    fn default() -> Self {
        Self {
            black: 0.0,
            exposure: 0.0,
            hlcompr: 35.0,
            hlcomprthresh: 0.0,
            contrast: 0.0,
            middle_grey: 18.42,
            brightness: 0.0,
            saturation: 0.0,
            vibrance: 0.0,
            chroma_denoise: 0.0,
            ca_red: 0.0,
            ca_blue: 0.0,
            clip: 0.0,
            filmic_white: 4.0,
            filmic_black: -8.0,
        }
    }
}
