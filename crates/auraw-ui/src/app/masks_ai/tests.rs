use super::ai_mask_source_proxy_edge;

#[test]
fn canonical_source_keeps_4k_for_three_by_two_but_caps_square_pixel_count() {
    assert_eq!(ai_mask_source_proxy_edge(6000, 4000), 4096);
    assert_eq!(ai_mask_source_proxy_edge(6000, 6000), 3464);
    assert_eq!(ai_mask_source_proxy_edge(3000, 3000), 3000);
}
