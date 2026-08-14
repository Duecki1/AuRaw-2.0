use super::gpu_preview_prewarm_cfa_kind;
use crate::pipeline::CfaKind;

#[test]
fn preview_prewarm_uses_the_bayer_template_explicitly() {
    assert_eq!(gpu_preview_prewarm_cfa_kind(), CfaKind::Bayer);
}
